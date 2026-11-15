//! Read the already-loaded registry without loading, repairing or seeding it.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use bac_host_contracts::workspace::{
    live_target, prepare_append, verify_readback, AppendRequest, AppendResult, OperationError,
    RegistryWindowSnapshot, Source, StagedAppend, TxAbort,
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    panic::AssertUnwindSafe,
    sync::OnceLock,
};
use tauri::{AppHandle, Manager};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    Selected,
    Prepared,
    Written,
}

type Overlay<'a> = dyn FnMut(&mut Value) + 'a;
type Writer<'a> = dyn FnMut(
        &str,
        Value,
        &HashMap<String, (Option<String>, Value)>,
        &mut Vec<super::WindowEntry>,
    ) -> bool
    + 'a;

fn registry_lock_poisoned() -> OperationError {
    static DIAGNOSTIC: OnceLock<()> = OnceLock::new();
    DIAGNOSTIC.get_or_init(|| {
        let message = crate::connection_diagnostics::redact("BAC registry mutex poisoned");
        bat_error_log::log("warn", "bac.workspace.registry", &message);
    });
    OperationError::Internal
}

fn snapshot_entries(entries: &[super::WindowEntry]) -> Vec<RegistryWindowSnapshot> {
    entries
        .iter()
        .map(|entry| RegistryWindowSnapshot {
            id: entry.id.clone(),
            profile_id: entry.profile_id.clone(),
            detached_workspace_id: entry.detached_workspace_id.clone(),
            has_content: super::snapshot_has_content(&entry.snapshot),
            last_active_at: entry.last_active_at,
            workspace_document: super::workspace_value_from_snapshot(&entry.snapshot),
        })
        .collect()
}

pub(crate) fn observe(app: &AppHandle) -> Result<Vec<RegistryWindowSnapshot>, OperationError> {
    observe_state(&app.state::<super::WindowRegistryState>())
}

pub(super) fn observe_state(
    state: &super::WindowRegistryState,
) -> Result<Vec<RegistryWindowSnapshot>, OperationError> {
    let entries = state.entries.lock().map_err(|_| registry_lock_poisoned())?;
    std::panic::catch_unwind(AssertUnwindSafe(|| snapshot_entries(&entries)))
        .map_err(|_| OperationError::Internal)
}

