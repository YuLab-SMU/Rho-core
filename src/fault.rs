//! Crash points for fault experiments. With the `fault-injection` feature, a
//! process whose `RHO_CORE_CRASH_AT` names a point kills itself with SIGKILL
//! there, so a test can stop a real host inside one chosen window. Without the
//! feature every point is a no-op.

#[cfg(feature = "fault-injection")]
pub(crate) fn point(name: &str) {
    use std::sync::OnceLock;
    static TARGET: OnceLock<Option<String>> = OnceLock::new();
    let target = TARGET.get_or_init(|| std::env::var("RHO_CORE_CRASH_AT").ok());
    if target.as_deref() == Some(name) {
        // SAFETY: kill has no memory-safety preconditions. SIGKILL cannot be
        // caught, but in a multithreaded process kill may return before the
        // process is gone, so this thread blocks rather than run on.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        loop {
            // SAFETY: pause has no preconditions.
            unsafe { libc::pause() };
        }
    }
}

#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub(crate) fn point(_name: &str) {}

/// Deterministic syscall failure for the owned-process cancellation experiment.
#[cfg(feature = "fault-injection")]
pub(crate) fn io_failure(name: &str) -> Option<std::io::Error> {
    (std::env::var("RHO_CORE_FAIL_AT").ok().as_deref() == Some(name))
        .then(|| std::io::Error::from_raw_os_error(libc::EIO))
}

#[cfg(not(feature = "fault-injection"))]
pub(crate) fn io_failure(_name: &str) -> Option<std::io::Error> {
    None
}
