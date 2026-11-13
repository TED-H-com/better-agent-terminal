//! Immutable identity; publication never replaces an existing directory entry.
//! Unix protects integrity with effective ownership and no group/other write bits.
//! Windows relies on the host data directory's ACL and rejects reparse points.
//! Windows std cannot sync directories: after power loss an identity may need to
//! be created again, which Connector reports as a changed host. File sync and
//! no-replace hard-link publication still prevent adopting partial contents.
use crate::wire::{valid_host_id, Error};
use serde_json::{json, Value};
use std::fs::{self, DirBuilder, Metadata, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Fixed diagnostic categories, never OS messages or file contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    DataDirectoryUnavailable,
    DataDirectoryType,
    DirectoryMetadata,
    DirectoryType,
    DirectoryIntegrity,
    DirectoryReadOnly,
    DirectoryCreate,
    DirectorySync,
    IdentityMetadata,
    IdentityType,
    IdentityIntegrity,
    IdentitySize,
    IdentityContents,
    IdentityChanged,
    IdentityOpen,
    IdentityRead,
    Random,
    TempCreate,
    TempWrite,
    TempSync,
    Publish,
    TempRemove,
    CachePoisoned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageError {
    pub error: Error,
    pub reason: Reason,
    /// Only the data directory, bac directory or final identity path. Never the
    /// temporary name, whose random component could become the host identity.
    pub path: PathBuf,
}
impl StorageError {
    pub fn unavailable(reason: Reason, path: &Path) -> Self {
        Self {
            error: Error::StorageUnavailable,
            reason,
            path: path.into(),
        }
    }
    fn corrupt(reason: Reason, path: &Path) -> Self {
        Self {
            error: Error::StorageCorrupt,
            reason,
            path: path.into(),
        }
    }
}
type Result<T> = std::result::Result<T, StorageError>;

#[derive(Default)]
pub struct Identity {
    value: Mutex<Option<String>>,
}
impl Identity {
    /// Failed initialization is not cached; repaired storage can be retried.
    pub fn get(&self, data_dir: &Path) -> Result<String> {
        let mut value = self.value.lock().map_err(|_| StorageError {
            error: Error::Internal,
            reason: Reason::CachePoisoned,
            path: data_dir.into(),
        })?;
        if let Some(id) = value.as_ref() {
            return Ok(id.clone());
        }
        let id = load_or_create(data_dir)?;
        *value = Some(id.clone());
        Ok(id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    DirectoryReady,
    TempCreated,
    PartialWritten,
    Written,
    FileSynced,
    Published,
    TempRemoved,
    DirectorySynced,
}
fn redirected(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        // FILE_ATTRIBUTE_REPARSE_POINT. Unlike is_symlink(), covers non-name
        // surrogate reparse tags too. Junctions are name-surrogate symlinks.
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
fn integrity(meta: &Metadata) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no arguments or memory preconditions.
        meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        true
    }
}
fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    #[cfg(windows)]
    // FILE_FLAG_OPEN_REPARSE_POINT: validate the opened entry itself. Allow
    // readers, but not concurrent write/delete of the opened identity handle.
    options.custom_flags(0x00200000).share_mode(1);
    options
}
fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    options()
        .read(true)
        .open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| StorageError::unavailable(Reason::DirectorySync, path))?;
    #[cfg(not(unix))]
    let _ = path; // Windows durability limitation documented above.
    Ok(())
}
fn data_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)
        .map_err(|_| StorageError::unavailable(Reason::DataDirectoryUnavailable, path))?;
    if !meta.is_dir() || redirected(&meta) {
        return Err(StorageError::corrupt(Reason::DataDirectoryType, path));
    }
    Ok(())
}
fn writable_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    if fs::symlink_metadata(path)
        .map_err(|_| StorageError::unavailable(Reason::DirectoryMetadata, path))?
        .permissions()
        .readonly()
    {
        // Honor read-only modes even under a root-run test/container.
        return Err(StorageError::unavailable(Reason::DirectoryReadOnly, path));
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
fn bac_directory(data_dir: &Path, path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            writable_directory(data_dir)?;
            let mut builder = DirBuilder::new();
            #[cfg(unix)]
            builder.mode(0o700);
            match builder.create(path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(StorageError::unavailable(Reason::DirectoryCreate, path)),
            }
            fs::symlink_metadata(path)
                .map_err(|_| StorageError::unavailable(Reason::DirectoryMetadata, path))?
        }
        Err(_) => return Err(StorageError::unavailable(Reason::DirectoryMetadata, path)),
    };
    if !meta.is_dir() || redirected(&meta) {
        return Err(StorageError::corrupt(Reason::DirectoryType, path));
    }
    if !integrity(&meta) {
        return Err(StorageError::unavailable(Reason::DirectoryIntegrity, path));
    }
    // Also finishes a prior initializer's mkdir whose parent sync was interrupted.
    sync_dir(data_dir)
}
fn validate_file(meta: &Metadata, path: &Path) -> Result<()> {
    if !meta.is_file() || redirected(meta) {
        return Err(StorageError::corrupt(Reason::IdentityType, path));
    }
    if meta.len() > 256 {
        return Err(StorageError::corrupt(Reason::IdentitySize, path));
    }
    if !integrity(meta) {
        return Err(StorageError::unavailable(Reason::IdentityIntegrity, path));
    }
    Ok(())
}
fn same_entry(a: &Metadata, b: &Metadata, path: &Path) -> Result<()> {
    #[cfg(unix)]
    if a.ino() != b.ino() || a.dev() != b.dev() {
        return Err(StorageError::corrupt(Reason::IdentityChanged, path));
    }
    // Windows stable std has no file-index API. The opened no-follow handle
    // denies concurrent write/delete; all three observations reject reparses.
    #[cfg(not(unix))]
    let _ = (a, b, path);
    Ok(())
}
fn read(path: &Path) -> Result<Option<String>> {
    let before = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(StorageError::unavailable(Reason::IdentityMetadata, path)),
    };
    validate_file(&before, path)?;
    let mut file = options()
        .read(true)
        .open(path)
        .map_err(|_| StorageError::unavailable(Reason::IdentityOpen, path))?;
    let opened = file
        .metadata()
        .map_err(|_| StorageError::unavailable(Reason::IdentityMetadata, path))?;
    validate_file(&opened, path)?;
    same_entry(&before, &opened, path)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(257)
        .read_to_end(&mut bytes)
        .map_err(|_| StorageError::unavailable(Reason::IdentityRead, path))?;
    if bytes.len() > 256 {
        return Err(StorageError::corrupt(Reason::IdentitySize, path));
    }
    let invalid = || StorageError::corrupt(Reason::IdentityContents, path);
    let doc: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let obj = doc.as_object().ok_or_else(invalid)?;
    let id = doc["hostId"]
        .as_str()
        .filter(|s| valid_host_id(s))
        .ok_or_else(invalid)?;
    if obj.len() != 2 || doc["schema"] != 1 {
        return Err(invalid());
    }
    let sorted = format!("{}\n", json!({"schema":1,"hostId":id}));
    let schema_first = format!("{{\"schema\":1,\"hostId\":\"{id}\"}}\n");
    if bytes != sorted.as_bytes() && bytes != schema_first.as_bytes() {
        return Err(invalid());
    }
    let after = fs::symlink_metadata(path)
        .map_err(|_| StorageError::unavailable(Reason::IdentityMetadata, path))?;
    validate_file(&after, path)?;
    same_entry(&after, &opened, path)?;
    Ok(Some(id.into()))
}
fn random_id(dir: &Path) -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| StorageError::unavailable(Reason::Random, dir))?;
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
            && !redirected(&meta)
            && integrity(&meta)
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
fn initialize(data_dir: &Path, mut point: impl FnMut(Point) -> Result<()>) -> Result<String> {
    data_directory(data_dir)?;
    let dir = data_dir.join("bac");
    bac_directory(data_dir, &dir)?;
    point(Point::DirectoryReady)?;
    let target = dir.join("host.json");
    if let Some(id) = read(&target)? {
        sync_dir(&dir)?;
        return Ok(id);
    }
    writable_directory(&dir)?;
    cleanup_old_temps(&dir);
    let candidate = random_id(&dir)?;
    let temp = dir.join(format!("host.json.tmp-{candidate}"));
    let mut file = options()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|_| StorageError::unavailable(Reason::TempCreate, &dir))?;
    point(Point::TempCreated)?;
    let bytes = format!("{}\n", json!({"schema":1,"hostId":candidate})).into_bytes();
    let middle = bytes.len() / 2;
    file.write_all(&bytes[..middle])
        .map_err(|_| StorageError::unavailable(Reason::TempWrite, &dir))?;
    point(Point::PartialWritten)?;
    file.write_all(&bytes[middle..])
        .map_err(|_| StorageError::unavailable(Reason::TempWrite, &dir))?;
    point(Point::Written)?;
    file.sync_all()
        .map_err(|_| StorageError::unavailable(Reason::TempSync, &dir))?;
    point(Point::FileSynced)?;
    // Release the Windows write handle before hard-link publication/readback.
    drop(file);
    match fs::hard_link(&temp, &target) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(StorageError::unavailable(Reason::Publish, &target)),
    }
    point(Point::Published)?;
    fs::remove_file(&temp).map_err(|_| StorageError::unavailable(Reason::TempRemove, &dir))?;
    point(Point::TempRemoved)?;
    sync_dir(&dir)?;
    point(Point::DirectorySynced)?; // On Windows this is a no-op boundary, not a durability claim.
    read(&target)?.ok_or_else(|| StorageError::corrupt(Reason::IdentityChanged, &target))
}
pub fn load_or_create(data_dir: &Path) -> Result<String> {
    initialize(data_dir, |_| Ok(()))
}
#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
