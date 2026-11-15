//! BAC is intercepted before upstream profile routing or sidecar fallback.
//! Storage diagnostics use redacted stderr and the host error spool where installed,
//! once per reason. They never open a file below the host data directory.
use super::{ContextSink, HostContext, ProfileContexts, RemoteProtocol, SidecarState};
#[path = "catalog.rs"]
mod catalog;
pub(super) use bac_host_contracts::wire::reply_id;
use bac_host_contracts::workspace;
use bac_host_contracts::{
    identity::{Identity, Reason, StorageError},
    wire::{advertisement, Error},
};
#[derive(Default)]
pub(super) struct Connection(bac_host_contracts::wire::Connection);
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

// This gate bounds concurrent full-catalog scans; it never guards an upstream lock.
static LOCATE_MUTEX: Mutex<()> = Mutex::new(());

#[cfg(test)]
type SavedReadGate = (
    std::sync::mpsc::Sender<()>,
    Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>>,
);

#[cfg(test)]
fn saved_read_gates() -> &'static Mutex<HashMap<PathBuf, SavedReadGate>> {
    static GATES: OnceLock<Mutex<HashMap<PathBuf, SavedReadGate>>> = OnceLock::new();
    GATES.get_or_init(Default::default)
}

#[cfg(test)]
#[cfg_attr(feature = "desktop", allow(dead_code))]
pub(super) struct SavedReadGateGuard(PathBuf);

#[cfg(test)]
impl Drop for SavedReadGateGuard {
    fn drop(&mut self) {
        if let Ok(mut gates) = saved_read_gates().lock() {
            gates.remove(&self.0);
        }
    }
}

#[cfg(test)]
#[cfg_attr(feature = "desktop", allow(dead_code))]
pub(super) fn install_saved_read_gate(
    profiles_dir: &Path,
) -> (
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
    SavedReadGateGuard,
) {
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let key = profiles_dir.to_path_buf();
    if let Ok(mut gates) = saved_read_gates().lock() {
        gates.insert(
            key.clone(),
            (started_tx, Arc::new(Mutex::new(Some(release_rx)))),
        );
    }
    (started_rx, release_tx, SavedReadGateGuard(key))
}

fn read_saved_profile_for_platform(
    dir: &Path,
    profile_id: &str,
    windows: bool,
) -> (
    Result<workspace::SavedFileOutcome, workspace::SavedReadError>,
    usize,
) {
    if workspace::is_reserved_profile_id(profile_id, windows) {
        return (Err(workspace::SavedReadError::Unreadable), 0);
    }
    #[cfg(test)]
    {
        let gate = saved_read_gates()
            .lock()
            .ok()
            .and_then(|mut gates| gates.remove(dir));
        if let Some((started_tx, release_rx)) = gate {
            let _ = started_tx.send(());
            if let Ok(mut rx_guard) = release_rx.lock() {
                if let Some(rx) = rx_guard.take() {
                    let _ = rx.recv_timeout(std::time::Duration::from_secs(10));
                }
            }
        }
    }
    let path = bat_app_storage::profile::profile_path(dir, profile_id);
    workspace::read_bounded_saved_file_measured(&path, bat_app_storage::profile::migrate_snapshot)
}

fn read_saved_profile(
    dir: &Path,
    profile_id: &str,
) -> (
    Result<workspace::SavedFileOutcome, workspace::SavedReadError>,
    usize,
) {
    read_saved_profile_for_platform(dir, profile_id, cfg!(windows))
}

fn collect_saved_with(
    catalog: &[workspace::CatalogEntry],
    mut load: impl FnMut(
        &str,
    ) -> (
        Result<workspace::SavedFileOutcome, workspace::SavedReadError>,
        usize,
    ),
) -> Result<(Vec<workspace::SavedProfile>, Vec<String>), workspace::OperationError> {
    use workspace::{
        local_catalog_profiles, OperationError, SavedProfile, SavedReadError, SAVED_TOTAL_MAX_BYTES,
    };
    let local = local_catalog_profiles(catalog)?;
    let guard = LOCATE_MUTEX.lock().map_err(|_| OperationError::Internal)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut total = 0usize;
        let mut saved = Vec::new();
        let mut unavailable = Vec::new();
        for profile in local {
            let (outcome, bytes) = load(&profile.id);
            total = total
                .checked_add(bytes)
                .ok_or(OperationError::LocationTooLarge)?;
            if total > SAVED_TOTAL_MAX_BYTES {
                return Err(OperationError::LocationTooLarge);
            }
            match outcome {
                Ok(outcome) => saved.push(SavedProfile {
                    profile_id: profile.id.clone(),
                    outcome,
                }),
                Err(SavedReadError::TooLarge) => return Err(OperationError::LocationTooLarge),
                Err(SavedReadError::Unreadable) => unavailable.push(profile.id.clone()),
            }
        }
        Ok((saved, unavailable))
    }))
    .map_err(|_| OperationError::Internal)?;
    drop(guard);
    result
}

fn collect_local_saved(
    dir: &Path,
    catalog: &[workspace::CatalogEntry],
) -> Result<(Vec<workspace::SavedProfile>, Vec<String>), workspace::OperationError> {
    collect_saved_with(catalog, |profile_id| read_saved_profile(dir, profile_id))
}

use crate::commands::profile::bac_workspace::{
    append_persisted, emit_changed, first_saved_document, saved_document,
};

fn profiles_dir(ctx: &HostContext) -> Option<PathBuf> {
    ctx.data_dir_opt().map(|dir| dir.join("profiles"))
}

#[derive(Debug, PartialEq)]
enum AppendEvent {
    ProfileChanged,
    Window(String, Value),
    Broadcast(Value),
}

fn append_events(
    profile_id: &str,
    staged: &workspace::StagedAppend,
    live_windows: &[String],
) -> Vec<AppendEvent> {
    use serde_json::json;
    let Some(data) = staged.document.as_deref() else {
        return Vec::new();
    };
    if staged.profile_changed {
        let mut events = Vec::with_capacity(live_windows.len() + 2);
        events.push(AppendEvent::ProfileChanged);
        for id in live_windows {
            events.push(AppendEvent::Window(
                id.clone(),
                json!({ "windowId": id, "data": data }),
            ));
        }
        events.push(AppendEvent::Broadcast(
            json!({ "profileId": profile_id, "data": data }),
        ));
        events
    } else if let Some(window_id) = staged.result.window_id.as_deref() {
        let payload = json!({ "profileId": profile_id, "windowId": window_id, "data": data });
        vec![
            AppendEvent::Window(window_id.to_owned(), payload.clone()),
            AppendEvent::Broadcast(payload),
        ]
    } else {
        Vec::new()
    }
}

