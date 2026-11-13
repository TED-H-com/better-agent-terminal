//! BAC is intercepted before upstream profile routing or sidecar fallback.
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
        storage_error(
            ctx,
            &StorageError::unavailable(
                Reason::DataDirectoryUnavailable,
                std::path::Path::new("<unresolved-data-dir>"),
            ),
        )
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
    host.get(&path).map_err(|error| storage_error(ctx, &error))
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
fn storage_error(ctx: &HostContext, error: &StorageError) -> Error {
    static DIAGNOSTICS: OnceLock<StorageDiagnostics> = OnceLock::new();
    DIAGNOSTICS
        .get_or_init(Default::default)
        .log_once(error, |line| {
            log_storage_refusal(ctx, line);
        });
    error.error.clone()
}
fn log_storage_refusal(ctx: &HostContext, line: &str) {
    // log_tauri creates its parent directories. Do not let diagnosis create a
    // missing data directory or follow a refused data-directory symlink.
    let usable = ctx
        .data_dir_opt()
        .and_then(|p| std::fs::symlink_metadata(p).ok())
        .is_some_and(|m| {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                m.is_dir() && m.file_attributes() & 0x400 == 0
            }
            #[cfg(not(windows))]
            {
                m.is_dir() && !m.file_type().is_symlink()
            }
        });
    if usable {
        crate::commands::app::log_tauri(ctx, line);
    } else {
        // Existing headless error spool, plus the same redacted stderr fallback
        // used by log_tauri, which remains available when storage is absent.
        bat_error_log::log("warn", "bac.identity.refused", line);
        eprintln!("[tauri] {}", crate::connection_diagnostics::redact(line));
    }
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
        assert_eq!(identity(&ctx), Err(Error::StorageUnavailable));
        assert!(!path.exists());
    }
}
