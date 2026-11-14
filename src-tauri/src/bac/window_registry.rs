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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
        execute_locate, execute_read, CatalogEntry, LocateRequest, ReadRequest, SavedFileOutcome,
        Source,
    };
    use serde_json::json;
    use std::sync::Arc;

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
}
