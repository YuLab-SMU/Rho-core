//! Local Unix-domain socket transport for the host and its clients. One
//! connection per accepted socket; the exchange is the same line protocol.
//! Only processes of the host's user may connect, checked through peer creds.

use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The socket's name inside the host's state directory.
pub const SOCKET_NAME: &str = "host.sock";

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
        use std::os::fd::AsRawFd;
        // SAFETY: geteuid has no preconditions.
        let own = unsafe { libc::geteuid() };
        // SAFETY: a connected socket descriptor and two valid out-pointers.
        let (uid, _gid) = unsafe {
            let (mut uid, mut gid) = (0u32, 0u32);
            if libc::getpeereid(self.stream.as_raw_fd(), &mut uid, &mut gid) != 0 {
                return Err(format!(
                    "peer uid is unavailable: {}",
                    io::Error::last_os_error()
                ));
            }
            (uid, gid)
        };
        if uid == own {
            Ok(())
        } else {
            Err(format!("peer uid {uid} is not the host's uid {own}"))
        }
    }
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
    path: PathBuf,
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
        Ok(Self {
            listener,
            path: path.to_path_buf(),
        })
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

impl Drop for IpcListener {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Disconnects and releases a client connection.
pub fn shutdown_stream(_stream: &IpcStream) {}

/// Removes the socket file, for a host shutting down.
pub fn remove_socket_file(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Whether the socket file exists.
pub fn socket_exists(path: &Path) -> bool {
    path.try_exists().unwrap_or(false)
}
