//! Local Unix-domain socket transport for the host and its clients. One
//! connection per accepted socket; the exchange is the same line protocol.
//! Only processes of the host's user may connect, checked through peer creds.

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The socket's name inside the host's state directory.
pub const SOCKET_NAME: &str = "host.sock";

/// The socket the host serves for a state dir: a file inside it.
pub fn pipe_name(state_dir: &Path) -> PathBuf {
    state_dir.join(SOCKET_NAME)
}

/// A blocking duplex stream over one accepted socket.
pub struct IpcStream {
    stream: UnixStream,
}

impl IpcStream {
    /// Connects to the host's socket.
    pub fn connect(path: &Path) -> io::Result<Self> {
        Ok(Self {
            stream: UnixStream::connect(path)?,
        })
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.stream.set_write_timeout(timeout)
    }

    /// Peer credential check: only the host's own user may connect.
    pub fn peer_is_own_user(&self) -> Result<(), String> {
        // SAFETY: geteuid has no preconditions.
        let own = unsafe { libc::geteuid() };
        match peer_uid(&self.stream) {
            Ok(uid) if uid == own => Ok(()),
            Ok(uid) => Err(format!("peer uid {uid} is not the host's uid {own}")),
            Err(error) => Err(format!("peer uid is unavailable: {error}")),
        }
    }
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> io::Result<libc::uid_t> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: a connected socket descriptor and two valid out-pointers.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<libc::uid_t> {
    // SAFETY: zeroed ucred is a valid out-parameter of the size passed.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: a connected socket descriptor and a correctly sized buffer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn peer_uid(_stream: &UnixStream) -> io::Result<libc::uid_t> {
    Err(io::Error::other(
        "peer credentials are not implemented here",
    ))
}

impl Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

impl Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl Read for &IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&self.stream).read(buf)
    }
}

impl Write for &IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&self.stream).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&self.stream).flush()
    }
}

/// A listening Unix socket.
pub struct IpcListener {
    listener: UnixListener,
}

impl IpcListener {
    /// Replaces only a socket left by an earlier host of this state dir.
    pub fn bind(path: &Path) -> io::Result<Self> {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_socket() => fs::remove_file(path)?,
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "`{}` exists and is not a socket; not replacing it",
                    path.display()
                )));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(Self { listener })
    }

    /// Accepts one client within `timeout`; `Ok(None)` when the deadline passed.
    pub fn accept(&mut self, timeout: Duration) -> io::Result<Option<IpcStream>> {
        self.listener.set_nonblocking(true)?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    self.listener.set_nonblocking(false)?;
                    stream.set_nonblocking(false)?;
                    return Ok(Some(IpcStream { stream }));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        self.listener.set_nonblocking(false)?;
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => {
                    self.listener.set_nonblocking(false)?;
                    return Err(e);
                }
            }
        }
    }
}

/// The bound socket file's identity, so a host removes only its own file.
pub fn socket_identity(path: &Path) -> io::Result<(u64, u64)> {
    let meta = fs::metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

/// Removes the host's socket file at shutdown, unless the path no longer
/// names the file this host bound.
pub fn remove_socket_file(path: &Path, bound: (u64, u64)) {
    if let Ok(current) = fs::symlink_metadata(path)
        && (current.dev(), current.ino()) == bound
    {
        let _ = fs::remove_file(path);
    }
}
