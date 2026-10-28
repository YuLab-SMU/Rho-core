//! Local named-pipe transport for the host and its clients, standing in for
//! the Unix-domain socket. One pipe instance per connection; the exchange is
//! the same line protocol. Only processes of the host's user may connect: the
//! host compares the client's process token user with its own.

use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::CreateEventW;

use rho_core::os;

/// The pipe's name inside the host's state directory.
pub const SOCKET_NAME: &str = "host.sock";

/// The named pipe the host serves for a state dir. The full state dir path
/// is hashed into the pipe name so two projects on one machine never collide
/// (temp dirs share their leaf name); the state-dir lock still guarantees one
/// host per state dir.
pub fn pipe_name(state_dir: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    state_dir.hash(&mut hasher);
    let hash = hasher.finish();
    PathBuf::from(format!(r"\\.\pipe\rho-core.{hash:016x}.{SOCKET_NAME}"))
}

/// A blocking duplex stream over one named-pipe instance.
pub struct IpcStream {
    handle: OwnedHandle,
    /// Timeout for the next write, mirroring Unix write timeouts.
    write_timeout: Option<Duration>,
}

impl IpcStream {
    /// Connects to the host's pipe, waiting up to one default timeout.
    pub fn connect(path: &Path) -> io::Result<Self> {
        let wide = os::to_wide(&path.as_os_str().to_string_lossy());
        // SAFETY: a NUL-terminated wide path.
        let handle = unsafe {
            windows_sys::Win32::Storage::FileSystem::CreateFileW(
                wide.as_ptr(),
                0xC0000000u32, // GENERIC_READ | GENERIC_WRITE
                0,
                std::ptr::null(),
                windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
                0,
                std::ptr::null_mut() as *mut _,
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: taking ownership of the handle CreateFileW returned.
        Ok(Self {
            handle: unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) },
            write_timeout: None,
        })
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        // Reads are synchronous; the timeout is enforced by the caller's
        // deadline on the greeting/reply waits, not by the pipe.
        let _ = timeout;
        Ok(())
    }

    pub fn set_write_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout = timeout;
        Ok(())
    }

    /// The user-sid equality check against this host's own token.
    pub fn peer_is_own_user(&self) -> Result<(), String> {
        let own = os::current_process_user_sid()
            .map_err(|e| format!("this process's user is unavailable: {e}"))?;
        let pid = pipe_client_pid(self.handle.as_raw_handle() as HANDLE)
            .map_err(|e| format!("peer process is unavailable: {e}"))?;
        let peer =
            os::process_user_sid(pid).map_err(|e| format!("peer user is unavailable: {e}"))?;
        if own == peer {
            Ok(())
        } else {
            Err(format!(
                "peer user is not this host's user (peer sid {:?}, own sid {:?})",
                hex_short(&peer),
                hex_short(&own)
            ))
        }
    }
}

impl Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use std::os::windows::io::AsRawHandle as _;
        // SAFETY: the handle is live for as long as this stream is.
        unsafe { os::read_pipe(self.handle.as_raw_handle() as HANDLE, buf) }
    }
}

impl Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        use std::os::windows::io::AsRawHandle as _;
        // SAFETY: the handle is live for as long as this stream is.
        unsafe { os::write_pipe(self.handle.as_raw_handle() as HANDLE, buf)? }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Read for &IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use std::os::windows::io::AsRawHandle as _;
        // SAFETY: the handle is live for as long as this stream is.
        unsafe { os::read_pipe(self.handle.as_raw_handle() as HANDLE, buf) }
    }
}

impl Write for &IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        use std::os::windows::io::AsRawHandle as _;
        // SAFETY: the handle is live for as long as this stream is.
        unsafe { os::write_pipe(self.handle.as_raw_handle() as HANDLE, buf)? }
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A listening named pipe. `accept` creates one instance and waits for a
/// client, with a deadline so the host can shut down.
pub struct IpcListener {
    path: PathBuf,
    first: bool,
}

impl IpcListener {
    /// Removes a stale pipe name if present (named pipes vanish with their
    /// server; nothing to clean) and prepares the listener.
    pub fn bind(path: &Path) -> io::Result<Self> {
        // Named pipes need no filesystem cleanup; a stale name means a server
        // is still alive, which the state-dir lock already prevents.
        let _ = path.metadata();
        Ok(Self {
            path: path.to_path_buf(),
            first: true,
        })
    }

    /// Accepts one client, waiting up to `timeout` for the connection.
    /// Returns `Ok(None)` on timeout.
    pub fn accept(&mut self, timeout: Duration) -> io::Result<Option<IpcStream>> {
        let wide = os::to_wide(&self.path.as_os_str().to_string_lossy());
        let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if self.first {
            open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        // SAFETY: a NUL-terminated wide name and valid mode constants.
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                64 << 10,
                64 << 10,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            let error = io::Error::last_os_error();
            if self.first && error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "a pipe server with this name already exists",
                ));
            }
            if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) {
                return Ok(None);
            }
            return Err(error);
        }
        self.first = false;
        let event = unsafe { CreateEventW(std::ptr::null_mut(), 1, 0, std::ptr::null()) };
        if event == INVALID_HANDLE_VALUE || event.is_null() {
            // SAFETY: a handle CreateNamedPipeW returned.
            unsafe { CloseHandle(handle) };
            return Err(io::Error::last_os_error());
        }
        #[repr(C)]
        struct Overlapped {
            internal: usize,
            internal_high: usize,
            offset: u32,
            offset_high: u32,
            event: HANDLE,
        }
        let mut overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event,
        };
        // SAFETY: a valid pipe handle and overlapped structure.
        let started =
            unsafe { ConnectNamedPipe(handle, (&mut overlapped as *mut Overlapped).cast()) };
        let connected = if started == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(536) {
                // ERROR_PIPE_CONNECTED: a client arrived in the gap.
                true
            } else if error.raw_os_error() == Some(997) {
                // ERROR_IO_PENDING: wait for the client or the deadline.
                // SAFETY: a valid event handle.
                match unsafe {
                    windows_sys::Win32::System::Threading::WaitForSingleObject(
                        event,
                        timeout.as_millis().min(u32::MAX as u128) as u32,
                    )
                } {
                    WAIT_OBJECT_0 => true,
                    WAIT_TIMEOUT => false,
                    _ => false,
                }
            } else {
                return Err({
                    // SAFETY: a handle CreateNamedPipeW returned.
                    unsafe { CloseHandle(handle) };
                    io::Error::last_os_error()
                });
            }
        } else {
            true
        };
        // SAFETY: the event handle returned by CreateEventW above.
        unsafe { CloseHandle(event) };
        if !connected {
            // SAFETY: a handle CreateNamedPipeW returned.
            unsafe { CloseHandle(handle) };
            return Ok(None);
        }
        // SAFETY: taking ownership of the connected pipe instance.
        Ok(Some(IpcStream {
            handle: unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) },
            write_timeout: None,
        }))
    }
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        // Pipe instances close with their handles; the name disappears when
        // the last server instance closes. Nothing to unlink.
    }
}

fn pipe_client_pid(handle: HANDLE) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: a valid pipe server handle and out-pointer.
    if unsafe { GetNamedPipeClientProcessId(handle, &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

fn hex_short(bytes: &[u8]) -> String {
    bytes
        .iter()
        .rev()
        .take(4)
        .map(|b| format!("{b:02x}"))
        .collect()
}
