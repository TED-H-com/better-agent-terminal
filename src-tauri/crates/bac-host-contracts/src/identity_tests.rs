use super::*;
use serde_json::Value;
#[cfg(unix)]
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
        fs::create_dir(path.join("store")).unwrap();
        Self(path)
    }
    fn store(&self) -> PathBuf {
        self.0.join("store")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        #[cfg(unix)]
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
    #[cfg(unix)]
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
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(root.join("bac")).unwrap().permissions().mode() & 0o777,
        0o700
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(target(&root)).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_dir(root.join("bac")).unwrap().count(), 1);
    let copy = scratch.0.join("copy");
    fs::create_dir(&copy).unwrap();
    fs::create_dir(copy.join("bac")).unwrap();
    #[cfg(unix)]
    fs::set_permissions(copy.join("bac"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(target(&copy), &raw).unwrap();
    #[cfg(unix)]
    fs::set_permissions(target(&copy), fs::Permissions::from_mode(0o644)).unwrap();
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
#[cfg(unix)]
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
fn missing_data_directory_is_never_created() {
    let scratch = Scratch::new();
    assert!(load_or_create(&scratch.store()).is_ok());
    let absent = scratch.0.join("absent");
    assert_eq!(load_or_create(&absent), Err(Error::StorageUnavailable));
    assert!(!absent.exists());
}
#[cfg(unix)]
#[test]
fn readonly_directory_without_identity_is_unavailable() {
    let scratch = Scratch::new();
    let root = scratch.store();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o500)).unwrap();
    // The root has no mode policy. Kernel access checks normally refuse mkdir;
    // a privileged process can create there and must not be refused by a precheck.
    // SAFETY: geteuid has no arguments or memory preconditions.
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(load_or_create(&root), Err(Error::StorageUnavailable));
        assert!(!root.join("bac").exists());
    } else {
        assert!(load_or_create(&root).is_ok());
    }
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    load_or_create(&root).unwrap();
    fs::remove_file(target(&root)).unwrap();
    fs::set_permissions(root.join("bac"), fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(load_or_create(&root), Err(Error::StorageUnavailable));
    assert_eq!(fs::read_dir(root.join("bac")).unwrap().count(), 0);
}
#[cfg(unix)]
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
#[cfg(unix)]
#[test]
fn readable_modes_are_accepted_and_writable_modes_refused_without_changes() {
    let scratch = Scratch::new();
    let root = scratch.store();
    let id = load_or_create(&root).unwrap();
    let raw = fs::read(target(&root)).unwrap();
    for (path, safe, bad) in [
        (target(&root), 0o644, [0o664, 0o646]),
        (root.join("bac"), 0o755, [0o775, 0o757]),
    ] {
        fs::set_permissions(&path, fs::Permissions::from_mode(safe)).unwrap();
        assert_eq!(load_or_create(&root).unwrap(), id);
        for mode in bad {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(load_or_create(&root), Err(Error::StorageUnavailable));
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                mode
            );
            assert_eq!(fs::read(target(&root)).unwrap(), raw);
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(safe)).unwrap();
    }
    assert_eq!(load_or_create(&root).unwrap(), id);
    assert_eq!(fs::read(target(&root)).unwrap(), raw);
    assert_eq!(
        fs::metadata(target(&root)).unwrap().permissions().mode() & 0o777,
        0o644
    );
    assert_eq!(
        fs::metadata(root.join("bac")).unwrap().permissions().mode() & 0o777,
        0o755
    );
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
    assert_eq!(
        identity.get(&root).map_err(|e| e.error),
        Err(Error::StorageCorrupt)
    );
    private_write(&target(&root), &original);
    assert!(identity.get(&root).is_ok());
}

