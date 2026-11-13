use super::*;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix {
    use super::super::platform::{initialize, Point};
    use super::*;
    use serde_json::Value;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::{
        fs,
        path::PathBuf,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Barrier,
        },
    };

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "bac-identity-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn store(&self) -> PathBuf {
            self.0.join("store")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            for path in [self.store(), self.store().join("bac")] {
                if fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir()) {
                    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
                }
            }
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    fn target(root: &Path) -> PathBuf {
        root.join("bac/host.json")
    }
    fn private_write(path: &Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[test]
    fn create_reuse_and_copy_preserve_identity_and_permissions() {
        let scratch = Scratch::new();
        let root = scratch.store();
        let id = load_or_create(&root).unwrap();
        assert!(valid_host_id(&id));
        let raw = fs::read(target(&root)).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&raw).unwrap()["hostId"], id);
        assert_eq!(load_or_create(&root).unwrap(), id);
        assert_eq!(fs::read(target(&root)).unwrap(), raw);
        assert_eq!(
            fs::metadata(root.join("bac")).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(target(&root)).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(root.join("bac")).unwrap().count(), 1);
        let copy = scratch.0.join("copy");
        fs::create_dir(&copy).unwrap();
        fs::create_dir(copy.join("bac")).unwrap();
        fs::set_permissions(copy.join("bac"), fs::Permissions::from_mode(0o700)).unwrap();
        private_write(&target(&copy), &raw);
        assert_eq!(load_or_create(&copy).unwrap(), id);
    }
    #[test]
    fn corrupt_and_oversized_files_are_never_replaced() {
        let scratch = Scratch::new();
        let root = scratch.store();
        load_or_create(&root).unwrap();
        let id = "h_0123456789abcdef0123456789abcdef";
        let cases = vec![
            b"{".to_vec(),
            vec![b'x'; 257],
            b"null".to_vec(),
            format!("{{\"schema\":2,\"hostId\":\"{id}\"}}\n").into_bytes(),
            b"{\"schema\":1,\"hostId\":\"bad\"}\n".to_vec(),
            format!("{{\"schema\":1,\"hostId\":\"{id}\",\"extra\":true}}\n").into_bytes(),
            format!("{{\"schema\":1,\"schema\":1,\"hostId\":\"{id}\"}}\n").into_bytes(),
        ];
        for raw in cases {
            private_write(&target(&root), &raw);
            assert_eq!(load_or_create(&root), Err(Error::StorageCorrupt));
            assert_eq!(fs::read(target(&root)).unwrap(), raw);
        }
    }
    #[test]
    fn symlinks_and_nonregular_identity_are_rejected() {
        let scratch = Scratch::new();
        let root = scratch.store();
        load_or_create(&root).unwrap();
        let moved = root.join("real.json");
        fs::rename(target(&root), &moved).unwrap();
        symlink(&moved, target(&root)).unwrap();
        assert_eq!(load_or_create(&root), Err(Error::StorageCorrupt));
        fs::remove_file(target(&root)).unwrap();
        symlink(root.join("absent"), target(&root)).unwrap();
        assert_eq!(load_or_create(&root), Err(Error::StorageCorrupt));
        fs::remove_file(target(&root)).unwrap();
        fs::create_dir(target(&root)).unwrap();
        assert_eq!(load_or_create(&root), Err(Error::StorageCorrupt));
        fs::remove_dir(target(&root)).unwrap();
        fs::remove_dir(root.join("bac")).unwrap();
        symlink(&scratch.0, root.join("bac")).unwrap();
        assert_eq!(load_or_create(&root), Err(Error::StorageCorrupt));
    }
    #[test]
    fn missing_directory_is_created_only_under_an_existing_parent() {
        let scratch = Scratch::new();
        assert!(load_or_create(&scratch.store()).is_ok());
        let absent = scratch.0.join("missing-parent/child");
        assert_eq!(load_or_create(&absent), Err(Error::StorageUnavailable));
        assert!(!absent.exists());
    }
    #[test]
    fn readonly_directory_without_identity_is_unavailable() {
        let scratch = Scratch::new();
        let root = scratch.store();
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(load_or_create(&root), Err(Error::StorageUnavailable));
        assert!(!root.join("bac").exists());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        load_or_create(&root).unwrap();
        fs::remove_file(target(&root)).unwrap();
        fs::set_permissions(root.join("bac"), fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(load_or_create(&root), Err(Error::StorageUnavailable));
        assert_eq!(fs::read_dir(root.join("bac")).unwrap().count(), 0);
    }
    #[test]
    fn readonly_existing_identity_is_reusable_without_rewrite() {
        let scratch = Scratch::new();
        let root = scratch.store();
        let id = load_or_create(&root).unwrap();
        fs::set_permissions(target(&root), fs::Permissions::from_mode(0o400)).unwrap();
        fs::set_permissions(root.join("bac"), fs::Permissions::from_mode(0o500)).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(load_or_create(&root).unwrap(), id);
    }
    #[test]
    fn broad_existing_permissions_are_not_silently_fixed() {
        let scratch = Scratch::new();
        let root = scratch.store();
        load_or_create(&root).unwrap();
        fs::set_permissions(target(&root), fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(load_or_create(&root), Err(Error::StorageUnavailable));
        assert_eq!(
            fs::metadata(target(&root)).unwrap().permissions().mode() & 0o777,
            0o644
        );
        fs::set_permissions(target(&root), fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(root.join("bac"), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(load_or_create(&root), Err(Error::StorageUnavailable));
    }
    #[test]
    fn two_initializers_publish_once_and_loser_reads_winner() {
        let scratch = Scratch::new();
        let barrier = Arc::new(Barrier::new(2));
        let jobs: Vec<_> = (0..2)
            .map(|_| {
                let root = scratch.store();
                let b = barrier.clone();
                std::thread::spawn(move || {
                    initialize(&root, |point| {
                        if point == Point::FileSynced {
                            b.wait();
                        }
                        Ok(())
                    })
                    .unwrap()
                })
            })
            .collect();
        let ids: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
        assert_eq!(ids[0], ids[1]);
        assert_eq!(load_or_create(&scratch.store()).unwrap(), ids[0]);
        assert_eq!(
            fs::read_dir(scratch.store().join("bac")).unwrap().count(),
            1
        );
    }
    #[test]
    fn every_publication_crash_point_preserves_completed_identity() {
        for stop in [
            Point::DirectoryReady,
            Point::TempCreated,
            Point::PartialWritten,
            Point::Written,
            Point::FileSynced,
            Point::Published,
            Point::TempRemoved,
            Point::DirectorySynced,
        ] {
            let scratch = Scratch::new();
            let root = scratch.store();
            assert_eq!(
                initialize(&root, |point| if point == stop {
                    Err(Error::StorageUnavailable)
                } else {
                    Ok(())
                }),
                Err(Error::StorageUnavailable)
            );
            let before = fs::read(target(&root)).ok();
            let id = load_or_create(&root).unwrap();
            assert!(valid_host_id(&id));
            if let Some(bytes) = before {
                assert_eq!(fs::read(target(&root)).unwrap(), bytes, "{stop:?}");
            }
            // A completed identity never changes, even when old temp files remain.
            assert_eq!(load_or_create(&root).unwrap(), id);
        }
    }
    #[test]
    fn incomplete_temp_is_never_adopted_as_identity() {
        let scratch = Scratch::new();
        let root = scratch.store();
        initialize(&root, |point| {
            if point == Point::PartialWritten {
                Err(Error::StorageUnavailable)
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        let temp = fs::read_dir(root.join("bac"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let bytes = fs::read(&temp).unwrap();
        assert!(serde_json::from_slice::<Value>(&bytes).is_err());
        load_or_create(&root).unwrap();
        assert_eq!(fs::read(&temp).unwrap(), bytes);
        assert!(serde_json::from_slice::<Value>(&fs::read(target(&root)).unwrap()).is_ok());
    }
    #[test]
    fn process_identity_cache_serializes_concurrent_hellos() {
        let scratch = Scratch::new();
        let identity = Arc::new(Identity::default());
        let jobs: Vec<_> = (0..8)
            .map(|_| {
                let root = scratch.store();
                let i = identity.clone();
                std::thread::spawn(move || i.get(&root).unwrap())
            })
            .collect();
        let ids: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
        assert!(ids.iter().all(|id| id == &ids[0]));
        private_write(&target(&scratch.store()), b"corrupt");
        assert_eq!(identity.get(&scratch.store()).unwrap(), ids[0]);
    }
    #[test]
    fn failed_identity_initialization_can_be_retried_after_repair() {
        let scratch = Scratch::new();
        let root = scratch.store();
        load_or_create(&root).unwrap();
        let original = fs::read(target(&root)).unwrap();
        private_write(&target(&root), b"broken");
        let identity = Identity::default();
        assert_eq!(identity.get(&root), Err(Error::StorageCorrupt));
        private_write(&target(&root), &original);
        assert!(identity.get(&root).is_ok());
    }
}
#[cfg(not(unix))]
#[test]
fn unsupported_permission_backend_fails_closed_without_creating_files() {
    let path = std::env::temp_dir().join("bac-no-platform-identity");
    assert_eq!(load_or_create(&path), Err(Error::StorageUnavailable));
    assert!(!path.exists());
}
