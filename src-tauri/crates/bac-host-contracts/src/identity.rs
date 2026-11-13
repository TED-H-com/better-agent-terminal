//! Immutable identity; publication never replaces an existing directory entry.
#[cfg(unix)]
use crate::wire::valid_host_id;
use crate::wire::Error;
use std::path::Path;
use std::sync::Mutex;

#[derive(Default)]
pub struct Identity {
    value: Mutex<Option<String>>,
}
impl Identity {
    /// The adapter owns one Identity for each configured host data directory.
    /// Failed initialization is not cached; a repaired store can be retried.
    pub fn get(&self, data_dir: &Path) -> Result<String, Error> {
        let mut value = self.value.lock().map_err(|_| Error::Internal)?;
        if let Some(id) = value.as_ref() {
            return Ok(id.clone());
        }
        let id = load_or_create(data_dir)?;
        *value = Some(id.clone());
        Ok(id)
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use serde_json::{json, Value};
    use std::fs::{self, DirBuilder, File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
    use std::time::Duration;

    // POSIX uid_t is u32 on supported Linux/macOS targets. No new platform crate.
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    #[cfg(target_os = "linux")]
    const NOFOLLOW: i32 = 0o400000;
    #[cfg(target_os = "macos")]
    const NOFOLLOW: i32 = 0x100;
    // Other Unix platforms must be reviewed rather than opening symlinks.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    const NOFOLLOW: i32 = 0;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum Point {
        DirectoryReady,
        TempCreated,
        PartialWritten,
        Written,
        FileSynced,
        Published,
        TempRemoved,
        DirectorySynced,
    }

    fn unavailable(_: std::io::Error) -> Error {
        Error::StorageUnavailable
    }
    fn private(meta: &fs::Metadata) -> bool {
        // SAFETY: geteuid has no arguments, side effects, or memory preconditions.
        meta.uid() == unsafe { geteuid() } && meta.mode() & 0o077 == 0
    }
    fn sync_dir(path: &Path) -> Result<(), Error> {
        OpenOptions::new()
            .read(true)
            .custom_flags(NOFOLLOW)
            .open(path)
            .and_then(|f| f.sync_all())
            .map_err(unavailable)
    }
    fn directory(path: &Path, require_private: bool) -> Result<(), Error> {
        match fs::symlink_metadata(path) {
            Ok(meta) => {
                if !meta.is_dir() || meta.file_type().is_symlink() {
                    return Err(Error::StorageCorrupt);
                }
                if require_private && !private(&meta) {
                    return Err(Error::StorageUnavailable);
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                // Honor a read-only directory even when the server/test runs
                // as root and the OS would otherwise bypass its mode bits.
                let parent = path.parent().ok_or(Error::StorageUnavailable)?;
                if fs::metadata(parent)
                    .map_err(unavailable)?
                    .permissions()
                    .readonly()
                {
                    return Err(Error::StorageUnavailable);
                }
                match DirBuilder::new().mode(0o700).create(path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(unavailable(err)),
                }
                let meta = fs::symlink_metadata(path).map_err(unavailable)?;
                if !meta.is_dir() || meta.file_type().is_symlink() {
                    return Err(Error::StorageCorrupt);
                }
                if !private(&meta) {
                    return Err(Error::StorageUnavailable);
                }
                sync_dir(path.parent().ok_or(Error::StorageUnavailable)?)?;
            }
            Err(err) => return Err(unavailable(err)),
        }
        Ok(())
    }
    fn read(path: &Path) -> Result<Option<String>, Error> {
        let before = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(unavailable(err)),
        };
        if !before.is_file() || before.file_type().is_symlink() || before.len() > 256 {
            return Err(Error::StorageCorrupt);
        }
        if !private(&before) {
            return Err(Error::StorageUnavailable);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(NOFOLLOW)
            .open(path)
            .map_err(unavailable)?;
        let opened = file.metadata().map_err(unavailable)?;
        if before.ino() != opened.ino() || before.dev() != opened.dev() {
            return Err(Error::StorageCorrupt);
        }
        let mut bytes = Vec::new();
        file.take(257)
            .read_to_end(&mut bytes)
            .map_err(unavailable)?;
        if bytes.len() > 256 {
            return Err(Error::StorageCorrupt);
        }
        let doc: Value = serde_json::from_slice(&bytes).map_err(|_| Error::StorageCorrupt)?;
        let obj = doc.as_object().ok_or(Error::StorageCorrupt)?;
        let id = doc["hostId"]
            .as_str()
            .filter(|s| valid_host_id(s))
            .ok_or(Error::StorageCorrupt)?;
        if obj.len() != 2 || doc["schema"] != 1 {
            return Err(Error::StorageCorrupt);
        }
        // The on-disk schema is compact JSON + LF. Comparing its two allowed
        // spellings also rejects duplicate/escaped keys and trailing garbage.
        let sorted = format!("{}\n", json!({"schema":1,"hostId":id}));
        let schema_first = format!("{{\"schema\":1,\"hostId\":\"{id}\"}}\n");
        if bytes != sorted.as_bytes() && bytes != schema_first.as_bytes() {
            return Err(Error::StorageCorrupt);
        }
        let after = fs::symlink_metadata(path).map_err(unavailable)?;
        if after.ino() != opened.ino()
            || after.dev() != opened.dev()
            || !after.is_file()
            || !private(&after)
        {
            return Err(Error::StorageCorrupt);
        }
        Ok(Some(id.into()))
    }
    fn random_id() -> Result<String, Error> {
        let mut bytes = [0u8; 16];
        File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(unavailable)?;
        Ok(format!("h_{:032x}", u128::from_be_bytes(bytes)))
    }
    fn cleanup_old_temps(dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(suffix) = name.to_str().and_then(|s| s.strip_prefix("host.json.tmp-")) else {
                continue;
            };
            if !valid_host_id(suffix) {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_file()
                && private(&meta)
                && meta
                    .modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(86400))
            {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    pub(super) fn initialize(
        data_dir: &Path,
        mut point: impl FnMut(Point) -> Result<(), Error>,
    ) -> Result<String, Error> {
        if NOFOLLOW == 0 {
            return Err(Error::StorageUnavailable);
        }
        directory(data_dir, false)?;
        let dir = data_dir.join("bac");
        directory(&dir, true)?;
        point(Point::DirectoryReady)?;
        let target = dir.join("host.json");
        if let Some(id) = read(&target)? {
            // Also completes a publication whose process died before dir fsync.
            sync_dir(&dir)?;
            return Ok(id);
        }
        if fs::metadata(&dir)
            .map_err(unavailable)?
            .permissions()
            .readonly()
        {
            return Err(Error::StorageUnavailable);
        }
        cleanup_old_temps(&dir);
        let candidate = random_id()?;
        let temp = dir.join(format!("host.json.tmp-{candidate}"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(NOFOLLOW)
            .open(&temp)
            .map_err(unavailable)?;
        point(Point::TempCreated)?;
        let bytes = format!("{}\n", json!({"schema":1,"hostId":candidate})).into_bytes();
        let middle = bytes.len() / 2;
        file.write_all(&bytes[..middle]).map_err(unavailable)?;
        point(Point::PartialWritten)?;
        file.write_all(&bytes[middle..]).map_err(unavailable)?;
        point(Point::Written)?;
        file.sync_all().map_err(unavailable)?;
        point(Point::FileSynced)?;
        match fs::hard_link(&temp, &target) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(unavailable(err)),
        }
        point(Point::Published)?;
        fs::remove_file(&temp).map_err(unavailable)?;
        point(Point::TempRemoved)?;
        sync_dir(&dir)?;
        point(Point::DirectorySynced)?;
        read(&target)?.ok_or(Error::StorageCorrupt)
    }
}

pub fn load_or_create(data_dir: &Path) -> Result<String, Error> {
    #[cfg(unix)]
    {
        platform::initialize(data_dir, |_| Ok(()))
    }
    // std's Windows Permissions cannot establish an owner-only DACL. Do not
    // publish an identity with weaker permissions; add a reviewed ACL adapter
    // before enabling identity storage on Windows.
    #[cfg(not(unix))]
    {
        let _ = data_dir;
        Err(Error::StorageUnavailable)
    }
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
