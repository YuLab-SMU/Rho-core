//! Stateless helpers; each test fixture owns its processes and directories.
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rho_core::RunRequest;

pub mod dispatch;

#[track_caller]
pub fn within<T: Send + 'static>(
    timeout: Duration,
    what: &str,
    action: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(action());
    });
    receiver
        .recv_timeout(timeout)
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

#[track_caller]
pub fn fifo(path: &Path) {
    let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: valid NUL-terminated fixture path, no pointers retained.
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
}

pub fn request(
    id: &str,
    executor: &str,
    workdir: &str,
    code: &str,
    args: Vec<String>,
) -> RunRequest {
    RunRequest {
        request_id: id.into(),
        executor: executor.into(),
        workdir: workdir.into(),
        code: code.into(),
        args,
    }
}
