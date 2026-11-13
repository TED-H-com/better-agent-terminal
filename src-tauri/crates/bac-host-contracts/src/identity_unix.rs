//! All operations below the configured data root use verified directory handles.
use super::*;
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;

fn open_at(parent: &File, name: &CStr, flags: i32, mode: libc::mode_t) -> io::Result<File> {
    // SAFETY: parent is live, name is NUL terminated, flags/mode are valid openat arguments.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode as libc::c_int,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a fresh, exclusively owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn mkdir_at(parent: &File, name: &CStr) -> io::Result<()> {
    // SAFETY: parent is live and name is NUL terminated.
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
fn link_at(parent: &File, old: &CStr, new: &CStr) -> io::Result<()> {
    // SAFETY: both names are NUL terminated; parent remains live throughout linkat.
    let result = unsafe {
        libc::linkat(
            parent.as_raw_fd(),
            old.as_ptr(),
            parent.as_raw_fd(),
            new.as_ptr(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
fn unlink_at(parent: &File, name: &CStr) -> io::Result<()> {
    // SAFETY: name is NUL terminated and parent remains live throughout unlinkat.
    let result = unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
fn entry_error(error: io::Error, reason: Reason, path: &Path) -> StorageError {
    if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) {
        StorageError::corrupt(reason, path)
    } else {
        StorageError::unavailable(reason, path)
    }
}
fn unsupported_sync(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        [libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP, libc::EBADF].contains(&code)
    })
}
fn sync_handle(file: &File, path: &Path) -> Result<()> {
    match file.sync_all() {
        Ok(()) => Ok(()),
        Err(error) if unsupported_sync(&error) => Ok(()),
        Err(_) => Err(StorageError::unavailable(Reason::DirectorySync, path)),
    }
}
fn writable(file: &File, path: &Path) -> Result<()> {
    let meta = file
        .metadata()
        .map_err(|_| StorageError::unavailable(Reason::DirectoryMetadata, path))?;
    if meta.permissions().readonly() {
        return Err(StorageError::unavailable(Reason::DirectoryReadOnly, path));
    }
    Ok(())
}
pub(super) struct Store {
    data: File,
    bac: File,
    path: PathBuf,
}
impl Store {
    pub(super) fn open(data_dir: &Path) -> Result<Self> {
        // Only the configured root follows links. Never create or constrain its owner/mode.
        let data = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(data_dir)
            .map_err(|e| {
                if e.raw_os_error() == Some(libc::ENOTDIR) {
                    StorageError::corrupt(Reason::DataDirectoryType, data_dir)
                } else {
                    StorageError::unavailable(Reason::DataDirectoryUnavailable, data_dir)
                }
            })?;
        let path = data_dir.join("bac");
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        let bac = match open_at(&data, c"bac", flags, 0) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match mkdir_at(&data, c"bac") {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(_) => {
                        return Err(StorageError::unavailable(Reason::DirectoryCreate, &path))
                    }
                }
                sync_handle(&data, data_dir)?;
                open_at(&data, c"bac", flags, 0)
                    .map_err(|e| entry_error(e, Reason::DirectoryType, &path))?
            }
            Err(e) => return Err(entry_error(e, Reason::DirectoryType, &path)),
        };
        let meta = bac
            .metadata()
            .map_err(|_| StorageError::unavailable(Reason::DirectoryMetadata, &path))?;
        if !integrity(&meta) {
            return Err(StorageError::unavailable(Reason::DirectoryIntegrity, &path));
        }
        sync_handle(&data, data_dir)?;
        Ok(Self { data, bac, path })
    }
    pub(super) fn read(&self) -> Result<Option<String>> {
        let path = self.path.join("host.json");
        // NONBLOCK lets fstat reject a FIFO without waiting for a writer.
        let file = match open_at(
            &self.bac,
            c"host.json",
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            0,
        ) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(entry_error(e, Reason::IdentityType, &path)),
        };
        read_file(file, &path).map(Some)
    }
    pub(super) fn create(&self, name: &str) -> Result<File> {
        writable(&self.bac, &self.path)?;
        open_at(
            &self.bac,
            &CString::new(name).unwrap(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
            0o600,
        )
        .map_err(|_| StorageError::unavailable(Reason::TempCreate, &self.path))
    }
    pub(super) fn publish(&self, name: &str) -> io::Result<()> {
        link_at(&self.bac, &CString::new(name).unwrap(), c"host.json")
    }
    pub(super) fn remove(&self, name: &str) -> io::Result<()> {
        unlink_at(&self.bac, &CString::new(name).unwrap())
    }
    pub(super) fn sync(&self) -> Result<()> {
        sync_handle(&self.bac, &self.path)
    }
    pub(super) fn finish(&self) -> Result<()> {
        let fail = || StorageError::unavailable(Reason::IdentityChanged, &self.path);
        let current = open_at(
            &self.data,
            c"bac",
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_DIRECTORY,
            0,
        )
        .map_err(|_| fail())?;
        let now = current.metadata().map_err(|_| fail())?;
        let used = self.bac.metadata().map_err(|_| fail())?;
        if now.dev() != used.dev() || now.ino() != used.ino() {
            return Err(fail());
        }
        Ok(())
    }
    pub(super) fn cleanup(&self) {
        let Ok(entries) = fs::read_dir(&self.path) else {
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
            let Ok(name) = CString::new(name.as_bytes()) else {
                continue;
            };
            let Ok(file) = open_at(
                &self.bac,
                &name,
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
                0,
            ) else {
                continue;
            };
            let Ok(meta) = file.metadata() else {
                continue;
            };
            if meta.is_file()
                && integrity(&meta)
                && meta
                    .modified()
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(86400))
            {
                let _ = unlink_at(&self.bac, &name);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_unsupported_directory_sync_errors_are_tolerated() {
        for code in [libc::EINVAL, libc::ENOTSUP, libc::EOPNOTSUPP, libc::EBADF] {
            assert!(unsupported_sync(&io::Error::from_raw_os_error(code)));
        }
        for code in [libc::EIO, libc::ENOSPC, libc::EACCES] {
            assert!(!unsupported_sync(&io::Error::from_raw_os_error(code)));
        }
    }
}