pub(crate) fn append(
    app: &AppHandle,
    host_id: &str,
    request: &AppendRequest,
    persisted: &mut dyn FnMut(&mut Overlay<'_>) -> Result<StagedAppend, OperationError>,
) -> Result<StagedAppend, OperationError> {
    let state = app.state::<super::WindowRegistryState>();
    append_state(
        &state,
        host_id,
        request,
        persisted,
        &mut |window_id, value, runtime_meta, entries| {
            super::save_workspace_locked(app, window_id, value, runtime_meta, entries)
        },
    )
}

pub(super) fn append_state(
    state: &super::WindowRegistryState,
    host_id: &str,
    request: &AppendRequest,
    persisted: &mut dyn FnMut(&mut Overlay<'_>) -> Result<StagedAppend, OperationError>,
    writer: &mut Writer<'_>,
) -> Result<StagedAppend, OperationError> {
    append_state_at(state, host_id, request, persisted, writer, &mut |_| {})
}

fn append_state_at(
    state: &super::WindowRegistryState,
    host_id: &str,
    request: &AppendRequest,
    persisted: &mut dyn FnMut(&mut Overlay<'_>) -> Result<StagedAppend, OperationError>,
    writer: &mut Writer<'_>,
    point: &mut dyn FnMut(Point),
) -> Result<StagedAppend, OperationError> {
    let runtime_meta = state
        .runtime_meta
        .lock()
        .map_err(|_| registry_lock_poisoned())?;
    let mut entries = state.entries.lock().map_err(|_| registry_lock_poisoned())?;

    let target = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let snapshots = snapshot_entries(&entries);
        let target = live_target(&snapshots, &request.profile_id)?.cloned();
        point(Point::Selected);
        Ok::<_, OperationError>(target)
    }))
    .map_err(|_| OperationError::Internal)??;

    let Some(target) = target else {
        drop(entries);
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            persisted(&mut |doc| {
                super::apply_runtime_workspace_meta(doc, &request.profile_id, &runtime_meta);
            })
        }));
        drop(runtime_meta);
        return outcome.map_err(|_| OperationError::WriteFailed)?;
    };

    let id = target.id;
    let prospective = match std::panic::catch_unwind(AssertUnwindSafe(|| {
        let prospective = prepare_append(
            host_id,
            request,
            Source::Live,
            Some(id.clone()),
            &target.workspace_document,
            |doc| super::apply_runtime_workspace_meta(doc, &request.profile_id, &runtime_meta),
        )
        .map_err(Box::new)?;
        point(Point::Prepared);
        Ok::<_, Box<TxAbort>>(prospective)
    }))
    .map_err(|_| OperationError::Internal)?
    .map_err(|abort| *abort)
    {
        Ok(prospective) => prospective,
        Err(TxAbort::Noop(result)) => {
            return Ok(StagedAppend {
                result,
                document: None,
                profile_changed: false,
            });
        }
        Err(TxAbort::Refused(error)) => return Err(error),
    };

    let wrote = std::panic::catch_unwind(AssertUnwindSafe(|| {
        if !writer(&id, prospective.clone(), &runtime_meta, &mut entries) {
            return false;
        }
        point(Point::Written);
        true
    }))
    .map_err(|_| OperationError::WriteFailed)?;
    if !wrote {
        return Err(OperationError::WriteFailed);
    }

    let (revision, document) = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let entry = entries
            .iter()
            .find(|entry| entry.id == id)
            .ok_or(OperationError::WriteFailed)?;
        let readback = super::workspace_value_from_snapshot(&entry.snapshot);
        let revision = verify_readback(&readback, &prospective, &request.terminal.id)?;
        let document = serde_json::to_string_pretty(&readback)
            .map(Some)
            .map_err(|_| OperationError::WriteFailed)?;
        Ok::<_, OperationError>((revision, document))
    }))
    .map_err(|_| OperationError::WriteFailed)??;

    drop(entries);
    drop(runtime_meta);
    Ok(StagedAppend {
        result: AppendResult {
            host_id: host_id.to_owned(),
            profile_id: request.profile_id.clone(),
            source: Source::Live,
            window_id: Some(id),
            terminal_id: request.terminal.id.clone(),
            appended: true,
            revision,
        },
        document,
        profile_changed: false,
    })
}

pub(crate) fn live_window_ids_for_profile(app: &AppHandle, profile_id: &str) -> Vec<String> {
    let live = app
        .webview_windows()
        .keys()
        .cloned()
        .collect::<HashSet<_>>();
    live_window_ids_state(
        &app.state::<super::WindowRegistryState>(),
        &live,
        profile_id,
    )
}

