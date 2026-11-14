//! Read catalog metadata without hydrating remote credentials or seeding storage.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use bac_host_contracts::workspace::{CatalogEntry, OperationError};
use std::{panic::AssertUnwindSafe, path::Path};

pub(super) fn read_local_catalog(dir: &Path) -> Result<Vec<CatalogEntry>, OperationError> {
    let guard = bat_app_storage::profile::profile_index_guard();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let has_regular_index = ["index.json", "index.json.bak"].iter().any(|name| {
            std::fs::symlink_metadata(dir.join(name))
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
        });
        if !has_regular_index {
            return Err(OperationError::WorkspaceUnavailable);
        }
        bat_app_storage::profile::read_catalog_unlocked(dir)
            .map_err(|_| OperationError::WorkspaceUnavailable)
    }))
    .map_err(|_| OperationError::Internal)?;
    drop(guard);
    let index = result?;
    Ok(index
        .profiles
        .into_iter()
        .map(|profile| CatalogEntry {
            id: profile.id,
            is_local: profile.kind == "local",
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn catalog_regular_file_backup_and_non_grammar_rules() {
        let dir = std::env::temp_dir().join(format!(
            "bac-catalog-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        assert!(matches!(
            read_local_catalog(&dir),
            Err(OperationError::WorkspaceUnavailable)
        ));
        fs::create_dir(dir.join("index.json")).unwrap();
        assert!(matches!(
            read_local_catalog(&dir),
            Err(OperationError::WorkspaceUnavailable)
        ));
        fs::remove_dir(dir.join("index.json")).unwrap();
        fs::write(dir.join("index.json"), b"{").unwrap();
        let index = json!({"profiles":[
            {"id":"local","name":"Local","type":"local","createdAt":0,"updatedAt":0},
            {"id":"bad/id","name":"Bad","type":"local","createdAt":0,"updatedAt":0},
            {"id":"remote","name":"Remote","type":"remote","remoteToken":"fixture-only","createdAt":0,"updatedAt":0}
        ],"activeProfileIds":[]});
        fs::write(
            dir.join("index.json.bak"),
            serde_json::to_vec(&index).unwrap(),
        )
        .unwrap();
        let profiles = read_local_catalog(&dir).unwrap();
        use bac_host_contracts::workspace::require_local_profile;
        assert!(require_local_profile(&profiles, "local").is_ok());
        assert!(matches!(
            require_local_profile(&profiles, "bad/id"),
            Err(OperationError::ProfileNotFound)
        ));
        assert!(matches!(
            require_local_profile(&profiles, "remote"),
            Err(OperationError::ProfileNotLocal)
        ));
        assert!(matches!(
            require_local_profile(&profiles, "missing"),
            Err(OperationError::ProfileNotFound)
        ));
        fs::remove_dir_all(dir).unwrap();
    }
}
