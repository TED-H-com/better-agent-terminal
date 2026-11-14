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

use bac_host_contracts::workspace::{OperationError, RegistryWindowSnapshot};
use std::{panic::AssertUnwindSafe, sync::OnceLock};
use tauri::{AppHandle, Manager};

pub(crate) fn observe(app: &AppHandle) -> Result<Vec<RegistryWindowSnapshot>, OperationError> {
    observe_state(&app.state::<super::WindowRegistryState>())
}

pub(super) fn observe_state(
    state: &super::WindowRegistryState,
) -> Result<Vec<RegistryWindowSnapshot>, OperationError> {
    let entries = state.entries.lock().map_err(|_| {
        static DIAGNOSTIC: OnceLock<()> = OnceLock::new();
        DIAGNOSTIC.get_or_init(|| {
            let message =
                crate::connection_diagnostics::redact("BAC registry entries mutex poisoned");
            bat_error_log::log("warn", "bac.workspace.registry", &message);
        });
        OperationError::Internal
    })?;
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        entries
            .clone()
            .into_iter()
            .map(|entry| RegistryWindowSnapshot {
                id: entry.id,
                profile_id: entry.profile_id,
                detached_workspace_id: entry.detached_workspace_id,
                has_content: super::snapshot_has_content(&entry.snapshot),
                last_active_at: entry.last_active_at,
                workspace_document: super::workspace_value_from_snapshot(&entry.snapshot),
            })
            .collect()
    }))
    .map_err(|_| OperationError::Internal)
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
