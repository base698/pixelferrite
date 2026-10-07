//! Exclusive sibling temporary files for atomic replacement.
//!
//! Temporary names never reuse an existing file or follow a symlink. Private
//! files have owner-only permissions from their first write, including on failure.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Make an application-owned directory private. Call only for directories the
/// application owns, not a general document destination or the user's home.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private storage directory must not be a symlink or regular file",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub struct AtomicFile {
    file: File,
    temporary: PathBuf,
    destination: PathBuf,
    committed: bool,
}

impl AtomicFile {
    pub fn new(path: &Path, private: bool) -> io::Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if private {
            create_private_dir(parent)?;
        } else {
            fs::create_dir_all(parent)?;
        }
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
        })?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for _ in 0..128 {
            let mut temporary_name = std::ffi::OsString::from(".");
            temporary_name.push(name);
            temporary_name.push(format!(
                ".{}.{stamp:x}.{:x}.tmp",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            let temporary = parent.join(temporary_name);
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                // Preserve restrictive permissions of an existing regular document.
                let mode = if private {
                    0o600
                } else {
                    fs::symlink_metadata(path)
                        .ok()
                        .filter(|m| m.is_file())
                        .map_or(0o666, |m| m.permissions().mode() & 0o777)
                };
                options.mode(mode);
            }
            match options.open(&temporary) {
                Ok(file) => {
                    return Ok(Self {
                        file,
                        temporary,
                        destination: path.to_owned(),
                        committed: false,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create an exclusive temporary file",
        ))
    }

    pub fn file(&mut self) -> &mut File {
        &mut self.file
    }

    pub fn commit(mut self) -> io::Result<()> {
        self.file.sync_all()?;
        fs::rename(&self.temporary, &self.destination)?;
        self.committed = true;
        // Durably record the rename where directory handles support syncing.
        #[cfg(unix)]
        {
            let parent = self
                .destination
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

impl Drop for AtomicFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.temporary);
        }
    }
}

pub fn write(path: &Path, bytes: &[u8], private: bool) -> io::Result<()> {
    let mut temporary = AtomicFile::new(path, private)?;
    temporary.file().write_all(bytes)?;
    temporary.commit()
}
