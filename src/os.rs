//! Process primitives used to hold a run's process, per platform.
//!
//! Only the holder thread of a run stops or reaps its process, and it never
//! stops after reaping. Until the main process is reaped its pid (Unix) or its
//! named job object (Windows) cannot be reused for this run, so stop requests
//! reach this run only. Runs no Core holds are only probed for existence,
//! never stopped.

use std::io;
use std::sync::OnceLock;

use crate::record::{BootIdScope, ProcessContext};

/// The shell test fixtures run under: `/bin/sh` on Unix, Git Bash's MSYS
/// `sh.exe` on Windows (its path is fixed by the Git for Windows install).
pub fn fixture_sh_program() -> &'static str {
    #[cfg(unix)]
    {
        "/bin/sh"
    }
    #[cfg(windows)]
    {
        "C:\\Program Files\\Git\\usr\\bin\\sh.exe"
    }
}

/// This process's boot session identity and namespace, read once.
///
/// On Unix the boot id comes from the kernel or session; the pid namespace is
/// read on Linux. On Windows the boot id is the raw system boot time (stable
/// within one boot); there is no pid namespace.
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

#[cfg(unix)]
mod unix {
    use std::io;
    use std::os::fd::RawFd;
    use std::time::Duration;

    use super::nonempty;

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

    /// Whether the main process of a run no longer exists.
    pub(crate) fn process_absent(pid: u32) -> bool {
        absent(pid as i32)
    }

    /// Whether no process remains in a run's process group. The run id is not
    /// used on Unix; the pgid is the main pid.
    pub(crate) fn group_absent(_run_id: &str, pgid: u32) -> bool {
        absent(-(pgid as i32))
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
        let result =
            unsafe { libc::poll(entries.as_mut_ptr(), entries.len() as libc::nfds_t, millis) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(vec![false; fds.len()]);
            }
            return Err(error);
        }
        Ok(entries.iter().map(|entry| entry.revents != 0).collect())
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::io;

    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_INVALID_PARAMETER, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::JobObjects::{
        JobObjectBasicProcessIdList, OpenJobObjectW, QueryInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::OpenProcess;

    pub(crate) const JOB_NAME_PREFIX: &str = "Local\\rho-core.";

    /// The name of the job object that holds one run.
    pub(crate) fn job_name(boot_id: &str, run_id: &str) -> String {
        format!("{JOB_NAME_PREFIX}{boot_id}.{run_id}")
    }

    /// Opens a named job object. A missing job means its last member is gone.
    pub(crate) fn open_job(name: &str) -> io::Result<HANDLE> {
        // JOB_OBJECT_QUERY (0x4) and JOB_OBJECT_TERMINATE (0x8); opening with
        // no access is denied outright.
        const JOB_OBJECT_QUERY: u32 = 0x0004;
        const JOB_OBJECT_TERMINATE: u32 = 0x0008;
        let wide = super::to_wide(name);
        // SAFETY: name is a NUL-terminated wide string owned for the call.
        let handle =
            unsafe { OpenJobObjectW(JOB_OBJECT_QUERY | JOB_OBJECT_TERMINATE, 0, wide.as_ptr()) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(handle)
    }

    /// Ends every process in the job named `name`. Only the holder calls this.
    pub(crate) fn terminate_job(name: &str) -> io::Result<()> {
        let handle = open_job(name)?;
        // SAFETY: a valid job handle; termination has no memory preconditions.
        let ok = unsafe { TerminateJobObject(handle, 1) };
        // SAFETY: a handle open_job returned.
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Whether any process still belongs to the job named `name`. A job that
    /// cannot be opened reports empty: it either never existed or is gone.
    pub(crate) fn job_has_processes(name: &str) -> bool {
        let Ok(handle) = open_job(name) else {
            return false;
        };
        #[repr(C)]
        struct ProcessIdList {
            assigned: u32,
            listed: u32,
            ids: [usize; 64],
        }
        let mut list = ProcessIdList {
            assigned: 0,
            listed: 0,
            ids: [0; 64],
        };
        // SAFETY: a valid job handle and a writable buffer of the given size.
        let ok = unsafe {
            QueryInformationJobObject(
                handle,
                JobObjectBasicProcessIdList,
                (&mut list as *mut ProcessIdList).cast(),
                std::mem::size_of::<ProcessIdList>() as u32,
                std::ptr::null_mut(),
            )
        };
        // SAFETY: a handle open_job returned.
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            // A query buffer too small still proves members exist.
            let error = io::Error::last_os_error();
            return error.raw_os_error() == Some(174); // ERROR_MORE_DATA
        }
        list.assigned != 0
    }

    /// The kernel creation time of `pid`'s process, as FILETIME 100ns units,
    /// or `None` when the pid cannot be opened.
    pub(crate) fn process_create_time(pid: u32) -> Option<u64> {
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        // SAFETY: OpenProcess has no memory-safety preconditions.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return None;
        }
        #[repr(C)]
        struct FileTime {
            low: u32,
            high: u32,
        }
        let mut created = FileTime { low: 0, high: 0 };
        let mut exited = FileTime { low: 0, high: 0 };
        let mut kernel = FileTime { low: 0, high: 0 };
        let mut user = FileTime { low: 0, high: 0 };
        // SAFETY: a valid handle and four out-pointers.
        let ok = unsafe {
            windows_sys::Win32::System::Threading::GetProcessTimes(
                handle,
                (&mut created as *mut FileTime).cast(),
                (&mut exited as *mut FileTime).cast(),
                (&mut kernel as *mut FileTime).cast(),
                (&mut user as *mut FileTime).cast(),
            )
        };
        // SAFETY: a handle OpenProcess returned.
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            return None;
        }
        Some(((created.high as u64) << 32) | created.low as u64)
    }