fn emit_append_events(ctx: &HostContext, profile_id: &str, staged: &workspace::StagedAppend) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        #[cfg(feature = "desktop")]
        let live_windows = if staged.profile_changed && staged.document.is_some() {
            crate::window_registry::bac_workspace::live_window_ids_for_profile(
                ctx.app(),
                profile_id,
            )
        } else {
            Vec::new()
        };
        #[cfg(not(feature = "desktop"))]
        let live_windows: Vec<String> = Vec::new();
        for event in append_events(profile_id, staged, &live_windows) {
            match event {
                AppendEvent::ProfileChanged => emit_changed(ctx),
                AppendEvent::Window(id, payload) => {
                    ctx.emit_to(&id, "workspace:reload", payload);
                }
                AppendEvent::Broadcast(payload) => {
                    if let Some(remote_state) = ctx.try_state::<super::RustRemoteServerState>() {
                        remote_state.broadcast_event("workspace:reload", &payload);
                    }
                }
            }
        }
    }));
}

fn dispatch_append(
    ctx: &HostContext,
    host_id: &str,
    request: &workspace::AppendRequest,
    windows: bool,
) -> Result<Value, workspace::OperationError> {
    use workspace::OperationError;
    if workspace::is_reserved_profile_id(&request.profile_id, windows) {
        return Err(OperationError::WorkspaceUnavailable);
    }
    let dir = profiles_dir(ctx).ok_or(OperationError::WorkspaceUnavailable)?;
    let catalog = catalog::read_local_catalog(&dir)?;
    workspace::require_local_profile(&catalog, &request.profile_id)?;
    #[cfg(feature = "desktop")]
    let staged = crate::window_registry::bac_workspace::append(
        ctx.app(),
        host_id,
        request,
        &mut |overlay| append_persisted(&dir, host_id, request, overlay),
    )?;
    #[cfg(not(feature = "desktop"))]
    let staged = append_persisted(&dir, host_id, request, &mut |_| {})?;
    emit_append_events(ctx, &request.profile_id, &staged);
    serde_json::to_value(&staged.result).map_err(|_| {
        if staged.result.appended {
            OperationError::WriteFailed
        } else {
            OperationError::Internal
        }
    })
}

fn dispatch(
    ctx: &HostContext,
    host_id: &str,
    request: workspace::Request,
) -> Result<Value, workspace::OperationError> {
    use workspace::{execute_locate, execute_read, OperationError, Request};
    if let Request::WorkspaceAddTerminal(request) = &request {
        return dispatch_append(ctx, host_id, request, cfg!(windows));
    }
    #[cfg(feature = "desktop")]
    let entries = crate::window_registry::bac_workspace::observe(ctx.app())?;
    #[cfg(not(feature = "desktop"))]
    let entries = Vec::new();
    let catalog_unavailable = || match &request {
        Request::WorkspaceRead(_) | Request::WorkspaceAddTerminal(_) => {
            OperationError::WorkspaceUnavailable
        }
        Request::SessionLocate(_) => OperationError::LocationUnavailable(vec![]),
    };
    let dir = profiles_dir(ctx).ok_or_else(catalog_unavailable)?;
    let catalog = catalog::read_local_catalog(&dir).map_err(|error| match error {
        OperationError::WorkspaceUnavailable => catalog_unavailable(),
        other => other,
    })?;
    let value = match request {
        // Routed to dispatch_append before the registry is observed.
        Request::WorkspaceAddTerminal(_) => return Err(OperationError::Internal),
        Request::WorkspaceRead(request) => {
            let result = execute_read(
                host_id,
                &request,
                &catalog,
                &entries,
                || read_saved_profile(&dir, &request.profile_id).0,
                first_saved_document,
            )?;
            serde_json::to_value(result)
        }
        Request::SessionLocate(request) => {
            let (saved, unavailable) = collect_local_saved(&dir, &catalog)?;
            let result = execute_locate(
                host_id,
                &request,
                &catalog,
                &entries,
                &saved,
                unavailable,
                saved_document,
            )?;
            serde_json::to_value(result)
        }
    };
    value.map_err(|_| OperationError::Internal)
}

pub(super) fn capabilities(mut original: Value, protocol: RemoteProtocol) -> Value {
    // Only the isolated baseline fixture process suppresses the hook. This
    // branch does not exist in production builds.
    #[cfg(test)]
    if std::env::var_os("BAC_TEST_BASELINE_AUTH").is_some() {
        return original;
    }
    if protocol == RemoteProtocol::V2 {
        original["bac"] = advertisement();
    }
    original
}

fn identity(ctx: &HostContext) -> Result<String, Error> {
    static HOSTS: OnceLock<Mutex<HashMap<PathBuf, Arc<Identity>>>> = OnceLock::new();
    let path = ctx.data_dir_opt().ok_or_else(|| {
        storage_error(&StorageError::unavailable(
            Reason::DataDirectoryUnavailable,
            std::path::Path::new("<unresolved-data-dir>"),
        ))
    })?;
    // Keys come only from host configuration, never from BAC request parameters.
    // Separate fixture hosts in one process must not share an identity.
    let host = HOSTS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| Error::Internal)?
        .entry(path.clone())
        .or_default()
        .clone();
    host.get(&path).map_err(|error| storage_error(&error))
}

impl Connection {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn invoke(
        &self,
        ctx: &HostContext,
        sidecar: &SidecarState,
        protocol: RemoteProtocol,
        channel: &str,
        frame: &Value,
        contexts: &ProfileContexts,
        sink: ContextSink,
    ) -> Result<Value, String> {
        if channel.starts_with("bac:") {
            return Ok(self.0.invoke(
                protocol == RemoteProtocol::V2,
                channel,
                frame,
                || identity(ctx),
                |host_id, request| dispatch(ctx, host_id, request),
            ));
        }
        super::invoke_with_profile_context(ctx, sidecar, protocol, channel, frame, contexts, sink)
    }
}

