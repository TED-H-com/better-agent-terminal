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

use bac_host_contracts::workspace::{CatalogEntry, OperationError, CATALOG_FILE_MAX_BYTES};
use std::{io, panic::AssertUnwindSafe, path::Path};

fn catalog_file_is_acceptable(path: &Path) -> Result<bool, OperationError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.len() > CATALOG_FILE_MAX_BYTES as u64 {
                Err(OperationError::WorkspaceUnavailable)
            } else {
                Ok(true)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(OperationError::WorkspaceUnavailable),
    }
}

pub(super) fn read_local_catalog(dir: &Path) -> Result<Vec<CatalogEntry>, OperationError> {
    let guard = bat_app_storage::profile::profile_index_guard();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let mut has_regular_index = false;
        for name in ["index.json", "index.json.bak"] {
            if catalog_file_is_acceptable(&dir.join(name))? {
                has_regular_index = true;
            }
        }
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

    fn temp_catalog_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "bac-catalog-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_index_bytes() -> Vec<u8> {
        serde_json::to_vec(&json!({"profiles":[
            {"id":"default","name":"Default","type":"local","createdAt":0,"updatedAt":0},
            {"id":"remote","name":"Remote","type":"remote","remoteToken":"fixture-only","createdAt":0,"updatedAt":0}
        ],"activeProfileIds":[]}))
        .unwrap()
    }

    #[cfg(unix)]
    fn read_catalog_with_fifo_deadline(
        dir: &Path,
        fifo: &Path,
    ) -> Result<Vec<CatalogEntry>, OperationError> {
        use std::{os::unix::fs::OpenOptionsExt, sync::mpsc, time::Duration};
        let (tx, rx) = mpsc::channel();
        let dir_owned = dir.to_path_buf();
        let handle = std::thread::spawn(move || {
            let _ = tx.send(read_local_catalog(&dir_owned));
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => {
                handle.join().unwrap();
                result
            }
            Err(error) => {
                let _ = fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(fifo);
                let _ = handle.join();
                panic!("read_local_catalog did not return within 5s: {error}");
            }
        }
    }

    #[test]
    fn catalog_accepts_primary_only_backup_only_and_both_regular() {
        let valid = sample_index_bytes();

        let empty = temp_catalog_dir("empty");
        assert!(matches!(
            read_local_catalog(&empty),
            Err(OperationError::WorkspaceUnavailable)
        ));
        fs::remove_dir_all(empty).unwrap();

        let primary_only = temp_catalog_dir("primary-only");
        fs::write(primary_only.join("index.json"), &valid).unwrap();
        let profiles = read_local_catalog(&primary_only).unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].id, "default");
        assert!(profiles[0].is_local);
        assert_eq!(profiles[1].id, "remote");
        assert!(!profiles[1].is_local);
        fs::remove_dir_all(primary_only).unwrap();

        let backup_only = temp_catalog_dir("backup-only");
        fs::write(backup_only.join("index.json.bak"), &valid).unwrap();
        let profiles = read_local_catalog(&backup_only).unwrap();
        assert_eq!(profiles.len(), 2);
        fs::remove_dir_all(backup_only).unwrap();

        let both = temp_catalog_dir("both");
        fs::write(both.join("index.json"), &valid).unwrap();
        fs::write(both.join("index.json.bak"), &valid).unwrap();
        let profiles = read_local_catalog(&both).unwrap();
        assert_eq!(profiles.len(), 2);
        fs::remove_dir_all(both).unwrap();

        let max_exact = temp_catalog_dir("max-exact");
        let mut padded = vec![b' '; CATALOG_FILE_MAX_BYTES - valid.len()];
        padded.extend_from_slice(&valid);
        assert_eq!(padded.len(), CATALOG_FILE_MAX_BYTES);
        fs::write(max_exact.join("index.json"), &padded).unwrap();
        let profiles = read_local_catalog(&max_exact).unwrap();
        assert_eq!(profiles.len(), 2);
        fs::remove_dir_all(max_exact).unwrap();
    }

    #[test]
    fn catalog_rejects_non_regular_and_oversized_entries_for_each_file() {
        let valid = sample_index_bytes();
        for (target, peer) in [
            ("index.json", "index.json.bak"),
            ("index.json.bak", "index.json"),
        ] {
            let dir = temp_catalog_dir("dir-target");
            fs::write(dir.join(peer), &valid).unwrap();
            fs::create_dir(dir.join(target)).unwrap();
            assert!(matches!(
                read_local_catalog(&dir),
                Err(OperationError::WorkspaceUnavailable)
            ));
            fs::remove_dir_all(dir).unwrap();

            let dir = temp_catalog_dir("sparse-target");
            fs::write(dir.join(peer), &valid).unwrap();
            fs::File::create(dir.join(target))
                .unwrap()
                .set_len(CATALOG_FILE_MAX_BYTES as u64 + 1)
                .unwrap();
            assert!(matches!(
                read_local_catalog(&dir),
                Err(OperationError::WorkspaceUnavailable)
            ));
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn catalog_rejects_symlinks_devices_fifos_and_metadata_errors_for_each_file() {
        use std::{
            ffi::CString,
            os::unix::{
                ffi::OsStrExt,
                fs::{symlink, PermissionsExt},
            },
        };
        let valid = sample_index_bytes();
        for (target, peer) in [
            ("index.json", "index.json.bak"),
            ("index.json.bak", "index.json"),
        ] {
            for with_peer in [true, false] {
                let dir = temp_catalog_dir("fifo");
                if with_peer {
                    fs::write(dir.join(peer), &valid).unwrap();
                }
                let fifo_path = dir.join(target);
                let fifo = CString::new(fifo_path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
                assert!(matches!(
                    read_catalog_with_fifo_deadline(&dir, &fifo_path),
                    Err(OperationError::WorkspaceUnavailable)
                ));
                fs::remove_dir_all(dir).unwrap();
            }

            let dir = temp_catalog_dir("regular-symlink");
            fs::write(dir.join(peer), &valid).unwrap();
            let regular = dir.join("real-index.json");
            fs::write(&regular, &valid).unwrap();
            symlink(&regular, dir.join(target)).unwrap();
            assert!(matches!(
                read_local_catalog(&dir),
                Err(OperationError::WorkspaceUnavailable)
            ));
            fs::remove_dir_all(dir).unwrap();

            let dir = temp_catalog_dir("dangling-symlink");
            fs::write(dir.join(peer), &valid).unwrap();
            symlink(dir.join("missing.json"), dir.join(target)).unwrap();
            assert!(matches!(
                read_local_catalog(&dir),
                Err(OperationError::WorkspaceUnavailable)
            ));
            fs::remove_dir_all(dir).unwrap();

            for device in ["/dev/null", "/dev/zero"] {
                let dir = temp_catalog_dir("device-symlink");
                fs::write(dir.join(peer), &valid).unwrap();
                symlink(device, dir.join(target)).unwrap();
                assert!(matches!(
                    read_local_catalog(&dir),
                    Err(OperationError::WorkspaceUnavailable)
                ));
                fs::remove_dir_all(dir).unwrap();
            }
        }

        if unsafe { libc::geteuid() } != 0 {
            let dir = temp_catalog_dir("unreadable-dir");
            fs::write(dir.join("index.json"), &valid).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();
            let result = read_local_catalog(&dir);
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(matches!(result, Err(OperationError::WorkspaceUnavailable)));
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