// Compare public wire categories in the publication fault tests; detailed reasons
// are separately asserted below; only fixed names are serialized to clients.
fn load_or_create(path: &Path) -> std::result::Result<String, Error> {
    super::load_or_create(path).map_err(|e| e.error)
}
fn initialize(
    path: &Path,
    mut point: impl FnMut(Point) -> std::result::Result<(), Error>,
) -> std::result::Result<String, Error> {
    super::initialize(path, |p| {
        point(p).map_err(|error| StorageError {
            error,
            reason: Reason::TempWrite,
            path: path.into(),
        })
    })
    .map_err(|e| e.error)
}
#[test]
fn diagnostic_reason_and_path_do_not_contain_identity_or_contents() {
    let scratch = Scratch::new();
    let root = scratch.store();
    let id = load_or_create(&root).unwrap();
    fs::write(target(&root), b"private-content-not-for-logs").unwrap();
    let err = super::load_or_create(&root).unwrap_err();
    assert_eq!(err.reason, Reason::IdentityContents);
    assert_eq!(err.path, target(&root));
    assert_eq!(err.error, Error::StorageCorrupt);
    assert!(!format!("{err:?}").contains(&id));
    assert!(!format!("{err:?}").contains("private-content-not-for-logs"));
    assert_eq!(err.error.reply()["error"]["details"], serde_json::json!({}));
}
#[test]
fn data_directory_must_be_an_existing_directory() {
    let scratch = Scratch::new();
    let file = scratch.0.join("file");
    fs::write(&file, b"untouched").unwrap();
    assert_eq!(load_or_create(&file), Err(Error::StorageCorrupt));
    assert_eq!(fs::read(&file).unwrap(), b"untouched");
}
#[cfg(windows)]
#[test]
fn windows_junctions_are_symlinks_and_all_reparse_targets_are_refused() {
    let scratch = Scratch::new();
    let root = scratch.store();
    let elsewhere = scratch.0.join("junction-target");
    fs::create_dir(&elsewhere).unwrap();
    // PowerShell creates a junction without the symbolic-link privilege.
    for path in [root.join("bac"), root.join("bac/host.json")] {
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "New-Item -ItemType Junction -Path $env:BAC_TEST_LINK -Target $env:BAC_TEST_TARGET | Out-Null"])
            .env("BAC_TEST_LINK", &path).env("BAC_TEST_TARGET", &elsewhere)
            .output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let meta = fs::symlink_metadata(&path).unwrap();
        assert!(
            meta.file_type().is_symlink(),
            "Rust treats junctions as name-surrogate symlinks"
        );
        assert!(redirected(&meta));
        assert_eq!(load_or_create(&root), Err(Error::StorageCorrupt));
        fs::remove_dir(&path).unwrap();
        if !root.join("bac").exists() {
            fs::create_dir(root.join("bac")).unwrap();
        }
    }
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
}
#[cfg(windows)]
#[test]
fn windows_symbolic_links_are_refused() {
    use std::os::windows::fs::{symlink_dir, symlink_file};
    let scratch = Scratch::new();
    let root = scratch.store();
    let elsewhere = scratch.0.join("symlink-target");
    fs::create_dir(&elsewhere).unwrap();
    symlink_dir(&elsewhere, root.join("bac")).expect("CI must permit symbolic-link creation");
    assert_eq!(load_or_create(&root), Err(Error::StorageCorrupt));
    fs::remove_dir(root.join("bac")).unwrap();
    fs::create_dir(root.join("bac")).unwrap();
    let other = elsewhere.join("file");
    fs::write(&other, b"untouched").unwrap();
    symlink_file(&other, target(&root)).expect("CI must permit symbolic-link creation");
    assert_eq!(load_or_create(&root), Err(Error::StorageCorrupt));
    assert_eq!(fs::read(&other).unwrap(), b"untouched");
    fs::remove_file(target(&root)).unwrap();
    let linked_root = scratch.0.join("linked-root");
    symlink_dir(&root, &linked_root).unwrap();
    assert_eq!(
        load_or_create(&linked_root).unwrap(),
        load_or_create(&root).unwrap()
    );
    fs::remove_dir(linked_root).unwrap();
}