// The key is the fixed reason, not the path, connection, or host ID. The set is
// bounded by the enum and lives for the process; logging never changes replies.
#[derive(Default)]
struct StorageDiagnostics(Mutex<HashSet<Reason>>);
impl StorageDiagnostics {
    fn log_once(&self, error: &StorageError, log: impl FnOnce(&str)) {
        let first = self
            .0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(error.reason);
        if first {
            // Debug formatting escapes newline/control characters in paths.
            log(&format!(
                "[bac.identity] refusal reason={:?} path={:?}",
                error.reason, error.path
            ));
        }
    }
}
fn storage_error(error: &StorageError) -> Error {
    static DIAGNOSTICS: OnceLock<StorageDiagnostics> = OnceLock::new();
    DIAGNOSTICS
        .get_or_init(Default::default)
        .log_once(error, |line| {
            log_storage_refusal(line);
        });
    error.wire_error()
}
fn log_storage_refusal(line: &str) {
    bat_error_log::log("warn", "bac.identity.refused", line);
    eprintln!("[tauri] {}", crate::connection_diagnostics::redact(line));
}
#[cfg(test)]
mod bac_tests {
    use super::*;
    #[test]
    fn concurrent_refusals_log_once_per_reason_across_paths() {
        let diagnostics = Arc::new(StorageDiagnostics::default());
        let lines = Arc::new(Mutex::new(Vec::new()));
        let jobs: Vec<_> = (0..16)
            .map(|index| {
                let d = diagnostics.clone();
                let lines = lines.clone();
                std::thread::spawn(move || {
                    let failure = StorageError::unavailable(
                        Reason::IdentityOpen,
                        &PathBuf::from(format!("fixture-{index}/bac/host.json")),
                    );
                    d.log_once(&failure, |line| lines.lock().unwrap().push(line.to_owned()));
                })
            })
            .collect();
        for job in jobs {
            job.join().unwrap();
        }
        let other =
            StorageError::unavailable(Reason::DirectoryCreate, std::path::Path::new("fixture/bac"));
        diagnostics.log_once(&other, |line| lines.lock().unwrap().push(line.to_owned()));
        assert_eq!(lines.lock().unwrap().len(), 2);
    }
    #[test]
    fn diagnostic_is_one_line_reason_and_path_only() {
        let d = StorageDiagnostics::default();
        let failure = StorageError {
            error: Error::StorageCorrupt,
            reason: Reason::IdentityContents,
            path: PathBuf::from("fixture\npath/bac/host.json"),
        };
        let mut lines = Vec::new();
        d.log_once(&failure, |line| lines.push(line.to_owned()));
        assert_eq!(lines, ["[bac.identity] refusal reason=IdentityContents path=\"fixture\\npath/bac/host.json\""]);
        assert_eq!(
            failure.error.reply()["error"]["details"],
            serde_json::json!({})
        );
    }
    #[cfg(not(feature = "desktop"))]
    #[test]
    fn diagnosis_never_creates_missing_data_directory() {
        let path =
            std::env::temp_dir().join(format!("bac-missing-store-{:032x}", rand::random::<u128>()));
        assert!(!path.exists());
        let ctx = HostContext::from_headless(Arc::new(crate::host_context::HeadlessHost::new(
            Some(path.clone()),
            Arc::new(|_, _| {}),
        )));
        assert_eq!(
            identity(&ctx).unwrap_err().reply()["error"]["code"],
            "STORAGE_UNAVAILABLE"
        );
        assert!(!path.exists());
    }
    #[test]
    fn no_saved_windows_and_partial_unreadable_behavior() {
        use bac_host_contracts::workspace::{
            execute_locate, execute_read, CatalogEntry, LocateRequest, OperationError, ReadRequest,
            SavedFileOutcome,
        };
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };
        let dir = std::env::temp_dir().join(format!(
            "bac-snapshots-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let catalog = vec![CatalogEntry {
            id: "p".into(),
            is_local: true,
        }];
        let read = || {
            execute_read(
                "host",
                &ReadRequest {
                    profile_id: "p".into(),
                },
                &catalog,
                &[],
                || read_saved_profile(&dir, "p").0,
                first_saved_document,
            )
            .unwrap()
        };
        assert!(!read().append_target_eligible);
        fs::write(dir.join("p.json"), br#"{"version":2,"windows":[]}"#).unwrap();
        assert!(!read().append_target_eligible);
        let (saved, unavailable) = collect_local_saved(&dir, &catalog).unwrap();
        assert!(matches!(
            saved.first().map(|profile| &profile.outcome),
            Some(SavedFileOutcome::NoSavedWindows)
        ));
        let located = execute_locate(
            "host",
            &LocateRequest {
                session_id: "t".into(),
            },
            &catalog,
            &[],
            &saved,
            unavailable,
            saved_document,
        )
        .unwrap();
        assert!(located.locations.is_empty());
        fs::write(dir.join("p.json"), br#"{"version":1,"workspaces":[{"id":"w"}],"terminals":[{"id":"t","workspaceId":"w"}]}"#).unwrap();
        assert_eq!(read().document["workspaces"][0]["id"], "w");
        let mut catalog2 = catalog.clone();
        catalog2.push(CatalogEntry {
            id: "broken".into(),
            is_local: true,
        });
        fs::write(dir.join("broken.json"), b"{").unwrap();
        let (saved, unavailable) = collect_local_saved(&dir, &catalog2).unwrap();
        assert!(
            matches!(execute_locate("host", &LocateRequest { session_id: "t".into() }, &catalog2, &[], &saved, unavailable, saved_document), Err(OperationError::LocationUnavailable(ids)) if ids == ["broken"])
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_scan_enforces_profile_and_byte_bounds_before_unavailable() {
        use bac_host_contracts::workspace::{
            CatalogEntry, OperationError, SavedFileOutcome, SavedReadError, SAVED_FILE_MAX_BYTES,
        };
        let make = |n| {
            (0..n)
                .map(|i| CatalogEntry {
                    id: format!("p{i}"),
                    is_local: true,
                })
                .collect::<Vec<_>>()
        };
        assert!(matches!(
            collect_saved_with(&make(257), |_| panic!("must validate before reading")),
            Err(OperationError::LocationTooLarge)
        ));
        assert!(matches!(
            collect_saved_with(&make(1), |_| (
                Err(SavedReadError::TooLarge),
                SAVED_FILE_MAX_BYTES + 1
            )),
            Err(OperationError::LocationTooLarge)
        ));
        let mut index = 0;
        let result = collect_saved_with(&make(9), |_| {
            index += 1;
            if index == 1 {
                (Err(SavedReadError::Unreadable), SAVED_FILE_MAX_BYTES)
            } else {
                (Ok(SavedFileOutcome::NoSavedWindows), SAVED_FILE_MAX_BYTES)
            }
        });
        assert!(matches!(result, Err(OperationError::LocationTooLarge)));
        assert_eq!(index, 9);
        let invalid = vec![CatalogEntry {
            id: "invalid/id".into(),
            is_local: true,
        }];
        assert!(
            matches!(collect_saved_with(&invalid, |_| panic!("invalid catalog read disk")), Err(OperationError::LocationUnavailable(ids)) if ids.is_empty())
        );
    }

    #[test]
    fn crlf_pin_verifier_matches_lf_digests() {
        use sha2::{Digest, Sha256};
        const MESSAGE: &str = "Upstream workspace writer/overlay changed. Compare its call order, lock lifetime, normalization, persistence and emitted events with BAC composition; update the adapter and equivalence fixtures as needed, then explicitly update this source pin. Do not update the hash merely to make this test pass.";
        fn digest(source: &str, start: &str, end: &str) -> String {
            let normalized = source.replace("\r\n", "\n");
            let a = normalized.find(start).expect(MESSAGE);
            let b = a + normalized[a..].find(end).expect(MESSAGE);
            format!("{:x}", Sha256::digest(normalized[a..b].as_bytes()))
        }
        let cases = [
            (
                include_str!("../commands/profile.rs"),
                "pub fn profile_save_workspace_for_remote",
                "\n#[cfg(feature = \"desktop\")]",
                "a1dc58ae6d0a836a1427478b7c7baf5e721a79a7b9660a78319e63bcbfad2e8e",
            ),
            (
                include_str!("../window_registry.rs"),
                "pub fn with_runtime_workspace_meta",
                "\npub fn persist_agent_runtime_meta",
                "4bd606bbe8dc878db45e9b8b98d3a211635956b276ef2f33408aa9070c5a2924",
            ),
            (
                include_str!("../remote_server.rs"),
                "        \"workspace:save\" => {",
                "        \"pty:create\" => {",
                "7294fb109d3c7341ecd6dc437d1051cc0b6f2c233894d2bd15d25ec48f87c24e",
            ),
            (
                include_str!("../../crates/bat-app-storage/src/profile.rs"),
                "pub fn read_catalog_unlocked",
                "\npub fn read_index_at",
                "5b676a58ae4c25241f264a290f7a259209f4e88f2a1d49d20f3b436bcc29c0c7",
            ),
            (
                include_str!("../../crates/bat-app-storage/src/profile.rs"),
                "pub fn read_index_for_update_unlocked",
                "\n// bac-hook: catalog-without-hydration",
                "4038247a9d40eb84ff8b7386acf62ffd5226d105b25e47f74fa82faf519286b8",
            ),
        ];
        for (source, start, end, expected) in cases {
            assert_eq!(digest(source, start, end), expected, "{}", MESSAGE);
            assert_eq!(
                digest(
                    &source.replace("\r\n", "\n").replace('\n', "\r\n"),
                    start,
                    end
                ),
                expected,
                "{}",
                MESSAGE
            );
        }
    }

    #[cfg(not(feature = "desktop"))]
    #[test]
    fn dispatch_read_and_locate_saved_profiles_without_registry() {
        use bac_host_contracts::workspace::{LocateRequest, ReadRequest, Request};
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };
        let dir = std::env::temp_dir().join(format!(
            "bac-dispatch-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let profiles = dir.join("profiles");
        fs::create_dir_all(&profiles).unwrap();
        let index = serde_json::json!({"profiles":[
            {"id":"default","name":"Default","type":"local","createdAt":0,"updatedAt":0},
            {"id":"empty","name":"Empty","type":"local","createdAt":0,"updatedAt":0},
            {"id":"remote","name":"Remote","type":"remote","remoteToken":"fixture-only","createdAt":0,"updatedAt":0}
        ],"activeProfileIds":[]});
        fs::write(
            profiles.join("index.json"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        let snapshot = serde_json::json!({"version":2,"windows":[{"workspaces":[{"id":"w"}],"terminals":[{"id":"t","workspaceId":"w"}]}]});
        fs::write(
            profiles.join("default.json"),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        fs::write(
            profiles.join("empty.json"),
            br#"{"version":2,"windows":[]}"#,
        )
        .unwrap();
        let ctx = HostContext::from_headless(Arc::new(crate::host_context::HeadlessHost::new(
            Some(dir.clone()),
            Arc::new(|_, _| {}),
        )));
        let read = dispatch(
            &ctx,
            "fixture-host",
            Request::WorkspaceRead(ReadRequest {
                profile_id: "default".into(),
            }),
        )
        .unwrap();
        assert_eq!(read["source"], "persisted");
        assert_eq!(read["windowId"], Value::Null);
        assert_eq!(read["document"]["terminals"][0]["id"], "t");
        let located = dispatch(
            &ctx,
            "fixture-host",
            Request::SessionLocate(LocateRequest {
                session_id: "t".into(),
            }),
        )
        .unwrap();
        assert_eq!(located["disposition"], "located");
        assert_eq!(located["atomic"], false);
        assert_eq!(located["locations"][0]["profileRevision"], read["revision"]);
        assert_eq!(located["locations"][0]["windowId"], Value::Null);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn chinese_dotted_reserved_and_upstream_profile_ids_are_handled() {
        use bac_host_contracts::workspace::{
            execute_locate, execute_read, valid_profile_id, CatalogEntry, LocateRequest,
            OperationError, ReadRequest, SavedFileOutcome, SavedReadError,
        };
        use bat_app_storage::profile::{
            default_entry, profile_from_options, profile_path, unique_profile_id, ProfileIndex,
        };
        use std::{
            ffi::OsStr,
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };

        let mut index = ProfileIndex {
            profiles: vec![default_entry()],
            active_profile_ids: vec![],
            active_profile_id: None,
        };
        let cjk_42 = "\u{4e00}".repeat(42);
        let cjk_43 = "\u{4e00}".repeat(43);
        let names = [
            "ASCII Profile",
            "ASCII Profile",
            "my_name.v2",
            "\u{4e2d}\u{6587}",
            "\u{4e2d}\u{6587}",
            "\u{5de5}\u{4f5c}-\u{533a}",
            "team-\u{6d4b}\u{8bd5} 01",
            "\u{3042}-\u{4e2d}\u{6587}-\u{ac00}",
            "!!!",
            &cjk_42,
            &cjk_43,
        ];
        for name in names {
            let id = unique_profile_id(&index, name);
            if id.len() <= 128 {
                assert!(
                    valid_profile_id(&id),
                    "expected valid_profile_id for generated id {id:?}"
                );
            } else {
                assert!(
                    !valid_profile_id(&id),
                    "expected >128 byte id to fail: len={}",
                    id.len()
                );
            }
            index
                .profiles
                .push(profile_from_options(id, name.to_string(), None));
        }

        let dir = std::env::temp_dir().join(format!(
            "bac-profile-ids-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();

        let chinese = "\u{4e2d}\u{6587}";
        for id in [chinese, &cjk_42, ".", ".."] {
            let path = profile_path(&dir, id);
            assert_eq!(path.parent(), Some(dir.as_path()));
            let expected_name = format!("{id}.json");
            assert_eq!(path.file_name(), Some(OsStr::new(expected_name.as_str())));
            let snapshot = serde_json::json!({
                "version": 2,
                "windows": [{
                    "workspaces": [{"id": "w"}],
                    "terminals": [{"id": format!("a:b-{id}"), "workspaceId": "w"}]
                }]
            });
            fs::write(&path, serde_json::to_vec(&snapshot).unwrap()).unwrap();
        }

        let catalog = vec![
            CatalogEntry {
                id: chinese.into(),
                is_local: true,
            },
            CatalogEntry {
                id: cjk_42.clone(),
                is_local: true,
            },
            CatalogEntry {
                id: ".".into(),
                is_local: true,
            },
            CatalogEntry {
                id: "..".into(),
                is_local: true,
            },
        ];
        for id in [chinese, &cjk_42, ".", ".."] {
            let result = execute_read(
                "host",
                &ReadRequest {
                    profile_id: id.into(),
                },
                &catalog,
                &[],
                || read_saved_profile(&dir, id).0,
                first_saved_document,
            )
            .unwrap();
            assert_eq!(result.profile_id, id);
            assert_eq!(result.document["workspaces"][0]["id"], "w");
        }
        let (saved, unavailable) = collect_local_saved(&dir, &catalog).unwrap();
        assert!(unavailable.is_empty());
        let located = execute_locate(
            "host",
            &LocateRequest {
                session_id: format!("a:b-{chinese}"),
            },
            &catalog,
            &[],
            &saved,
            unavailable,
            saved_document,
        )
        .unwrap();
        assert_eq!(located.locations.len(), 1);
        assert_eq!(located.locations[0].profile_id, chinese);

        let colon_catalog = vec![CatalogEntry {
            id: "a:b".into(),
            is_local: true,
        }];
        assert!(matches!(
            collect_saved_with(&colon_catalog, |_| panic!("must reject invalid catalog id")),
            Err(OperationError::LocationUnavailable(ids)) if ids.is_empty()
        ));

        let con_snapshot =
            br#"{"version":2,"windows":[{"workspaces":[{"id":"w"}],"terminals":[]}]}"#;
        fs::write(dir.join("CON.json"), con_snapshot).unwrap();
        fs::write(dir.join("con.foo.json"), con_snapshot).unwrap();
        for reserved in ["CON", "con.foo", "lpt0", "com9"] {
            let (outcome, bytes) = read_saved_profile_for_platform(&dir, reserved, true);
            assert!(matches!(outcome, Err(SavedReadError::Unreadable)));
            assert_eq!(bytes, 0);
        }
        assert!(matches!(
            read_saved_profile_for_platform(&dir, "CON", false).0,
            Ok(SavedFileOutcome::Snapshot { .. })
        ));
        assert!(matches!(
            read_saved_profile_for_platform(&dir, "con.foo", false).0,
            Ok(SavedFileOutcome::Snapshot { .. })
        ));
        assert!(matches!(
            read_saved_profile_for_platform(&dir, "lpt0", false).0,
            Ok(SavedFileOutcome::NoSavedWindows)
        ));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn collect_saved_with_catches_panic_without_poisoning_locate_mutex() {
        use bac_host_contracts::workspace::{CatalogEntry, OperationError, SavedFileOutcome};
        let catalog = vec![CatalogEntry {
            id: "default".into(),
            is_local: true,
        }];
        let err = collect_saved_with(&catalog, |_| panic!("injected loader panic")).unwrap_err();
        assert_eq!(err, OperationError::Internal);
        assert!(!LOCATE_MUTEX.is_poisoned());
        let (saved, unavailable) =
            collect_saved_with(&catalog, |_| (Ok(SavedFileOutcome::NoSavedWindows), 0)).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].profile_id, "default");
        assert!(matches!(saved[0].outcome, SavedFileOutcome::NoSavedWindows));
        assert!(unavailable.is_empty());
    }

    #[test]
    fn duplicate_catalog_ids_read_saved_once_and_respect_first_entry() {
        use bac_host_contracts::workspace::{
            execute_locate, execute_read, CatalogEntry, Disposition, LocateRequest, OperationError,
            ReadRequest, SavedFileOutcome, SAVED_FILE_MAX_BYTES,
        };
        use std::collections::HashMap;

        let make_window = |session_id: &str| {
            serde_json::json!({
                "workspaces": [{"id": "w"}],
                "terminals": [{"id": session_id, "workspaceId": "w"}]
            })
        };
        let dup_window = make_window("t-dup");

        let once_catalog = vec![CatalogEntry {
            id: "dup".into(),
            is_local: true,
        }];
        let twice_catalog = vec![
            CatalogEntry {
                id: "dup".into(),
                is_local: true,
            },
            CatalogEntry {
                id: "dup".into(),
                is_local: true,
            },
        ];

        let mut once_reads = 0usize;
        let (once_saved, once_unavailable) = collect_saved_with(&once_catalog, |id| {
            assert_eq!(id, "dup");
            once_reads += 1;
            (
                Ok(SavedFileOutcome::Snapshot {
                    raw_bytes: 128,
                    windows: vec![dup_window.clone()],
                }),
                128,
            )
        })
        .unwrap();
        assert_eq!(once_reads, 1);

        let mut twice_reads = 0usize;
        let (twice_saved, twice_unavailable) = collect_saved_with(&twice_catalog, |id| {
            assert_eq!(id, "dup");
            twice_reads += 1;
            (
                Ok(SavedFileOutcome::Snapshot {
                    raw_bytes: 128,
                    windows: vec![dup_window.clone()],
                }),
                128,
            )
        })
        .unwrap();
        assert_eq!(twice_reads, 1);
        assert_eq!(twice_saved.len(), 1);

        let once_located = execute_locate(
            "host",
            &LocateRequest {
                session_id: "t-dup".into(),
            },
            &once_catalog,
            &[],
            &once_saved,
            once_unavailable,
            saved_document,
        )
        .unwrap();
        let twice_located = execute_locate(
            "host",
            &LocateRequest {
                session_id: "t-dup".into(),
            },
            &twice_catalog,
            &[],
            &twice_saved,
            twice_unavailable,
            saved_document,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&twice_located).unwrap(),
            serde_json::to_value(&once_located).unwrap()
        );
        assert_eq!(
            twice_located.location_revision,
            once_located.location_revision
        );

        let nine_dups = vec![
            CatalogEntry {
                id: "dup".into(),
                is_local: true,
            };
            9
        ];
        let mut max_reads = 0usize;
        let (max_saved, _) = collect_saved_with(&nine_dups, |_| {
            max_reads += 1;
            (Ok(SavedFileOutcome::NoSavedWindows), SAVED_FILE_MAX_BYTES)
        })
        .unwrap();
        assert_eq!(max_reads, 1);
        assert_eq!(max_saved.len(), 1);

        let remote_then_local = vec![
            CatalogEntry {
                id: "r".into(),
                is_local: false,
            },
            CatalogEntry {
                id: "r".into(),
                is_local: true,
            },
        ];
        let (rtl_saved, rtl_unavailable) =
            collect_saved_with(&remote_then_local, |_| panic!("remote-first must not read"))
                .unwrap();
        assert!(rtl_saved.is_empty());
        assert!(rtl_unavailable.is_empty());
        assert_eq!(
            execute_read(
                "host",
                &ReadRequest {
                    profile_id: "r".into()
                },
                &remote_then_local,
                &[],
                || panic!("remote-first must not read"),
                first_saved_document,
            )
            .unwrap_err(),
            OperationError::ProfileNotLocal
        );
        let rtl_locate = execute_locate(
            "host",
            &LocateRequest {
                session_id: "t-r".into(),
            },
            &remote_then_local,
            &[],
            &rtl_saved,
            rtl_unavailable,
            saved_document,
        )
        .unwrap();
        assert_eq!(rtl_locate.disposition, Disposition::Unregistered);
        assert!(rtl_locate.locations.is_empty());

        let local_then_remote = vec![
            CatalogEntry {
                id: "l".into(),
                is_local: true,
            },
            CatalogEntry {
                id: "l".into(),
                is_local: false,
            },
        ];
        let mut counts: HashMap<String, usize> = HashMap::new();
        let (ltr_saved, ltr_unavailable) = collect_saved_with(&local_then_remote, |id| {
            *counts.entry(id.to_owned()).or_default() += 1;
            (
                Ok(SavedFileOutcome::Snapshot {
                    raw_bytes: 64,
                    windows: vec![make_window("t-l")],
                }),
                64,
            )
        })
        .unwrap();
        assert_eq!(counts.get("l").copied(), Some(1));
        assert!(ltr_unavailable.is_empty());
        let ltr_read = execute_read(
            "host",
            &ReadRequest {
                profile_id: "l".into(),
            },
            &local_then_remote,
            &[],
            || {
                Ok(SavedFileOutcome::Snapshot {
                    raw_bytes: 64,
                    windows: vec![make_window("t-l")],
                })
            },
            first_saved_document,
        )
        .unwrap();
        let ltr_locate = execute_locate(
            "host",
            &LocateRequest {
                session_id: "t-l".into(),
            },
            &local_then_remote,
            &[],
            &ltr_saved,
            ltr_unavailable,
            saved_document,
        )
        .unwrap();
        assert_eq!(ltr_locate.disposition, Disposition::Located);
        assert_eq!(ltr_locate.locations.len(), 1);
        assert_eq!(ltr_locate.locations[0].profile_id, "l");
        assert_eq!(ltr_locate.locations[0].profile_revision, ltr_read.revision);
    }

    #[test]
    fn append_events_follow_the_append_target() {
        use bac_host_contracts::workspace::{AppendResult, Source, StagedAppend};
        use serde_json::json;

        let doc = "{\n  \"terminals\": []\n}";
        let staged = |source: Source,
                      window_id: Option<&str>,
                      appended: bool,
                      document: Option<&str>,
                      profile_changed: bool| StagedAppend {
            result: AppendResult {
                host_id: "fixture-host".into(),
                profile_id: "p1".into(),
                source,
                window_id: window_id.map(str::to_owned),
                terminal_id: "t2".into(),
                appended,
                revision: format!("sha256:{}", "0".repeat(64)),
            },
            document: document.map(str::to_owned),
            profile_changed,
        };

        assert_eq!(
            append_events(
                "p1",
                &staged(Source::Live, Some("main"), false, None, false),
                &["main".into()],
            ),
            Vec::<AppendEvent>::new()
        );
        assert_eq!(
            append_events(
                "p1",
                &staged(Source::Persisted, None, false, None, true),
                &["a".into()],
            ),
            Vec::<AppendEvent>::new()
        );

        let live_payload = json!({
            "profileId": "p1",
            "windowId": "main",
            "data": doc,
        });
        let live_events = append_events(
            "p1",
            &staged(Source::Live, Some("main"), true, Some(doc), false),
            &["other".into()],
        );
        assert_eq!(
            live_events,
            vec![
                AppendEvent::Window("main".into(), live_payload.clone()),
                AppendEvent::Broadcast(live_payload),
            ]
        );

        let saved_with_windows = append_events(
            "p1",
            &staged(Source::Persisted, None, true, Some(doc), true),
            &["a".into(), "b".into()],
        );
        assert_eq!(
            saved_with_windows,
            vec![
                AppendEvent::ProfileChanged,
                AppendEvent::Window("a".into(), json!({ "windowId": "a", "data": doc })),
                AppendEvent::Window("b".into(), json!({ "windowId": "b", "data": doc })),
                AppendEvent::Broadcast(json!({ "profileId": "p1", "data": doc })),
            ]
        );

        let saved_without_windows = append_events(
            "p1",
            &staged(Source::Persisted, None, true, Some(doc), true),
            &[],
        );
        assert_eq!(
            saved_without_windows,
            vec![
                AppendEvent::ProfileChanged,
                AppendEvent::Broadcast(json!({ "profileId": "p1", "data": doc })),
            ]
        );

        assert_eq!(
            append_events(
                "p1",
                &staged(Source::Persisted, None, true, Some(doc), false),
                &["a".into()],
            ),
            Vec::<AppendEvent>::new()
        );

        for event in live_events
            .iter()
            .chain(saved_with_windows.iter())
            .chain(saved_without_windows.iter())
        {
            if let AppendEvent::Window(_, payload) | AppendEvent::Broadcast(payload) = event {
                assert_eq!(payload.get("data"), Some(&Value::String(doc.to_owned())));
            }
        }
    }

    #[cfg(not(feature = "desktop"))]
    type CapturedEvents = Arc<Mutex<Vec<(String, Value)>>>;

    #[cfg(not(feature = "desktop"))]
    fn append_host(data_dir: Option<PathBuf>) -> (HostContext, CapturedEvents) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let mut host = crate::host_context::HeadlessHost::new(
            data_dir,
            Arc::new(move |topic, payload| {
                captured
                    .lock()
                    .unwrap()
                    .push((topic.into(), payload.clone()));
            }),
        );
        host.manage(crate::event_hub::RuntimeEventHubState::default());
        host.manage(crate::remote_server::RustRemoteServerState::default());
        (HostContext::from_headless(Arc::new(host)), events)
    }

    #[cfg(not(feature = "desktop"))]
    fn append_fixture_dir(tag: &str) -> PathBuf {
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };
        let dir = std::env::temp_dir().join(format!(
            "bac-dispatch-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let profiles = dir.join("profiles");
        fs::create_dir_all(&profiles).unwrap();
        let index = serde_json::json!({"profiles":[
            {"id":"default","name":"Default","type":"local","createdAt":0,"updatedAt":0},
            {"id":"CON","name":"Con","type":"local","createdAt":0,"updatedAt":0},
            {"id":"remote","name":"Remote","type":"remote","remoteToken":"fixture-only","createdAt":0,"updatedAt":0}
        ],"activeProfileIds":[]});
        fs::write(
            profiles.join("index.json"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        let snapshot = serde_json::json!({"version":2,"windows":[{"workspaces":[{"id":"w"}],"terminals":[{"id":"t","workspaceId":"w"}]}]});
        let snapshot_bytes = serde_json::to_vec(&snapshot).unwrap();
        fs::write(profiles.join("default.json"), &snapshot_bytes).unwrap();
        #[cfg(not(windows))]
        fs::write(profiles.join("CON.json"), &snapshot_bytes).unwrap();
        dir
    }

    #[cfg(not(feature = "desktop"))]
    fn read_profile_revision(ctx: &HostContext, profile_id: &str) -> (Value, String) {
        let read = dispatch(
            ctx,
            "fixture-host",
            workspace::Request::WorkspaceRead(workspace::ReadRequest {
                profile_id: profile_id.into(),
            }),
        )
        .unwrap();
        let revision = read["revision"].as_str().unwrap().to_owned();
        (read, revision)
    }

    #[cfg(not(feature = "desktop"))]
    fn append_request_with(
        profile_id: &str,
        expected_revision: &str,
        terminal_id: &str,
        workspace_id: &str,
        title: &str,
    ) -> workspace::AppendRequest {
        let parsed = workspace::parse_request(
            "bac:workspace-add-terminal",
            &serde_json::json!({
                "profileId": profile_id,
                "expectedRevision": expected_revision,
                "terminal": {
                    "id": terminal_id,
                    "workspaceId": workspace_id,
                    "title": title,
                    "type": "terminal",
                    "cwd": "/repo",
                    "agentPreset": "codex-agent-worktree",
                    "model": null,
                    "agentParams": null,
                    "worktreeBranch": null
                }
            }),
        )
        .expect("valid request");
        let workspace::Request::WorkspaceAddTerminal(request) = parsed else {
            panic!("expected WorkspaceAddTerminal");
        };
        request
    }

    #[cfg(not(feature = "desktop"))]
    fn append_request(
        profile_id: &str,
        expected_revision: &str,
        terminal_id: &str,
    ) -> workspace::AppendRequest {
        append_request_with(profile_id, expected_revision, terminal_id, "w", "Term 2")
    }

    #[cfg(not(feature = "desktop"))]
    fn profile_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let profiles = dir.join("profiles");
        let Ok(entries) = std::fs::read_dir(&profiles) else {
            return Vec::new();
        };
        let mut files: Vec<(String, Vec<u8>)> = entries
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                let bytes = std::fs::read(entry.path()).unwrap();
                (name, bytes)
            })
            .collect();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }

    #[cfg(not(feature = "desktop"))]
    #[test]
    fn dispatch_append_to_saved_profile_writes_once_and_emits_profile_changed() {
        use workspace::Request;

        let dir = append_fixture_dir("append-once");
        let (ctx, events) = append_host(Some(dir.clone()));
        let (_, rev_before) = read_profile_revision(&ctx, "default");
        let request = append_request("default", &rev_before, "t2");

        let reply = dispatch(&ctx, "fixture-host", Request::WorkspaceAddTerminal(request)).unwrap();
        let object = reply.as_object().expect("reply object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "appended",
                "hostId",
                "profileId",
                "revision",
                "source",
                "terminalId",
                "windowId",
            ]
        );
        assert_eq!(reply["hostId"], "fixture-host");
        assert_eq!(reply["profileId"], "default");
        assert_eq!(reply["source"], "persisted");
        assert_eq!(reply["windowId"], Value::Null);
        assert_eq!(reply["terminalId"], "t2");
        assert_eq!(reply["appended"], true);
        let rev_after = reply["revision"].as_str().expect("revision string");
        assert_ne!(rev_after, rev_before);

        let (read_after, read_rev) = read_profile_revision(&ctx, "default");
        assert_eq!(read_rev, rev_after);
        let terminal_ids: Vec<&str> = read_after["document"]["terminals"]
            .as_array()
            .expect("terminals array")
            .iter()
            .map(|term| term["id"].as_str().unwrap())
            .collect();
        assert_eq!(terminal_ids, vec!["t", "t2"]);

        let captured = events.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, "profile:changed");
        let changed_ids: Vec<&str> = captured[0].1["profiles"]
            .as_array()
            .expect("profiles array")
            .iter()
            .filter_map(|entry| entry["id"].as_str())
            .collect();
        assert!(
            changed_ids.contains(&"default"),
            "expected default in {changed_ids:?}"
        );
        drop(captured);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(feature = "desktop"))]
    #[test]
    fn dispatch_append_noop_and_refusals_write_nothing_and_emit_nothing() {
        use workspace::{OperationError, Request};

        let dir = append_fixture_dir("append-noop-refusal");
        let (ctx, events) = append_host(Some(dir.clone()));
        let (_, rev_initial) = read_profile_revision(&ctx, "default");
        let first = dispatch(
            &ctx,
            "fixture-host",
            Request::WorkspaceAddTerminal(append_request("default", &rev_initial, "t2")),
        )
        .unwrap();
        assert_eq!(first["appended"], true);
        let rev_current = first["revision"].as_str().unwrap().to_owned();
        assert_eq!(events.lock().unwrap().len(), 1);
        events.lock().unwrap().clear();
        let baseline = profile_files(&dir);

        let noop = dispatch(
            &ctx,
            "fixture-host",
            Request::WorkspaceAddTerminal(append_request("default", &rev_current, "t2")),
        )
        .unwrap();
        assert_eq!(noop["appended"], false);
        assert_eq!(noop["revision"], rev_current);
        assert_eq!(profile_files(&dir), baseline);
        assert!(events.lock().unwrap().is_empty());

        let conflict = append_request_with("default", &rev_current, "t2", "w", "Other Title");
        assert_eq!(
            dispatch(
                &ctx,
                "fixture-host",
                Request::WorkspaceAddTerminal(conflict)
            ),
            Err(OperationError::TerminalExists {
                terminal_id: "t2".into(),
                current_revision: rev_current.clone(),
            })
        );
        assert_eq!(profile_files(&dir), baseline);
        assert!(events.lock().unwrap().is_empty());

        let stale = append_request("default", &rev_initial, "t3");
        assert_eq!(
            dispatch(&ctx, "fixture-host", Request::WorkspaceAddTerminal(stale)),
            Err(OperationError::RevisionMismatch {
                current_revision: rev_current.clone(),
            })
        );
        assert_eq!(profile_files(&dir), baseline);
        assert!(events.lock().unwrap().is_empty());

        let unknown_ws = append_request_with("default", &rev_current, "t3", "missing-ws", "Term 2");
        assert_eq!(
            dispatch(
                &ctx,
                "fixture-host",
                Request::WorkspaceAddTerminal(unknown_ws)
            ),
            Err(OperationError::WorkspaceNotFound("missing-ws".into()))
        );
        assert_eq!(profile_files(&dir), baseline);
        assert!(events.lock().unwrap().is_empty());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(all(not(feature = "desktop"), not(windows)))]
    #[test]
    fn dispatch_append_checks_the_profile_before_any_write() {
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };
        use workspace::{OperationError, Request};

        let dir = append_fixture_dir("append-profile-checks");
        let (ctx, events) = append_host(Some(dir.clone()));
        let (_, con_rev) = read_profile_revision(&ctx, "CON");
        let con_request = append_request("CON", &con_rev, "t2");
        let initial_files = profile_files(&dir);

        assert_eq!(
            dispatch_append(&ctx, "fixture-host", &con_request, true),
            Err(OperationError::WorkspaceUnavailable)
        );
        assert_eq!(profile_files(&dir), initial_files);
        assert!(events.lock().unwrap().is_empty());

        let con_reply = dispatch_append(&ctx, "fixture-host", &con_request, false).unwrap();
        assert_eq!(con_reply["appended"], true);
        assert_eq!(con_reply["profileId"], "CON");
        assert_eq!(events.lock().unwrap().len(), 1);
        events.lock().unwrap().clear();
        let baseline = profile_files(&dir);

        let default_request = append_request("default", &con_rev, "t2");
        let (no_dir_ctx, no_dir_events) = append_host(None);
        assert_eq!(
            dispatch(
                &no_dir_ctx,
                "fixture-host",
                Request::WorkspaceAddTerminal(default_request.clone())
            ),
            Err(OperationError::WorkspaceUnavailable)
        );
        assert_eq!(profile_files(&dir), baseline);
        assert!(no_dir_events.lock().unwrap().is_empty());

        let empty_dir = std::env::temp_dir().join(format!(
            "bac-dispatch-noindex-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(empty_dir.join("profiles")).unwrap();
        let (empty_ctx, empty_events) = append_host(Some(empty_dir.clone()));
        assert_eq!(
            dispatch(
                &empty_ctx,
                "fixture-host",
                Request::WorkspaceAddTerminal(default_request)
            ),
            Err(OperationError::WorkspaceUnavailable)
        );
        assert!(profile_files(&empty_dir).is_empty());
        assert!(empty_events.lock().unwrap().is_empty());
        fs::remove_dir_all(empty_dir).unwrap();

        let unknown_request = append_request("missing", &con_rev, "t2");
        assert_eq!(
            dispatch(
                &ctx,
                "fixture-host",
                Request::WorkspaceAddTerminal(unknown_request)
            ),
            Err(OperationError::ProfileNotFound)
        );
        assert_eq!(profile_files(&dir), baseline);
        assert!(events.lock().unwrap().is_empty());

        let remote_request = append_request("remote", &con_rev, "t2");
        assert_eq!(
            dispatch(
                &ctx,
                "fixture-host",
                Request::WorkspaceAddTerminal(remote_request)
            ),
            Err(OperationError::ProfileNotLocal)
        );
        assert_eq!(profile_files(&dir), baseline);
        assert!(events.lock().unwrap().is_empty());

        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(not(feature = "desktop"))]
    #[test]
    fn dispatch_append_reply_survives_a_panicking_event_sink() {
        use workspace::Request;

        let dir = append_fixture_dir("append-panic-sink");
        let mut host = crate::host_context::HeadlessHost::new(
            Some(dir.clone()),
            Arc::new(|topic, _| panic!("sink panicked on {topic}")),
        );
        host.manage(crate::event_hub::RuntimeEventHubState::default());
        host.manage(crate::remote_server::RustRemoteServerState::default());
        let ctx = HostContext::from_headless(Arc::new(host));

        let (_, rev_before) = read_profile_revision(&ctx, "default");
        let reply = dispatch(
            &ctx,
            "fixture-host",
            Request::WorkspaceAddTerminal(append_request("default", &rev_before, "t2")),
        )
        .unwrap();
        assert_eq!(reply["appended"], true);
        let rev_after = reply["revision"].as_str().unwrap().to_owned();

        let saved_raw = std::fs::read(dir.join("profiles").join("default.json")).unwrap();
        let saved: Value = serde_json::from_slice(&saved_raw).unwrap();
        let saved_ids: Vec<&str> = saved["windows"][0]["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|term| term["id"].as_str().unwrap())
            .collect();
        assert_eq!(saved_ids, vec!["t", "t2"]);

        let (read_after, read_rev) = read_profile_revision(&ctx, "default");
        assert_eq!(read_rev, rev_after);
        assert_eq!(read_after["document"]["terminals"][1]["id"], "t2");

        let retry = dispatch(
            &ctx,
            "fixture-host",
            Request::WorkspaceAddTerminal(append_request("default", &rev_after, "t2")),
        )
        .unwrap();
        assert_eq!(retry["appended"], false);
        assert_eq!(retry["revision"], rev_after);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