pub(super) fn live_window_ids_state(
    state: &super::WindowRegistryState,
    live: &HashSet<String>,
    profile_id: &str,
) -> Vec<String> {
    let Ok(entries) = state.entries.lock() else {
        return Vec::new();
    };
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        entries
            .iter()
            .filter(|entry| {
                entry.profile_id == profile_id
                    && entry.detached_workspace_id.is_none()
                    && live.contains(&entry.id)
            })
            .map(|entry| entry.id.clone())
            .collect()
    }))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bac_host_contracts::workspace::{
        canonical_digest, execute_locate, execute_read, normalize_registration, parse_request,
        CatalogEntry, LocateRequest, ReadRequest, Request, SavedFileOutcome, Source,
    };
    use bat_app_storage::window_snapshot::{
        persist_entries_at, profile_windows, write_global_workspace_at, write_profile_snapshot_at,
    };
    use serde_json::json;
    use std::{
        collections::BTreeMap,
        fs,
        path::{Path, PathBuf},
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn observe_empty_registry_observes_no_windows_and_seeds_nothing() {
        let state = super::super::WindowRegistryState::default();
        assert!(observe_state(&state).unwrap().is_empty());
        assert!(state.entries.lock().unwrap().is_empty());
        assert!(state.runtime_meta.lock().unwrap().is_empty());
        assert!(state.fresh_windows.lock().unwrap().is_empty());
    }

    #[test]
    fn observe_ignores_remote_and_uncatalogued_and_rejects_oversized_window_id() {
        let state = super::super::WindowRegistryState::default();
        let make = |id: String, profile: &str, detached: Option<&str>| super::super::WindowEntry {
            id,
            profile_id: profile.into(),
            snapshot: super::super::snapshot_from_workspace_value(
                json!({"workspaces":[{"id":"w"}],"terminals":[]}),
            ),
            detached_workspace_id: detached.map(str::to_owned),
            detached_parent_window_id: None,
            last_active_at: 1,
        };
        state.entries.lock().unwrap().extend([
            make("x".repeat(1023), "remote", None),
            make("x".repeat(1023), "missing", None),
            make("x".repeat(1023), "local", Some("w")),
        ]);
        let observed = observe_state(&state).unwrap();
        let catalog = vec![
            CatalogEntry {
                id: "local".into(),
                is_local: true,
            },
            CatalogEntry {
                id: "remote".into(),
                is_local: false,
            },
        ];
        assert!(matches!(
            execute_read(
                "h",
                &ReadRequest {
                    profile_id: "local".into()
                },
                &catalog,
                &observed,
                || panic!("oversized detached entry must not fall back"),
                |_| None
            ),
            Err(OperationError::WorkspaceUnavailable)
        ));
        let mut entries = state.entries.lock().unwrap();
        entries.retain(|entry| entry.profile_id != "local");
        entries.push(make("detached".into(), "local", Some("w")));
        drop(entries);
        let observed = observe_state(&state).unwrap();
        let result = execute_read(
            "h",
            &ReadRequest {
                profile_id: "local".into(),
            },
            &catalog,
            &observed,
            || Ok(SavedFileOutcome::NoSavedWindows),
            |_| None,
        )
        .unwrap();
        assert_eq!(result.source, Source::Persisted);
        assert!(execute_locate(
            "h",
            &LocateRequest {
                session_id: "t".into()
            },
            &catalog,
            &observed,
            &[],
            vec![],
            |_| None
        )
        .is_ok());
    }

    #[test]
    fn poisoned_entries_lock_returns_internal_error_without_recovery() {
        let state = Arc::new(super::super::WindowRegistryState::default());
        let other = state.clone();
        let _ = std::thread::spawn(move || {
            let _guard = other.entries.lock().unwrap();
            panic!("inject poison");
        })
        .join();
        assert!(matches!(
            observe_state(&state),
            Err(OperationError::Internal)
        ));
        assert!(state.entries.is_poisoned());
    }

    const HOST: &str = "h_0123456789abcdef0123456789abcdef";
    const FIXED_LAST_ACTIVE_AT: i64 = 1_700_000_000_000;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bac-live-append-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_entry(
        id: &str,
        profile_id: &str,
        detached_workspace_id: Option<&str>,
        doc: Value,
    ) -> super::super::WindowEntry {
        super::super::WindowEntry {
            id: id.to_owned(),
            profile_id: profile_id.to_owned(),
            snapshot: super::super::snapshot_from_workspace_value(doc),
            detached_workspace_id: detached_workspace_id.map(str::to_owned),
            detached_parent_window_id: None,
            last_active_at: 100,
        }
    }

    fn parse_append_request(
        profile_id: &str,
        expected_revision: &str,
        terminal_id: &str,
        workspace_id: &str,
        title: &str,
    ) -> AppendRequest {
        let req = parse_request(
            "bac:workspace-add-terminal",
            &json!({
                "profileId": profile_id,
                "expectedRevision": expected_revision,
                "terminal": {
                    "id": terminal_id,
                    "workspaceId": workspace_id,
                    "title": title,
                    "type": "terminal",
                    "cwd": "/repo",
                    "agentPreset": "codex-agent"
                }
            }),
        )
        .unwrap();
        let Request::WorkspaceAddTerminal(append) = req else {
            panic!("expected append request");
        };
        append
    }

    fn write_live_files(
        dir: &Path,
        calls: &mut usize,
        window_id: &str,
        mut value: Value,
        runtime_meta: &HashMap<String, (Option<String>, Value)>,
        entries: &mut [super::super::WindowEntry],
    ) -> bool {
        *calls += 1;
        let Some(slot_index) = entries.iter().position(|entry| entry.id == window_id) else {
            return false;
        };
        let mut entry = entries[slot_index].clone();
        super::super::apply_runtime_workspace_meta(&mut value, &entry.profile_id, runtime_meta);
        entry.snapshot = super::super::snapshot_from_workspace_value(value);
        entry.last_active_at = FIXED_LAST_ACTIVE_AT;
        entries[slot_index] = entry.clone();
        persist_entries_at(&dir.join("windows.json"), entries);
        if window_id == "main" {
            write_global_workspace_at(&dir.join("workspace.json"), &entry.snapshot);
        }
        let windows = profile_windows(entries, &entry.profile_id);
        write_profile_snapshot_at(
            &dir.join("profiles")
                .join(format!("{}.json", entry.profile_id)),
            &entry.profile_id,
            "Test",
            &windows,
        );
        true
    }

    fn fixed_persisted_staged(profile_id: &str, terminal_id: &str) -> StagedAppend {
        StagedAppend {
            result: AppendResult {
                host_id: HOST.to_owned(),
                profile_id: profile_id.to_owned(),
                source: Source::Persisted,
                window_id: None,
                terminal_id: terminal_id.to_owned(),
                appended: true,
                revision: "sha256:1111111111111111111111111111111111111111111111111111111111111111"
                    .to_owned(),
            },
            document: Some("{\"workspaces\":[],\"terminals\":[]}".to_owned()),
            profile_changed: true,
        }
    }

    fn persisted_double<'a>(
        calls: &'a mut usize,
        staged: StagedAppend,
    ) -> impl FnMut(&mut Overlay<'_>) -> Result<StagedAppend, OperationError> + 'a {
        move |_overlay| {
            *calls += 1;
            Ok(staged.clone())
        }
    }

    fn collect_dir_files(root: &Path, current: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                collect_dir_files(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }

    #[test]
    fn desktop_append_live_entry_runtime_meta_equivalence_and_readback() {
        let dir_bac = temp_dir("equiv-bac");
        let dir_upstream = temp_dir("equiv-upstream");
        let initial_doc = json!({
            "workspaces": [{"id": "w1", "name": "Main"}],
            "activeWorkspaceId": "w1",
            "activeGroup": null,
            "terminals": [{
                "id": "t1",
                "workspaceId": "w1",
                "title": "Existing",
                "type": "terminal",
                "cwd": "/repo",
                "agentPreset": "codex-agent",
                "x-upstream": {"kept": true}
            }],
            "activeTerminalId": "t1"
        });
        let meta_override = json!({
            "sdkSessionId": "sess-live-1",
            "codexSandboxMode": "workspace-write",
            "effort": "high"
        });

        let state = super::super::WindowRegistryState::default();
        state
            .entries
            .lock()
            .unwrap()
            .push(make_entry("main", "p1", None, initial_doc.clone()));
        state.runtime_meta.lock().unwrap().insert(
            "t1".to_owned(),
            (Some("p1".to_owned()), meta_override.clone()),
        );

        let stored_doc =
            super::super::workspace_value_from_snapshot(&state.entries.lock().unwrap()[0].snapshot);
        let (current_revision, _) = canonical_digest(&stored_doc).unwrap();
        let request = parse_append_request("p1", &current_revision, "t2", "w1", "Appended");

        let mut writer_calls = 0usize;
        let mut persisted_calls = 0usize;
        let staged = append_state_at(
            &state,
            HOST,
            &request,
            &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t2")),
            &mut |window_id, value, runtime_meta, entries| {
                write_live_files(
                    &dir_bac,
                    &mut writer_calls,
                    window_id,
                    value,
                    runtime_meta,
                    entries,
                )
            },
            &mut |_| {},
        )
        .unwrap();

        assert_eq!(writer_calls, 1);
        assert_eq!(persisted_calls, 0);
        assert!(staged.result.appended);
        assert_eq!(staged.result.source, Source::Live);
        assert_eq!(staged.result.window_id.as_deref(), Some("main"));
        assert_eq!(staged.result.profile_id, "p1");
        assert_eq!(staged.result.terminal_id, "t2");
        assert!(!staged.profile_changed);

        let state2 = super::super::WindowRegistryState::default();
        state2
            .entries
            .lock()
            .unwrap()
            .push(make_entry("main", "p1", None, initial_doc));
        state2
            .runtime_meta
            .lock()
            .unwrap()
            .insert("t1".to_owned(), (Some("p1".to_owned()), meta_override));

        let mut upstream_doc = stored_doc;
        upstream_doc
            .get_mut("terminals")
            .and_then(Value::as_array_mut)
            .unwrap()
            .push(normalize_registration(&request.terminal));
        let mut upstream_writer_calls = 0usize;
        {
            let runtime_meta2 = state2.runtime_meta.lock().unwrap();
            let mut entries2 = state2.entries.lock().unwrap();
            assert!(write_live_files(
                &dir_upstream,
                &mut upstream_writer_calls,
                "main",
                upstream_doc,
                &runtime_meta2,
                &mut entries2,
            ));
        }
        assert_eq!(upstream_writer_calls, 1);

        let mut files_bac = BTreeMap::new();
        let mut files_upstream = BTreeMap::new();
        collect_dir_files(&dir_bac, &dir_bac, &mut files_bac);
        collect_dir_files(&dir_upstream, &dir_upstream, &mut files_upstream);
        assert_eq!(
            files_bac.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["profiles/p1.json", "windows.json", "workspace.json"]
        );
        assert_eq!(files_bac, files_upstream);

        let profile_json: Value = serde_json::from_slice(&files_bac["profiles/p1.json"]).unwrap();
        let t1_in_profile = &profile_json["windows"][0]["terminals"][0];
        assert_eq!(t1_in_profile["x-upstream"], json!({"kept": true}));
        assert_eq!(t1_in_profile["sdkSessionId"], json!("sess-live-1"));
        assert_eq!(
            t1_in_profile["agentParams"]["sandboxMode"],
            json!("workspace-write")
        );
        assert_eq!(t1_in_profile["agentParams"]["effortLevel"], json!("high"));

        let readback =
            super::super::workspace_value_from_snapshot(&state.entries.lock().unwrap()[0].snapshot);
        assert_eq!(
            staged.result.revision,
            canonical_digest(&readback).unwrap().0
        );
        assert_eq!(
            staged.document,
            Some(serde_json::to_string_pretty(&readback).unwrap())
        );

        fs::remove_dir_all(dir_bac).unwrap();
        fs::remove_dir_all(dir_upstream).unwrap();
    }

    #[test]
    fn desktop_append_live_noop_conflict_and_stale_revision_call_no_writer() {
        let existing_req = parse_append_request(
            "p1",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "t1",
            "w1",
            "Existing",
        );
        let initial_doc = json!({
            "workspaces": [{"id": "w1"}],
            "activeWorkspaceId": "w1",
            "activeGroup": null,
            "terminals": [normalize_registration(&existing_req.terminal)],
            "activeTerminalId": "t1"
        });
        let state = super::super::WindowRegistryState::default();
        state
            .entries
            .lock()
            .unwrap()
            .push(make_entry("main", "p1", None, initial_doc));
        let before_doc =
            super::super::workspace_value_from_snapshot(&state.entries.lock().unwrap()[0].snapshot);
        let (current_revision, _) = canonical_digest(&before_doc).unwrap();

        let mut writer_calls = 0usize;
        let mut persisted_calls = 0usize;

        let noop = append_state_at(
            &state,
            HOST,
            &existing_req,
            &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t1")),
            &mut |_, _, _, _| {
                writer_calls += 1;
                true
            },
            &mut |_| {},
        )
        .unwrap();
        assert!(!noop.result.appended);
        assert_eq!(noop.result.source, Source::Live);
        assert_eq!(noop.result.window_id.as_deref(), Some("main"));
        assert_eq!(noop.result.revision, current_revision);
        assert_eq!(noop.document, None);
        assert!(!noop.profile_changed);

        let conflict_req = parse_append_request("p1", &current_revision, "t1", "w1", "Different");
        assert_eq!(
            append_state_at(
                &state,
                HOST,
                &conflict_req,
                &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t1")),
                &mut |_, _, _, _| {
                    writer_calls += 1;
                    true
                },
                &mut |_| {},
            ),
            Err(OperationError::TerminalExists {
                terminal_id: "t1".to_owned(),
                current_revision: current_revision.clone(),
            })
        );

        let stale_req = parse_append_request(
            "p1",
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
            "t2",
            "w1",
            "New",
        );
        assert_eq!(
            append_state_at(
                &state,
                HOST,
                &stale_req,
                &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t2")),
                &mut |_, _, _, _| {
                    writer_calls += 1;
                    true
                },
                &mut |_| {},
            ),
            Err(OperationError::RevisionMismatch {
                current_revision: current_revision.clone(),
            })
        );

        let missing_ws_req =
            parse_append_request("p1", &current_revision, "t2", "w-missing", "New");
        assert_eq!(
            append_state_at(
                &state,
                HOST,
                &missing_ws_req,
                &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t2")),
                &mut |_, _, _, _| {
                    writer_calls += 1;
                    true
                },
                &mut |_| {},
            ),
            Err(OperationError::WorkspaceNotFound("w-missing".to_owned()))
        );

        assert_eq!(writer_calls, 0);
        assert_eq!(persisted_calls, 0);
        let entries = state.entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].last_active_at, 100);
        assert_eq!(
            super::super::workspace_value_from_snapshot(&entries[0].snapshot),
            before_doc
        );
    }

    #[test]
    fn desktop_append_oversized_window_id_detached_refuses_without_persisted_fallback() {
        let doc = json!({
            "workspaces": [{"id": "w1"}],
            "terminals": []
        });
        let state = super::super::WindowRegistryState::default();
        state.entries.lock().unwrap().extend([
            make_entry("main", "p1", None, doc.clone()),
            make_entry(&"w".repeat(1_023), "p1", Some("w1"), doc),
        ]);
        let stored_doc =
            super::super::workspace_value_from_snapshot(&state.entries.lock().unwrap()[0].snapshot);
        let (rev, _) = canonical_digest(&stored_doc).unwrap();
        let request = parse_append_request("p1", &rev, "t1", "w1", "Agent");

        let mut writer_calls = 0usize;
        let mut persisted_calls = 0usize;
        assert_eq!(
            append_state_at(
                &state,
                HOST,
                &request,
                &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t1")),
                &mut |_, _, _, _| {
                    writer_calls += 1;
                    true
                },
                &mut |_| {},
            ),
            Err(OperationError::WorkspaceUnavailable)
        );
        assert_eq!(writer_calls, 0);
        assert_eq!(persisted_calls, 0);
    }

    #[test]
    fn desktop_append_detached_only_falls_back_holding_runtime_meta() {
        let doc = json!({
            "workspaces": [{"id": "w1"}],
            "terminals": []
        });
        let request = parse_append_request(
            "p1",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "t2",
            "w1",
            "Agent",
        );
        let expected_staged = fixed_persisted_staged("p1", "t2");

        for detached_only in [true, false] {
            let state = super::super::WindowRegistryState::default();
            if detached_only {
                state.entries.lock().unwrap().push(make_entry(
                    "detached-1",
                    "p1",
                    Some("w1"),
                    doc.clone(),
                ));
            }
            state.runtime_meta.lock().unwrap().insert(
                "t1".to_owned(),
                (
                    Some("p1".to_owned()),
                    json!({
                        "sdkSessionId": "sess-persisted",
                        "codexSandboxMode": "read-only",
                        "effort": "low"
                    }),
                ),
            );

            let mut writer_calls = 0usize;
            let mut persisted_calls = 0usize;
            let outcome = append_state_at(
                &state,
                HOST,
                &request,
                &mut |overlay| {
                    persisted_calls += 1;
                    assert!(state.runtime_meta.try_lock().is_err());
                    assert!(state.entries.try_lock().is_ok());
                    let mut sample = json!({
                        "workspaces": [{"id": "w1"}],
                        "terminals": [{
                            "id": "t1",
                            "workspaceId": "w1",
                            "agentPreset": "codex-agent"
                        }]
                    });
                    overlay(&mut sample);
                    assert_eq!(
                        sample["terminals"][0]["sdkSessionId"],
                        json!("sess-persisted")
                    );
                    assert_eq!(
                        sample["terminals"][0]["agentParams"]["sandboxMode"],
                        json!("read-only")
                    );
                    assert_eq!(
                        sample["terminals"][0]["agentParams"]["effortLevel"],
                        json!("low")
                    );
                    Ok(expected_staged.clone())
                },
                &mut |_, _, _, _| {
                    writer_calls += 1;
                    true
                },
                &mut |_| {},
            )
            .unwrap();

            assert_eq!(outcome, expected_staged);
            assert_eq!(persisted_calls, 1);
            assert_eq!(writer_calls, 0);
            assert!(state.runtime_meta.try_lock().is_ok());
            assert!(state.entries.try_lock().is_ok());
        }
    }

    #[test]
    fn desktop_append_writer_failure_and_readback_mismatch_are_write_failed() {
        let doc = json!({
            "workspaces": [{"id": "w1"}],
            "terminals": []
        });
        let state = super::super::WindowRegistryState::default();
        state
            .entries
            .lock()
            .unwrap()
            .push(make_entry("main", "p1", None, doc));
        let stored_doc =
            super::super::workspace_value_from_snapshot(&state.entries.lock().unwrap()[0].snapshot);
        let (rev, _) = canonical_digest(&stored_doc).unwrap();
        let request = parse_append_request("p1", &rev, "t1", "w1", "Agent");

        let mut writer_calls = 0usize;
        let mut persisted_calls = 0usize;
        assert_eq!(
            append_state_at(
                &state,
                HOST,
                &request,
                &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t1")),
                &mut |_, _, _, _| {
                    writer_calls += 1;
                    false
                },
                &mut |_| {},
            ),
            Err(OperationError::WriteFailed)
        );
        assert_eq!(writer_calls, 1);
        assert_eq!(persisted_calls, 0);

        let mut mismatch_writer_calls = 0usize;
        assert_eq!(
            append_state_at(
                &state,
                HOST,
                &request,
                &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t1")),
                &mut |window_id, _prospective, _runtime_meta, entries| {
                    mismatch_writer_calls += 1;
                    let slot = entries
                        .iter()
                        .position(|entry| entry.id == window_id)
                        .unwrap();
                    entries[slot].snapshot =
                        super::super::snapshot_from_workspace_value(stored_doc.clone());
                    true
                },
                &mut |_| {},
            ),
            Err(OperationError::WriteFailed)
        );
        assert_eq!(mismatch_writer_calls, 1);
        assert_eq!(persisted_calls, 0);
    }

    #[test]
    fn unwind_panic_in_pure_step_does_not_poison_registry_mutexes() {
        let doc = json!({
            "workspaces": [{"id": "w1"}],
            "terminals": []
        });
        let make_state = || {
            let state = super::super::WindowRegistryState::default();
            state
                .entries
                .lock()
                .unwrap()
                .push(make_entry("main", "p1", None, doc.clone()));
            state
        };
        let state0 = make_state();
        let stored_doc = super::super::workspace_value_from_snapshot(
            &state0.entries.lock().unwrap()[0].snapshot,
        );
        let (rev, _) = canonical_digest(&stored_doc).unwrap();
        let request = parse_append_request("p1", &rev, "t1", "w1", "Agent");

        for target_point in [Point::Selected, Point::Prepared] {
            let state = make_state();
            let mut writer_calls = 0usize;
            let mut persisted_calls = 0usize;
            assert_eq!(
                append_state_at(
                    &state,
                    HOST,
                    &request,
                    &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t1")),
                    &mut |_, _, _, _| {
                        writer_calls += 1;
                        true
                    },
                    &mut |point| {
                        if point == target_point {
                            panic!("injected pure step panic");
                        }
                    },
                ),
                Err(OperationError::Internal)
            );
            assert_eq!(writer_calls, 0);
            assert_eq!(persisted_calls, 0);
            assert!(state.entries.lock().is_ok());
            assert!(state.runtime_meta.lock().is_ok());
        }

        {
            let state = make_state();
            let mut writer_calls = 0usize;
            let mut persisted_calls = 0usize;
            assert_eq!(
                append_state_at(
                    &state,
                    HOST,
                    &request,
                    &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t1")),
                    &mut |_, _, _, _| {
                        writer_calls += 1;
                        panic!("injected writer panic");
                    },
                    &mut |_| {},
                ),
                Err(OperationError::WriteFailed)
            );
            assert_eq!(writer_calls, 1);
            assert_eq!(persisted_calls, 0);
            assert!(state.entries.lock().is_ok());
            assert!(state.runtime_meta.lock().is_ok());
        }

        {
            let state = make_state();
            let mut writer_calls = 0usize;
            let mut persisted_calls = 0usize;
            assert_eq!(
                append_state_at(
                    &state,
                    HOST,
                    &request,
                    &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t1")),
                    &mut |window_id, prospective, _runtime_meta, entries| {
                        writer_calls += 1;
                        let slot = entries
                            .iter()
                            .position(|entry| entry.id == window_id)
                            .unwrap();
                        entries[slot].snapshot =
                            super::super::snapshot_from_workspace_value(prospective);
                        true
                    },
                    &mut |point| {
                        if point == Point::Written {
                            panic!("injected post-write panic");
                        }
                    },
                ),
                Err(OperationError::WriteFailed)
            );
            assert_eq!(writer_calls, 1);
            assert_eq!(persisted_calls, 0);
            assert!(state.entries.lock().is_ok());
            assert!(state.runtime_meta.lock().is_ok());
        }
    }

    #[test]
    fn poisoned_runtime_meta_or_entries_returns_internal_error() {
        let request = parse_append_request(
            "p1",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "t1",
            "w1",
            "Agent",
        );

        let state_meta = Arc::new(super::super::WindowRegistryState::default());
        let other_meta = state_meta.clone();
        let _ = std::thread::spawn(move || {
            let _guard = other_meta.runtime_meta.lock().unwrap();
            panic!("inject runtime_meta poison");
        })
        .join();
        let mut writer_calls = 0usize;
        let mut persisted_calls = 0usize;
        assert_eq!(
            append_state_at(
                &state_meta,
                HOST,
                &request,
                &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t1")),
                &mut |_, _, _, _| {
                    writer_calls += 1;
                    true
                },
                &mut |_| {},
            ),
            Err(OperationError::Internal)
        );
        assert_eq!(writer_calls, 0);
        assert_eq!(persisted_calls, 0);
        assert!(state_meta.runtime_meta.is_poisoned());

        let state_entries = Arc::new(super::super::WindowRegistryState::default());
        let other_entries = state_entries.clone();
        let _ = std::thread::spawn(move || {
            let _guard = other_entries.entries.lock().unwrap();
            panic!("inject entries poison");
        })
        .join();
        assert_eq!(
            append_state_at(
                &state_entries,
                HOST,
                &request,
                &mut persisted_double(&mut persisted_calls, fixed_persisted_staged("p1", "t1")),
                &mut |_, _, _, _| {
                    writer_calls += 1;
                    true
                },
                &mut |_| {},
            ),
            Err(OperationError::Internal)
        );
        assert_eq!(writer_calls, 0);
        assert_eq!(persisted_calls, 0);
        assert!(state_entries.entries.is_poisoned());
    }

    #[test]
    fn live_window_ids_skip_detached_and_closed_windows_and_survive_poison() {
        let doc = json!({
            "workspaces": [{"id": "w1"}],
            "terminals": []
        });
        let state = Arc::new(super::super::WindowRegistryState::default());
        state.entries.lock().unwrap().extend([
            make_entry("win-a", "p1", None, doc.clone()),
            make_entry("win-detached", "p1", Some("w1"), doc.clone()),
            make_entry("win-closed", "p1", None, doc.clone()),
            make_entry("win-other", "p2", None, doc.clone()),
            make_entry("win-b", "p1", None, doc),
        ]);
        let live = HashSet::from([
            "win-a".to_owned(),
            "win-detached".to_owned(),
            "win-other".to_owned(),
            "win-b".to_owned(),
        ]);
        assert_eq!(
            live_window_ids_state(&state, &live, "p1"),
            vec!["win-a".to_owned(), "win-b".to_owned()]
        );

        let other = state.clone();
        let _ = std::thread::spawn(move || {
            let _guard = other.entries.lock().unwrap();
            panic!("inject entries poison for live_window_ids_state");
        })
        .join();
        assert!(state.entries.is_poisoned());
        assert!(live_window_ids_state(&state, &live, "p1").is_empty());
    }
}
