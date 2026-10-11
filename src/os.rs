//! Unix process primitives used to hold a run's process group.
//!
//! Only the holder thread of a run signals or reaps its process, and it never
//! signals after reaping. Until the main process is reaped its pid, and with it
//! the process group id, cannot be reused, so group signals reach this run only.
//! Runs no Core holds are only probed with signal 0, never signalled.

use std::io;
use std::os::fd::RawFd;
use std::sync::OnceLock;
use std::time::Duration;

use crate::record::{BootIdScope, ProcessContext};

/// Reports whether `pid` has exited, leaving it unreaped (`WNOWAIT`).
pub(crate) fn has_exited(pid: i32) -> io::Result<bool> {
    loop {
        // SAFETY: zeroed siginfo_t is a valid out-parameter; si_pid stays 0
        // when WNOHANG finds no state change.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
        // SAFETY: plain syscall on a child of this process with a valid pointer.
        let result = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) };
        if result == 0 {
            // SAFETY: waitid filled (or left zeroed) the siginfo_t.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Sends `signal` to every member of process group `pgid`.
pub(crate) fn signal_group(pgid: i32, signal: i32) -> io::Result<()> {
    if signal == libc::SIGTERM
        && let Some(error) = crate::fault::io_failure("signal-term")
    {
        return Err(error);
    }
    // SAFETY: kill has no memory-safety preconditions.
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Whether any process remains in group `pgid` (EPERM still means present).
pub(crate) fn group_exists(pgid: i32) -> bool {
    match signal_group(pgid, 0) {
        Ok(()) => true,
        Err(error) => error.raw_os_error() == Some(libc::EPERM),
    }
}

/// Whether `kill(target, 0)` reports that no such process (`target > 0`) or
/// process group (`target < 0`) exists. Any other answer is not absence.
pub(crate) fn absent(target: i32) -> bool {
    // SAFETY: kill with signal 0 only checks existence and permission.
    let result = unsafe { libc::kill(target, 0) };
    result != 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// This process's boot session and pid namespace, read once.
pub(crate) fn process_context() -> Result<ProcessContext, String> {
    static CONTEXT: OnceLock<Result<ProcessContext, String>> = OnceLock::new();
    CONTEXT
        .get_or_init(|| {
            let (boot_id, boot_id_scope) = boot_id().map_err(|e| format!("boot session: {e}"))?;
            Ok(ProcessContext {
                boot_id,
                boot_id_scope,
                pid_namespace: pid_namespace().map_err(|e| format!("pid namespace: {e}"))?,
            })
        })
        .clone()
}

#[cfg(target_os = "macos")]
fn boot_id() -> io::Result<(String, BootIdScope)> {
    let mut buffer = [0u8; 128];
    let mut length = buffer.len();
    // SAFETY: a NUL-terminated name and a buffer whose size is passed in `length`.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.bootsessionuuid".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    let bytes = &buffer[..length.min(buffer.len())];
    let text = bytes.split(|&b| b == 0).next().unwrap_or_default();
    nonempty(String::from_utf8_lossy(text).into_owned()).map(|id| (id, BootIdScope::Kernel))
}

#[cfg(target_os = "linux")]
fn boot_id() -> io::Result<(String, BootIdScope)> {
    use std::io::Read;
    use std::os::fd::AsRawFd;

    // Classify and read the same opened file: the path may be a bind-mounted
    // local identity instead of the kernel's procfs value.
    let mut file = std::fs::File::open("/proc/sys/kernel/random/boot_id")?;
    // SAFETY: statfs is a plain syscall output structure; fstatfs fills it.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: file owns a valid descriptor and stat is a valid output pointer.
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let scope = if i128::from(stat.f_type) == i128::from(libc::PROC_SUPER_MAGIC) {
        BootIdScope::Kernel
    } else {
        BootIdScope::LocalView
    };
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    nonempty(text).map(|id| (id, scope))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn boot_id() -> io::Result<(String, BootIdScope)> {
    Err(io::Error::other(
        "no boot session identity on this platform",
    ))
}

#[cfg(target_os = "linux")]
fn pid_namespace() -> io::Result<Option<String>> {
    let link = std::fs::read_link("/proc/self/ns/pid")?;
    nonempty(link.to_string_lossy().into_owned()).map(Some)
}

#[cfg(not(target_os = "linux"))]
fn pid_namespace() -> io::Result<Option<String>> {
    Ok(None)
}

fn nonempty(text: String) -> io::Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(io::Error::other("empty identity"));
    }
    Ok(text.to_owned())
}

/// Blocks until `pid` exits and reaps it, returning the raw wait status.
pub(crate) fn reap(pid: i32) -> io::Result<i32> {
    let mut status = 0;
    loop {
        // SAFETY: valid pointer to a local status integer.
        if unsafe { libc::waitpid(pid, &mut status, 0) } == pid {
            return Ok(status);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Waits up to `timeout` for input or hang-up on `fds`; returns readiness flags.
pub(crate) fn poll_readable(fds: &[RawFd], timeout: Duration) -> io::Result<Vec<bool>> {
    let mut entries: Vec<libc::pollfd> = fds
        .iter()
        .map(|&fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let millis = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    // SAFETY: entries is a valid, correctly sized pollfd array.
    let result = unsafe { libc::poll(entries.as_mut_ptr(), entries.len() as libc::nfds_t, millis) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(vec![false; fds.len()]);
        }
        return Err(error);
    }
    Ok(entries.iter().map(|entry| entry.revents != 0).collect())
}