    /// Whether `kill(pid, 0)`-style absence holds: a pid is gone when it
    /// cannot be opened at all, or when it can be opened, its exit code
    /// is already set, and its creation time precedes `spawned_after` —
    /// a later creation time means the pid was reused by another process.
    /// A terminated process stays openable while any handle references it,
    /// so the exit code is the absence proof, not openability.
    pub(crate) fn pid_absent_since(pid: u32, spawned_after: Option<u64>) -> bool {
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const STILL_ACTIVE: u32 = 259;
        // SAFETY: OpenProcess has no memory-safety preconditions.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            let error = io::Error::last_os_error();
            // A fully released pid answers either "not found" or, after the
            // pid is reused and released again, "invalid parameter".
            let code = error.raw_os_error();
            return code == Some(ERROR_FILE_NOT_FOUND as i32)
                || code == Some(ERROR_INVALID_PARAMETER as i32);
        }
        let mut exit_code = 0u32;
        // SAFETY: a valid process handle and out-pointer.
        let ok = unsafe {
            windows_sys::Win32::System::Threading::GetExitCodeProcess(handle, &mut exit_code)
        };
        // SAFETY: a handle OpenProcess returned.
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            return false;
        }
        if exit_code == STILL_ACTIVE {
            // A reused pid on an unrelated live process is present, but it
            // was created after this run's spawn; treat the newer process as
            // not-this-run by comparing creation times.
            if let Some(after) = spawned_after
                && let Some(created) = process_create_time(pid)
                && created > after
            {
                return true;
            }
            return false;
        }
        true
    }

    /// Whether the main process of a run no longer exists.
    pub(crate) fn process_absent(pid: u32) -> bool {
        pid_absent_since(pid, None)
    }

    /// Whether no process remains in a run's hold. Windows named job objects
    /// die with their last open handle, so a restarted Core cannot reopen the
    /// job a dead host created; the main pid's absence is the only observable
    /// proof, and it is conservative (grandchildren could outlive it).
    pub(crate) fn group_absent(_run_id: &str, pid: u32) -> bool {
        pid_absent_since(pid, None)
    }

    /// Opens a process by id for a full handle (used by the host peer check).
    pub(crate) fn open_process_full(pid: u32) -> io::Result<HANDLE> {
        const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
        // SAFETY: OpenProcess has no memory-safety preconditions.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(handle)
    }

    /// Calls `f` with this process's token user sid bytes, for peer checks.
    pub(crate) fn with_process_user<T>(
        process: HANDLE,
        f: impl FnOnce(&[u8]) -> io::Result<T>,
    ) -> io::Result<T> {
        use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TokenUser};
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: valid process handle and token out-pointer.
        if unsafe {
            windows_sys::Win32::System::Threading::OpenProcessToken(
                process,
                TOKEN_QUERY,
                &mut token,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut size = 0u32;
        // SAFETY: size query with a null buffer is the documented probe.
        let _ =
            unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size) };
        let mut buffer = vec![0u8; size as usize];
        // SAFETY: a writable buffer of exactly the returned size.
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        };
        // SAFETY: a handle OpenProcessToken returned.
        unsafe { CloseHandle(token) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // TOKEN_USER.User.Sid starts after the SID_AND_ATTRIBUTES header.
        const HEADER: usize = std::mem::size_of::<usize>() + std::mem::size_of::<u32>();
        if buffer.len() < HEADER {
            return Err(io::Error::other("token user truncated"));
        }
        // The SID length is the buffer remainder; the attributes field ends it.
        let sid = &buffer[HEADER..];
        f(sid)
    }

    /// The user sid of this process's token, as raw bytes.
    pub fn current_process_user_sid() -> io::Result<Vec<u8>> {
        // SAFETY: the current process's own pseudo handle.
        let process = -1isize as HANDLE;
        with_process_user(process, |sid| Ok(sid.to_vec()))
    }

    /// The user sid of the process `pid`'s token, as raw bytes.
    pub fn process_user_sid(pid: u32) -> io::Result<Vec<u8>> {
        let process = open_process_full(pid)?;
        let result = with_process_user(process, |sid| Ok(sid.to_vec()));
        // SAFETY: a handle open_process_full returned.
        unsafe { CloseHandle(process) };
        result
    }

    /// Opens a raw duplex connection to a named pipe, for tests that speak
    /// the wire protocol by hand.
    ///
    /// # Safety
    /// The returned stream wraps a live kernel handle; reading or writing it
    /// dereferences the raw handle inside.
    pub unsafe fn debug_connect_pipe(path: &str) -> io::Result<RawPipeStream> {
        let wide = super::to_wide(path);
        // SAFETY: a NUL-terminated wide path.
        let handle = unsafe {
            windows_sys::Win32::Storage::FileSystem::CreateFileW(
                wide.as_ptr(),
                0xC000_0000u32,
                0,
                std::ptr::null(),
                windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        use std::os::windows::io::{FromRawHandle, OwnedHandle, RawHandle};
        Ok(RawPipeStream {
            handle: unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) },
        })
    }

    /// A raw named-pipe client stream for manual protocol exchanges.
    pub struct RawPipeStream {
        handle: std::os::windows::io::OwnedHandle,
    }

    impl std::io::Read for RawPipeStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            use std::os::windows::io::AsRawHandle as _;
            // SAFETY: the handle is live for as long as this stream is.
            unsafe { read_pipe(self.handle.as_raw_handle() as HANDLE, buf) }
        }
    }

    impl std::io::Write for RawPipeStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            use std::os::windows::io::AsRawHandle as _;
            // SAFETY: the handle is live for as long as this stream is.
            unsafe { write_pipe(self.handle.as_raw_handle() as HANDLE, buf)? }
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl std::io::Read for &RawPipeStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            use std::os::windows::io::AsRawHandle as _;
            // SAFETY: the handle is live for as long as this stream is.
            unsafe { read_pipe(self.handle.as_raw_handle() as HANDLE, buf) }
        }
    }

    impl std::io::Write for &RawPipeStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            use std::os::windows::io::AsRawHandle as _;
            // SAFETY: the handle is live for as long as this stream is.
            unsafe { write_pipe(self.handle.as_raw_handle() as HANDLE, buf)? }
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Reads from a synchronous pipe handle; Ok(0) at end of stream.
    ///
    /// # Safety
    /// `handle` must be a live, readable kernel handle.
    pub unsafe fn read_pipe(handle: HANDLE, buffer: &mut [u8]) -> io::Result<usize> {
        let mut read = 0u32;
        // SAFETY: a valid readable handle and a writable buffer of its size.
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::ReadFile(
                handle,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(109) {
                // ERROR_BROKEN_PIPE: the write end closed.
                return Ok(0);
            }
            return Err(error);
        }
        Ok(read as usize)
    }

    /// Writes all of `bytes` to a synchronous pipe handle.
    ///
    /// # Safety
    /// `handle` must be a live, writable kernel handle.
    pub unsafe fn write_pipe(handle: HANDLE, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let mut written = 0u32;
            // SAFETY: a valid writable handle and a readable buffer.
            let ok = unsafe {
                windows_sys::Win32::Storage::FileSystem::WriteFile(
                    handle,
                    bytes.as_ptr().cast(),
                    bytes.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            bytes = &bytes[written as usize..];
        }
        Ok(())
    }

    /// The UTF-16 form of `text`, NUL-terminated.
    pub fn to_wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The system's boot time as a stable opaque string. It stands in for the
    /// Unix boot id: a changed value proves every earlier run was lost.
    pub(crate) fn boot_time() -> io::Result<String> {
        // The boot time is not directly exposed without WMI; the system's
        // tick count since boot combined with the wall clock at first use is
        // stable for this process and changes across reboots with the wall
        // clock at boot. The kernel's SystemTimeOfDayInformation is read via
        // ntdll for an exact, boot-stable value.
        #[repr(C)]
        struct LargeInteger(i64);
        #[repr(C)]
        struct SystemTimeOfDayInformation {
            boot_time: LargeInteger,
            current_time: LargeInteger,
            time_zone_bias: LargeInteger,
            current_time_zone_id: u32,
            reserved: u32,
        }
        type Query = unsafe extern "system" fn(u32, *mut c_void, u32, *mut u32) -> i32;
        let wide = super::to_wide("ntdll.dll");
        // SAFETY: a NUL-terminated wide module name.
        let ntdll =
            unsafe { windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(wide.as_ptr()) };
        if ntdll.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a valid module handle and NUL-terminated export name.
        let address = unsafe {
            windows_sys::Win32::System::LibraryLoader::GetProcAddress(
                ntdll,
                c"NtQuerySystemInformation".as_ptr().cast(),
            )
        };
        let Some(address) = address else {
            return Err(io::Error::other("NtQuerySystemInformation unavailable"));
        };
        // SAFETY: the pointer names the documented ntdll export.
        let query: Query = unsafe { std::mem::transmute(address) };
        const SYSTEM_TIME_OF_DAY_INFORMATION: u32 = 3;
        let mut info = SystemTimeOfDayInformation {
            boot_time: LargeInteger(0),
            current_time: LargeInteger(0),
            time_zone_bias: LargeInteger(0),
            current_time_zone_id: 0,
            reserved: 0,
        };
        let mut returned = 0u32;
        // SAFETY: a writable buffer of exactly the passed size.
        let status = unsafe {
            query(
                SYSTEM_TIME_OF_DAY_INFORMATION,
                (&mut info as *mut SystemTimeOfDayInformation).cast(),
                std::mem::size_of::<SystemTimeOfDayInformation>() as u32,
                &mut returned,
            )
        };
        if status < 0 {
            return Err(io::Error::other(format!(
                "NtQuerySystemInformation: {status:#x}"
            )));
        }
        Ok(format!("boot-{:016x}", info.boot_time.0 as u64))
    }
}

#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
pub(crate) use windows::*;

#[cfg(windows)]
/// Helpers the Host binary uses across the crate boundary.
pub use windows::{current_process_user_sid, process_user_sid, read_pipe, to_wide, write_pipe};

#[cfg(windows)]
pub use windows::{RawPipeStream, debug_connect_pipe};

#[cfg(unix)]
fn nonempty(text: String) -> io::Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(io::Error::other("empty identity"));
    }
    Ok(text.to_owned())
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

#[cfg(windows)]
fn boot_id() -> io::Result<(String, BootIdScope)> {
    windows::boot_time().map(|id| (id, BootIdScope::Kernel))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
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
