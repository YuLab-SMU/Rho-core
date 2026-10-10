//! Unix process primitives used to hold a run's process group.
//!
//! Only the holder thread of a run signals or reaps its process, and it never
//! signals after reaping. Until the main process is reaped its pid, and with it
//! the process group id, cannot be reused, so group signals reach this run only.

use std::io;
use std::os::fd::RawFd;
use std::time::Duration;

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
