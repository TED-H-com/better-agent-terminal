//! Immutable identity; publication never replaces an existing directory entry.
//! Unix protects integrity with effective ownership and no group/other write bits.
//! Windows relies on the host data directory's ACL and rejects reparse points.
//! Windows std cannot sync directories: after power loss an identity may need to
//! be created again, which Connector reports as a changed host. File sync and
//! no-replace hard-link publication still prevent adopting partial contents.
//! Unix uses verified directory handles; an account able to replace data-root
//! entries can lose/change identity, as with host certificates/tokens, but cannot
//! redirect identity writes outside a verified directory or cause adoption from
//! an unchecked entry. The final bac entry is checked before returning/caching.
//! Windows path checks rely on the data root ACL. The configured root may itself
//! be a link/junction on either platform. Unsupported Unix directory sync has the
//! same power-loss durability limit as Windows. No file inode comparison is needed
//! on Unix: metadata and contents come from the same no-follow opened handle.
//! Temporary-file sync_all is strict (full sync on Apple): an unsupported file
//! sync refuses creation with STORAGE_UNAVAILABLE/temp_sync; existing identities
//! remain readable. Only unsupported directory sync is tolerated as above.
use crate::wire::{valid_host_id, Error};
use serde_json::{json, Value};
#[cfg(not(unix))]
use std::fs::DirBuilder;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
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
    DirectoryOpen,
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
    CachePoisoned,
}
impl Reason {
    pub fn name(self) -> &'static str {
        match self {
            Self::DataDirectoryUnavailable => "data_directory_unavailable",
            Self::DataDirectoryType => "data_directory_type",
            Self::DirectoryMetadata => "directory_metadata",
            Self::DirectoryType => "directory_type",
            Self::DirectoryOpen => "directory_open",
            Self::DirectoryIntegrity => "directory_integrity",
            Self::DirectoryReadOnly => "directory_read_only",
            Self::DirectoryCreate => "directory_create",
            Self::DirectorySync => "directory_sync",
            Self::IdentityMetadata => "identity_metadata",
            Self::IdentityType => "identity_type",
            Self::IdentityIntegrity => "identity_integrity",
            Self::IdentitySize => "identity_size",
            Self::IdentityContents => "identity_contents",
            Self::IdentityChanged => "identity_changed",
            Self::IdentityOpen => "identity_open",
            Self::IdentityRead => "identity_read",
            Self::Random => "random",
            Self::TempCreate => "temp_create",
            Self::TempWrite => "temp_write",
            Self::TempSync => "temp_sync",
            Self::Publish => "publish",
            Self::CachePoisoned => "cache_poisoned",
        }
    }
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
    pub fn wire_error(&self) -> Error {
        match self.error {
            Error::StorageUnavailable | Error::StorageCorrupt => {
                Error::StorageReason(Box::new(self.error.clone()), self.reason.name())
            }
            _ => self.error.clone(),
        }
    }

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
#[cfg(not(unix))]
fn options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(windows)]
    // FILE_FLAG_OPEN_REPARSE_POINT: validate the opened entry itself. Allow
    // read and delete sharing: a publisher may unlink its temporary hard link.
    // The opened handle still reads the same file; concurrent writes are denied.
    options.custom_flags(0x00200000).share_mode(1 | 4);
    options
}
#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> Result<()> {
    // Windows standard library has no directory sync.
    Ok(())
}
#[cfg(not(unix))]
fn data_directory(path: &Path) -> Result<()> {
    let meta = fs::metadata(path)
        .map_err(|_| StorageError::unavailable(Reason::DataDirectoryUnavailable, path))?;
    if !meta.is_dir() {
        return Err(StorageError::corrupt(Reason::DataDirectoryType, path));
    }
    Ok(())
}
#[cfg(not(unix))]
fn bac_directory(data_dir: &Path, path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let builder = DirBuilder::new();
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
#[cfg(not(unix))]
fn read(path: &Path) -> Result<Option<String>> {
    let before = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(StorageError::unavailable(Reason::IdentityMetadata, path)),
    };
    validate_file(&before, path)?;
    let file = options()
        .read(true)
        .open(path)
        .map_err(|_| StorageError::unavailable(Reason::IdentityOpen, path))?;
    let opened = file
        .metadata()
        .map_err(|_| StorageError::unavailable(Reason::IdentityMetadata, path))?;
    validate_file(&opened, path)?;
    let id = read_file(file, path)?;
    let after = fs::symlink_metadata(path)
        .map_err(|_| StorageError::unavailable(Reason::IdentityMetadata, path))?;
    validate_file(&after, path)?;
    Ok(Some(id))
}
fn read_file(mut file: File, path: &Path) -> Result<String> {
    let meta = file
        .metadata()
        .map_err(|_| StorageError::unavailable(Reason::IdentityMetadata, path))?;
    validate_file(&meta, path)?;
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
    Ok(id.into())
}
fn random_id(dir: &Path) -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| StorageError::unavailable(Reason::Random, dir))?;
    Ok(format!("h_{:032x}", u128::from_be_bytes(bytes)))
}
#[cfg(not(unix))]
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
#[cfg(unix)]
#[path = "identity_unix.rs"]
mod unix;
#[cfg(unix)]
use unix::Store;
#[cfg(not(unix))]
struct Store {
    path: PathBuf,
}
#[cfg(not(unix))]
impl Store {
    fn open(data_dir: &Path) -> Result<Self> {
        data_directory(data_dir)?;
        let path = data_dir.join("bac");
        bac_directory(data_dir, &path)?;
        Ok(Self { path })
    }
    fn read(&self) -> Result<Option<String>> {
        read(&self.path.join("host.json"))
    }
    fn create(&self, name: &str) -> Result<File> {
        options()
            .write(true)
            .create_new(true)
            .open(self.path.join(name))
            .map_err(|_| StorageError::unavailable(Reason::TempCreate, &self.path))
    }
    fn publish(&self, name: &str) -> std::io::Result<()> {
        fs::hard_link(self.path.join(name), self.path.join("host.json"))
    }
    fn remove(&self, name: &str) -> std::io::Result<()> {
        fs::remove_file(self.path.join(name))
    }
    fn sync(&self) -> Result<()> {
        sync_dir(&self.path)
    }
    fn finish(&self) -> Result<()> {
        Ok(())
    }
    fn cleanup(&self) {
        cleanup_old_temps(&self.path)
    }
}
fn initialize(data_dir: &Path, mut point: impl FnMut(Point) -> Result<()>) -> Result<String> {
    let store = Store::open(data_dir)?;
    let dir = data_dir.join("bac");
    let target = dir.join("host.json");
    point(Point::DirectoryReady)?;
    if let Some(id) = store.read()? {
        store.sync()?;
        store.finish()?;
        return Ok(id);
    }
    store.cleanup();
    let candidate = random_id(&dir)?;
    let temp = format!("host.json.tmp-{candidate}");
    let mut file = store.create(&temp)?;
    // Fault callbacks simulate abrupt process exit, deliberately preserving debris.
    // Actual pre-publication I/O failures clean up only our own candidate.
    let cleanup_error = |reason, path: &Path| {
        let _ = store.remove(&temp);
        StorageError::unavailable(reason, path)
    };
    point(Point::TempCreated)?;
    let bytes = format!("{}\n", json!({"schema":1,"hostId":candidate})).into_bytes();
    let middle = bytes.len() / 2;
    file.write_all(&bytes[..middle])
        .map_err(|_| cleanup_error(Reason::TempWrite, &dir))?;
    point(Point::PartialWritten)?;
    file.write_all(&bytes[middle..])
        .map_err(|_| cleanup_error(Reason::TempWrite, &dir))?;
    point(Point::Written)?;
    file.sync_all()
        .map_err(|_| cleanup_error(Reason::TempSync, &dir))?;
    point(Point::FileSynced)?;
    drop(file);
    match store.publish(&temp) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(cleanup_error(Reason::Publish, &target)),
    }
    point(Point::Published)?;
    // Publication decides the identity. Cleanup failure cannot undo acceptance.
    let _ = store.remove(&temp);
    point(Point::TempRemoved)?;
    store.sync()?;
    point(Point::DirectorySynced)?;
    let id = store
        .read()?
        .ok_or_else(|| StorageError::corrupt(Reason::IdentityChanged, &target))?;
    store.finish()?;
    Ok(id)
}
pub fn load_or_create(data_dir: &Path) -> Result<String> {
    initialize(data_dir, |_| Ok(()))
}
#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
