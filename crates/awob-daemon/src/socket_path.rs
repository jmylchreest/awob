//! Socket filesystem lifecycle, anchored to an owned private directory.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rustix::fs::{
    AtFlags, Mode, OFlags, RenameFlags, mkdirat, open, openat, renameat_with, unlinkat,
};

use crate::ipc::IpcError;

const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[derive(Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    parent: File,
    name: OsString,
    identity: Identity,
}

impl Server {
    pub fn bind(path: PathBuf) -> Result<Self, IpcError> {
        // Preserve the public Unix socket path length/encoding requirements,
        // even though binding below uses a shorter, descriptor-relative path.
        SocketAddr::from_pathname(&path)?;
        let name = path
            .file_name()
            .ok_or_else(|| unsafe_path(&path, "missing socket filename"))?
            .to_owned();
        let parent_path = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent_path)?;
        let parent =
            File::from(open(parent_path, DIRECTORY_FLAGS, Mode::empty()).map_err(io::Error::from)?);
        let metadata = parent.metadata()?;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(unsafe_path(
                parent_path,
                "parent must be owned by the running user and have no group or other permissions",
            ));
        }
        prepare_existing(&parent, &name, &path)?;

        // A staged bind avoids changing the process-wide umask in a multithreaded
        // daemon. The socket is inaccessible to other users until mode 600 is set.
        let stage = StagingDirectory::create(&parent)?;
        let staged_path = descriptor_path(&stage.directory, OsStr::new("s"));
        let listener = UnixListener::bind(&staged_path).map_err(|source| IpcError::Bind {
            path: path.clone(),
            source,
        })?;
        fs::set_permissions(&staged_path, fs::Permissions::from_mode(0o600))?;
        let identity = Identity::of(&fs::symlink_metadata(&staged_path)?);
        // Never overwrite an entry another startup created after the stale probe.
        renameat_with(
            &stage.directory,
            "s",
            &parent,
            &name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|source| IpcError::Bind {
            path: path.clone(),
            source: source.into(),
        })?;
        drop(stage);
        Ok(Self {
            listener,
            path,
            parent,
            name,
            identity,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn try_clone_listener(&self) -> Result<UnixListener, IpcError> {
        Ok(self.listener.try_clone()?)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Leave files and replacement sockets alone. The directory descriptor
        // also keeps cleanup attached to the original parent after a rename.
        if let Ok(metadata) = fs::symlink_metadata(descriptor_path(&self.parent, &self.name))
            && metadata.file_type().is_socket()
            && Identity::of(&metadata) == self.identity
        {
            let _ = unlinkat(&self.parent, &self.name, AtFlags::empty());
        }
    }
}

fn unsafe_path(path: &Path, reason: &'static str) -> IpcError {
    IpcError::UnsafePath {
        path: path.to_owned(),
        reason,
    }
}

fn descriptor_path(directory: &File, name: &OsStr) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name)
}

fn prepare_existing(parent: &File, name: &OsStr, public_path: &Path) -> Result<(), IpcError> {
    let anchored = descriptor_path(parent, name);
    let previous = match fs::symlink_metadata(&anchored) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !previous.file_type().is_socket() || previous.uid() != rustix::process::geteuid().as_raw() {
        return Err(unsafe_path(
            public_path,
            "existing entry is not a socket owned by the running user",
        ));
    }
    match UnixStream::connect(&anchored) {
        Ok(_) => {
            return Err(IpcError::AlreadyRunning {
                path: public_path.to_owned(),
            });
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
            ) => {}
        Err(source) => {
            return Err(IpcError::Bind {
                path: public_path.to_owned(),
                source,
            });
        }
    }
    let current = match fs::symlink_metadata(&anchored) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !current.file_type().is_socket() || Identity::of(&current) != Identity::of(&previous) {
        return Err(unsafe_path(
            public_path,
            "entry changed during stale socket check",
        ));
    }
    unlinkat(parent, name, AtFlags::empty()).map_err(|source| IpcError::StaleSocketUnlink {
        path: public_path.to_owned(),
        source: source.into(),
    })?;
    Ok(())
}

struct StagingDirectory<'a> {
    parent: &'a File,
    directory: File,
    name: OsString,
}

impl<'a> StagingDirectory<'a> {
    fn create(parent: &'a File) -> io::Result<Self> {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        for _ in 0..128 {
            let name = OsString::from(format!(
                ".awob-{:x}-{:x}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            match mkdirat(parent, &name, Mode::RWXU) {
                Ok(()) => match openat(parent, &name, DIRECTORY_FLAGS, Mode::empty()) {
                    Ok(directory) => {
                        return Ok(Self {
                            parent,
                            directory: File::from(directory),
                            name,
                        });
                    }
                    Err(error) => {
                        let _ = unlinkat(parent, &name, AtFlags::REMOVEDIR);
                        return Err(error.into());
                    }
                },
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create private socket staging directory",
        ))
    }
}

impl Drop for StagingDirectory<'_> {
    fn drop(&mut self) {
        let _ = unlinkat(&self.directory, "s", AtFlags::empty());
        let _ = unlinkat(self.parent, &self.name, AtFlags::REMOVEDIR);
    }
}