#[cfg(unix)]
#[test]
fn replacing_verified_directory_never_redirects_or_adopts() {
    for link in [true, false] {
        let scratch = Scratch::new();
        let root = scratch.store();
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).unwrap();
        let saved = root.join("saved-bac");
        let error = super::initialize(&root, |p| {
            if p == Point::DirectoryReady {
                fs::rename(root.join("bac"), &saved).unwrap();
                if link {
                    symlink(&outside, root.join("bac")).unwrap();
                } else {
                    fs::create_dir(root.join("bac")).unwrap();
                    fs::set_permissions(root.join("bac"), fs::Permissions::from_mode(0o777))
                        .unwrap();
                }
            }
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.error, Error::StorageUnavailable);
        assert_eq!(error.reason, Reason::IdentityChanged);
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
        assert_eq!(fs::read_dir(root.join("bac")).unwrap().count(), 0);
        let bytes = fs::read(saved.join("host.json")).unwrap();
        if link {
            fs::remove_file(root.join("bac")).unwrap();
        } else {
            fs::remove_dir(root.join("bac")).unwrap();
        }
        fs::rename(saved, root.join("bac")).unwrap();
        let id = load_or_create(&root).unwrap();
        assert_eq!(load_or_create(&root).unwrap(), id);
        assert_eq!(fs::read(target(&root)).unwrap(), bytes);
        assert_eq!(fs::read_dir(root.join("bac")).unwrap().count(), 1);
    }
}
#[cfg(unix)]
#[test]
fn linked_data_root_works_and_fifo_identity_is_refused() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let scratch = Scratch::new();
    let alias = scratch.0.join("alias");
    symlink(scratch.store(), &alias).unwrap();
    let id = load_or_create(&alias).unwrap();
    assert_eq!(load_or_create(&scratch.store()).unwrap(), id);
    fs::remove_file(target(&alias)).unwrap();
    let name = CString::new(target(&alias).as_os_str().as_bytes()).unwrap();
    // SAFETY: name is NUL terminated and mode is a valid permission mask.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert_eq!(load_or_create(&alias), Err(Error::StorageCorrupt));
}
#[test]
fn cleanup_failure_after_publication_does_not_refuse_identity() {
    let scratch = Scratch::new();
    let root = scratch.store();
    let id = initialize(&root, |p| {
        if p == Point::Published {
            // A removed candidate makes the store's unlink fail with NotFound.
            let temp = fs::read_dir(root.join("bac"))
                .unwrap()
                .flatten()
                .find(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .starts_with("host.json.tmp-")
                })
                .unwrap();
            fs::remove_file(temp.path()).unwrap();
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(load_or_create(&root).unwrap(), id);
}
#[cfg(windows)]
#[test]
fn held_identity_reader_allows_publisher_cleanup_and_initialization() {
    let scratch = Scratch::new();
    let root = scratch.store();
    let mut reader = None;
    let id = initialize(&root, |p| {
        if p == Point::Published {
            reader = Some(options().read(true).open(target(&root)).unwrap());
            assert!(load_or_create(&root).is_ok());
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(load_or_create(&root).unwrap(), id);
    assert_eq!(fs::read_dir(root.join("bac")).unwrap().count(), 1);
    drop(reader);
}
#[test]
fn process_worker() {
    let Some(root) = std::env::var_os("BAC_IDENTITY_PROCESS_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let crash = std::env::var_os("BAC_IDENTITY_PROCESS_CRASH").is_some();
    let id = initialize(&root, |p| {
        if crash && p == Point::PartialWritten {
            std::process::exit(73);
        }
        if p == Point::FileSynced {
            println!("CANDIDATE_READY");
            std::io::stdout().flush().unwrap();
            std::io::stdin().read_exact(&mut [0u8]).unwrap();
        }
        Ok(())
    })
    .unwrap();
    fs::write(
        root.join(format!(
            "result-{}",
            std::env::var("BAC_IDENTITY_PROCESS_INDEX").unwrap()
        )),
        id,
    )
    .unwrap();
}
#[test]
fn separate_processes_publish_once_and_aborted_partial_write_recovers() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let scratch = Scratch::new();
    let root = scratch.store();
    let spawn = |index: &str, crash: bool| {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "identity::tests::process_worker", "--nocapture"])
            .env("BAC_IDENTITY_PROCESS_ROOT", &root)
            .env("BAC_IDENTITY_PROCESS_INDEX", index)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        if crash {
            cmd.env("BAC_IDENTITY_PROCESS_CRASH", "1");
        }
        Child(cmd.spawn().unwrap())
    };
    let mut jobs = [spawn("0", false), spawn("1", false)];
    let mut readers = Vec::new();
    for job in &mut jobs {
        let mut reader = BufReader::new(job.0.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line.contains("CANDIDATE_READY") {
                break;
            }
        }
        readers.push(reader);
    }
    for job in &mut jobs {
        job.0.stdin.take().unwrap().write_all(b"x").unwrap();
    }
    for reader in &mut readers {
        let mut rest = String::new();
        reader.read_to_string(&mut rest).unwrap();
    }
    for job in &mut jobs {
        assert!(job.0.wait().unwrap().success());
    }
    assert_eq!(
        fs::read(root.join("result-0")).unwrap(),
        fs::read(root.join("result-1")).unwrap()
    );
    assert_eq!(fs::read_dir(root.join("bac")).unwrap().count(), 1);
    fs::remove_file(target(&root)).unwrap();
    let mut crash = spawn("crash", true);
    assert_eq!(crash.0.wait().unwrap().code(), Some(73));
    assert!(!target(&root).exists());
    let temp = fs::read_dir(root.join("bac"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let partial = fs::read(&temp).unwrap();
    assert!(serde_json::from_slice::<Value>(&partial).is_err());
    let id = load_or_create(&root).unwrap();
    assert_eq!(load_or_create(&root).unwrap(), id);
    assert_eq!(fs::read(&temp).unwrap(), partial);
}

#[test]
fn storage_reason_names_are_unique_bounded_and_the_only_client_detail() {
    let reasons = [
        Reason::DataDirectoryUnavailable,
        Reason::DataDirectoryType,
        Reason::DirectoryMetadata,
        Reason::DirectoryType,
        Reason::DirectoryOpen,
        Reason::DirectoryIntegrity,
        Reason::DirectoryReadOnly,
        Reason::DirectoryCreate,
        Reason::DirectorySync,
        Reason::IdentityMetadata,
        Reason::IdentityType,
        Reason::IdentityIntegrity,
        Reason::IdentitySize,
        Reason::IdentityContents,
        Reason::IdentityChanged,
        Reason::IdentityOpen,
        Reason::IdentityRead,
        Reason::Random,
        Reason::TempCreate,
        Reason::TempWrite,
        Reason::TempSync,
        Reason::Publish,
        Reason::TempRemove,
        Reason::CachePoisoned,
    ];
    let mut seen = std::collections::HashSet::new();
    for reason in reasons {
        let name = reason.name();
        assert!(name.len() <= 64 && name.as_bytes()[0].is_ascii_lowercase());
        assert!(name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'));
        assert!(seen.insert(name));
        for error in [
            StorageError::unavailable(reason, Path::new("private-path")),
            StorageError::corrupt(reason, Path::new("private-path")),
        ] {
            let mut expected = error.error.reply();
            expected["error"]["details"] = json!({"reason":name});
            assert_eq!(error.wire_error().reply(), expected);
        }
    }
}

#[cfg(unix)]
#[test]
fn writable_data_root_has_no_integrity_mode_policy() {
    let scratch = Scratch::new();
    let root = scratch.store();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
    let id = load_or_create(&root).unwrap();
    assert_eq!(load_or_create(&root).unwrap(), id);
    assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o777);
}

#[cfg(unix)]
#[test]
fn denied_opens_report_open_reasons_without_mutating_storage() {
    // SAFETY: geteuid has no arguments or memory preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("SKIP: privileged users bypass the kernel permission refusal");
        return;
    }
    let scratch = Scratch::new();
    let root = scratch.store();
    let id = load_or_create(&root).unwrap();
    let bytes = fs::read(target(&root)).unwrap();
    for (path, restore, reason) in [
        (root.join("bac"), 0o700, Reason::DirectoryOpen),
        (target(&root), 0o600, Reason::IdentityOpen),
    ] {
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let result = super::load_or_create(&root);
        let mode = fs::symlink_metadata(&path).unwrap().mode() & 0o777;
        fs::set_permissions(&path, fs::Permissions::from_mode(restore)).unwrap();
        let failure = result.unwrap_err();
        assert_eq!(mode, 0o000);
        assert_eq!(failure.error, Error::StorageUnavailable);
        assert_eq!(failure.reason, reason);
        assert_eq!(
            failure.wire_error().reply(),
            json!({"ok":false,"error":{
            "code":"STORAGE_UNAVAILABLE","message":"BAC identity storage is unavailable",
            "retryable":false,"details":{"reason":reason.name()}}})
        );
        assert_eq!(fs::read(target(&root)).unwrap(), bytes);
        assert_eq!(fs::read_dir(root.join("bac")).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        assert_eq!(load_or_create(&root).unwrap(), id);
    }
}
#[cfg(unix)]
#[test]
fn linked_entries_keep_type_reasons_and_are_not_replaced() {
    let scratch = Scratch::new();
    let root = scratch.store();
    let id = load_or_create(&root).unwrap();
    let bytes = fs::read(target(&root)).unwrap();
    for (path, reason) in [
        (root.join("bac"), Reason::DirectoryType),
        (target(&root), Reason::IdentityType),
    ] {
        let saved = scratch.0.join("saved-entry");
        fs::rename(&path, &saved).unwrap();
        symlink(&saved, &path).unwrap();
        let failure = super::load_or_create(&root).unwrap_err();
        assert_eq!(failure.error, Error::StorageCorrupt);
        assert_eq!(failure.reason, reason);
        assert_eq!(
            failure.wire_error().reply(),
            json!({"ok":false,"error":{
            "code":"STORAGE_CORRUPT","message":"BAC identity storage is invalid",
            "retryable":false,"details":{"reason":reason.name()}}})
        );
        assert_eq!(fs::read_link(&path).unwrap(), saved);
        assert_eq!(fs::read(target(&root)).unwrap(), bytes);
        fs::remove_file(&path).unwrap();
        fs::rename(saved, &path).unwrap();
        assert_eq!(fs::read_dir(root.join("bac")).unwrap().count(), 1);
        assert_eq!(load_or_create(&root).unwrap(), id);
    }
}
