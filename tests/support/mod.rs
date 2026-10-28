//! Shared test fixtures.
#![allow(dead_code)]

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use rho_core::{Executor, RunRequest};

pub mod dispatch;

/// The canonical path in plain form, matching what Core reports on Windows
/// (`fs::canonicalize` returns an extended-path prefix there).
pub fn plain(path: std::path::PathBuf) -> std::path::PathBuf {
    let text = path.as_os_str().to_string_lossy().into_owned();
    const PLAIN: &str = "\\\\?\\";
    if let Some(rest) = text.strip_prefix(PLAIN) {
        std::path::PathBuf::from(rest)
    } else {
        path
    }
}

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

/// The shell fixture tasks run under: `/bin/sh` on Unix, Git Bash's MSYS
/// `sh.exe` on Windows. Its absolute path is an executor program, so no PATH
/// lookup is involved.
pub fn sh_program() -> &'static str {
    #[cfg(unix)]
    {
        "/bin/sh"
    }
    #[cfg(windows)]
    {
        r"C:\Program Files\Git\usr\bin\sh.exe"
    }
}

/// A fixture executor named `sh` for [`sh_program`].
pub fn sh_executor() -> Executor {
    Executor::new("sh", sh_program())
}

/// Creates a start/release gate.
///
/// On Unix a named pipe, read by fixture scripts with `read line < "$path"`;
/// writing one line opens the gate. On Windows a plain file: the script side
/// polls for its existence and reads the line from it, and `open` writes it.
#[track_caller]
pub fn gate(path: &Path) {
    #[cfg(unix)]
    {
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: valid NUL-terminated fixture path, no pointers retained.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    }
    #[cfg(windows)]
    {
        // No filesystem object is created ahead of time. The script side uses
        // `wait_gate` (existence poll), and `open` creates the file.
        let _ = path;
    }
}

/// Blocks until the gate `path` carries a line; returns that line. Unix reads
/// the FIFO; Windows polls for the file and reads its content.
#[track_caller]
pub fn wait_gate(path: &Path) -> String {
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            let line = text.lines().next().unwrap_or_default().to_owned();
            if !line.is_empty() {
                return line;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Opens the gate by writing `line` to it.
#[track_caller]
pub fn open_gate(path: &Path, line: &str) {
    std::fs::write(path, format!("{line}\n")).unwrap();
}

/// Kills `pid` hard, as `kill -9` would. Fixture cleanup only.
#[track_caller]
pub fn kill_pid(pid: u32, _signal: i32) {
    #[cfg(unix)]
    {
        // SAFETY: kill has no memory-safety preconditions.
        assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGKILL) }, 0);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess};
        const PROCESS_TERMINATE: u32 = 0x0001;
        // SAFETY: OpenProcess/TerminateProcess have no memory-safety preconditions.
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            assert!(!handle.is_null() && handle != INVALID_HANDLE_VALUE);
            TerminateProcess(handle, 1);
            CloseHandle(handle);
        }
    }
}

/// Sends `signal` (15 = term, 9 = kill) to `pid`. On Windows the host stops
/// through its console ctrl handler, so "term" posts CTRL_BREAK and "kill"
/// terminates the process outright.
#[track_caller]
pub fn send_signal(pid: u32, signal: i32) {
    #[cfg(unix)]
    {
        // SAFETY: kill has no memory-safety preconditions.
        assert_eq!(unsafe { libc::kill(pid as i32, signal) }, 0);
    }
    #[cfg(windows)]
    {
        if signal == 9 {
            kill_pid(pid, 9);
        } else {
            // The host runs in its own console and process group (the fixture
            // sets CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP), so the
            // ctrl event addressed to its group id reaches only it.
            use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
            // SAFETY: a plain event id and a process group id.
            assert_ne!(
                unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) },
                0
            );
        }
    }
}

/// Whether a process with `pid` exists, as `kill -0` would report.
#[track_caller]
pub fn process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks existence and permission.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER, INVALID_HANDLE_VALUE,
        };
        use windows_sys::Win32::System::Threading::OpenProcess;
        // SAFETY: OpenProcess has no memory-safety preconditions.
        let handle = unsafe { OpenProcess(0x1000, 0, pid) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            let error = std::io::Error::last_os_error();
            // A fully released pid answers ERROR_INVALID_PARAMETER; a live
            // but protected process answers ACCESS_DENIED and stays alive.
            let absent = error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32)
                || error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32);
            return !absent;
        }
        // SAFETY: a handle OpenProcess returned.
        unsafe { CloseHandle(handle) };
        true
    }
}

/// Waits until no process has `pid`.
pub fn wait_pid_absent(pid: u32, patience: Duration) {
    let deadline = std::time::Instant::now() + patience;
    while process_alive(pid) {
        assert!(
            std::time::Instant::now() < deadline,
            "process {pid} never ended"
        );
        thread::sleep(Duration::from_millis(10));
    }
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
