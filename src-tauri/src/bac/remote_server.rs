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

fn read_saved_profile(
    dir: &Path,
    profile_id: &str,
) -> (
    Result<workspace::SavedFileOutcome, workspace::SavedReadError>,
    usize,
) {
    let path = bat_app_storage::profile::profile_path(dir, profile_id);
    workspace::read_bounded_saved_file_measured(&path, bat_app_storage::profile::migrate_snapshot)
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
        valid_id, OperationError, SavedProfile, SavedReadError, PROFILES_LIMIT,
        SAVED_TOTAL_MAX_BYTES,
    };
    let local: Vec<_> = catalog.iter().filter(|profile| profile.is_local).collect();
    if local.iter().any(|profile| !valid_id(&profile.id)) {
        return Err(OperationError::LocationUnavailable(vec![]));
    }
    if local.len() > PROFILES_LIMIT {
        return Err(OperationError::LocationTooLarge);
    }
    let _guard = LOCATE_MUTEX.lock().map_err(|_| OperationError::Internal)?;
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
}

fn collect_local_saved(
    dir: &Path,
    catalog: &[workspace::CatalogEntry],
) -> Result<(Vec<workspace::SavedProfile>, Vec<String>), workspace::OperationError> {
    collect_saved_with(catalog, |profile_id| read_saved_profile(dir, profile_id))
}

fn first_saved_document(windows: &[Value]) -> Option<Value> {
    bat_app_storage::profile::workspace_from_first_snapshot_window(
        &serde_json::json!({"windows":windows}),
    )
}

fn saved_document(window: &Value) -> Option<Value> {
    bat_app_storage::profile::workspace_from_first_snapshot_window(
        &serde_json::json!({"windows":[window]}),
    )
}

fn profiles_dir(ctx: &HostContext) -> Option<PathBuf> {
    ctx.data_dir_opt().map(|dir| dir.join("profiles"))
}

fn dispatch(
    ctx: &HostContext,
    host_id: &str,
    request: workspace::Request,
) -> Result<Value, workspace::OperationError> {
    use workspace::{execute_locate, execute_read, OperationError, Request};
    #[cfg(feature = "desktop")]
    let entries = crate::window_registry::bac_workspace::observe(ctx.app())?;
    #[cfg(not(feature = "desktop"))]
    let entries = Vec::new();
    let catalog_unavailable = || match &request {
        Request::WorkspaceRead(_) => OperationError::WorkspaceUnavailable,
        Request::SessionLocate(_) => OperationError::LocationUnavailable(vec![]),
    };
    let dir = profiles_dir(ctx).ok_or_else(catalog_unavailable)?;
    let catalog = catalog::read_local_catalog(&dir).map_err(|error| match error {
        OperationError::WorkspaceUnavailable => catalog_unavailable(),
        other => other,
    })?;
    let value = match request {
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

}
