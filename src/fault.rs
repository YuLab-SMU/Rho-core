//! Crash points for fault experiments. With the `fault-injection` feature, a
//! process whose `RHO_CORE_CRASH_AT` names a point kills itself there, so a
//! test can stop a real host inside one chosen window. Without the feature
//! every point is a no-op.

#[cfg(feature = "fault-injection")]
pub(crate) fn point(name: &str) {
    use std::sync::OnceLock;
    static TARGET: OnceLock<Option<String>> = OnceLock::new();
    let target = TARGET.get_or_init(|| std::env::var("RHO_CORE_CRASH_AT").ok());
    if target.as_deref() == Some(name) {
        kill_self();
    }
}

#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub(crate) fn point(_name: &str) {}

/// Deterministic failure for the owned-process cancellation experiment.
#[cfg(all(feature = "fault-injection", unix))]
pub(crate) fn io_failure(name: &str) -> Option<std::io::Error> {
    (std::env::var("RHO_CORE_FAIL_AT").ok().as_deref() == Some(name))
        .then(|| std::io::Error::from_raw_os_error(libc::EIO))
}

#[cfg(all(feature = "fault-injection", windows))]
pub(crate) fn io_failure(name: &str) -> Option<std::io::Error> {
    use windows_sys::Win32::Foundation::ERROR_IO_DEVICE;
    (std::env::var("RHO_CORE_FAIL_AT").ok().as_deref() == Some(name))
        .then(|| std::io::Error::from_raw_os_error(ERROR_IO_DEVICE as i32))
}

#[cfg(not(feature = "fault-injection"))]
pub(crate) fn io_failure(_name: &str) -> Option<std::io::Error> {
    None
}

/// Ends this process the way an uncatchable kill would.
#[cfg(feature = "fault-injection")]
fn kill_self() -> ! {
    #[cfg(unix)]
    {
        // SAFETY: kill has no memory-safety preconditions. SIGKILL cannot be
        // caught, but in a multithreaded process kill may return before the
        // process is gone, so this thread blocks rather than run on.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        loop {
            // SAFETY: pause has no preconditions.
            unsafe { libc::pause() };
        }
    }
    #[cfg(windows)]
    {
        // SAFETY: the pseudo handle of this process; termination is immediate.
        unsafe {
            windows_sys::Win32::System::Threading::TerminateProcess(
                -1isize as _,
                0x4000_000F, // STATUS_FATAL_APP_EXIT-shaped unhandled kill
            );
        }
        loop {
            std::thread::park();
        }
    }
}
