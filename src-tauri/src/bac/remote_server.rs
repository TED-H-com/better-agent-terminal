//! BAC is intercepted before upstream profile routing or sidecar fallback.
//! Storage diagnostics use redacted stderr and the host error spool where installed,
//! once per reason. They never open a file below the host data directory.
use super::{ContextSink, HostContext, ProfileContexts, RemoteProtocol, SidecarState};
pub(super) use bac_host_contracts::wire::reply_id;
use bac_host_contracts::{
    identity::{Identity, Reason, StorageError},
    wire::{advertisement, Error},
};
#[derive(Default)]
pub(super) struct Connection(bac_host_contracts::wire::Connection);
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

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
            return Ok(self
                .0
                .invoke(protocol == RemoteProtocol::V2, channel, frame, || {
                    identity(ctx)
                }));
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
}
