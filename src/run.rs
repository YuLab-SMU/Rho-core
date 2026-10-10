//! One accepted run: its record, the thread holding its process, and retained output.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::error::ReadError;
use crate::{Executor, Limits, os};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const READ_BUFFER: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn file_name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// The original request as accepted, plus what Core resolved for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestInfo {
    pub caller: String,
    pub request_id: String,
    pub executor: String,
    pub program: PathBuf,
    pub program_args: Vec<String>,
    /// As given by the caller (project-relative).
    pub workdir: PathBuf,
    /// Canonical directory checked at acceptance and used to start the process.
    pub workdir_resolved: PathBuf,
    pub code_sha256: String,
    pub code_bytes: usize,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    /// The process was no longer held but its status could not be read.
    Unknown(String),
}

/// What Core knows about the native process. Times are Core observation times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunStatus {
    /// Accepted; the process is being started.
    Starting,
    /// Accepted, and it is known that no process was started.
    NotStarted {
        reason: String,
    },
    Running {
        pid: u32,
        spawned_at: SystemTime,
    },
    /// The main process was reaped. `group_released` is true once no member of
    /// the run's process group remained; false means members were still present
    /// when Core stopped holding the run.
    Finished {
        pid: u32,
        spawned_at: SystemTime,
        exit: Exit,
        exit_observed_at: SystemTime,
        group_released: bool,
    },
}

impl RunStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::NotStarted { .. } | Self::Finished { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    Caller,
    Shutdown,
}

/// A stop request and the signals actually sent. Stop is confirmed only by
/// [`RunStatus::Finished`]; nothing is rolled back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelInfo {
    pub reason: CancelReason,
    pub requested_at: SystemTime,
    pub term_sent_at: Option<SystemTime>,
    pub kill_sent_at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamInfo {
    /// Bytes kept and readable through [`crate::Core::read_output`].
    pub retained_bytes: u64,
    /// Bytes the process wrote that Core read, retained or not.
    pub observed_bytes: u64,
    pub limit_bytes: u64,
    /// End of stream was observed.
    pub eof: bool,
    pub retention_error: Option<String>,
}

impl StreamInfo {
    pub fn truncated(&self) -> bool {
        self.observed_bytes > self.retained_bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunView {
    /// Unique within this Core instance; the pid is not an identity.
    pub run_id: String,
    pub request: RequestInfo,
    pub accepted_at: SystemTime,
    pub status: RunStatus,
    pub cancel: Option<CancelInfo>,
    pub stdout: StreamInfo,
    pub stderr: StreamInfo,
}

impl RunView {
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputChunk {
    pub stream: Stream,
    pub offset: u64,
    pub bytes: Vec<u8>,
    /// Stream state when the chunk was read.
    pub info: StreamInfo,
    pub run_terminal: bool,
}

pub(crate) struct RunShared {
    pub(crate) run_id: String,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) request: RequestInfo,
    accepted_at: SystemTime,
    dir: PathBuf,
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    status: RunStatus,
    cancel: Option<CancelInfo>,
    stdout: StreamInfo,
    stderr: StreamInfo,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl RunShared {
    pub(crate) fn new(
        run_id: String,
        fingerprint: [u8; 32],
        request: RequestInfo,
        dir: PathBuf,
        stream_limit: u64,
    ) -> Self {
        let stream = StreamInfo {
            retained_bytes: 0,
            observed_bytes: 0,
            limit_bytes: stream_limit,
            eof: false,
            retention_error: None,
        };
        Self {
            run_id,
            fingerprint,
            request,
            accepted_at: SystemTime::now(),
            dir,
            state: Mutex::new(State {
                status: RunStatus::Starting,
                cancel: None,
                stdout: stream.clone(),
                stderr: stream,
            }),
            changed: Condvar::new(),
        }
    }

    fn view_of(&self, state: &State) -> RunView {
        RunView {
            run_id: self.run_id.clone(),
            request: self.request.clone(),
            accepted_at: self.accepted_at,
            status: state.status.clone(),
            cancel: state.cancel.clone(),
            stdout: state.stdout.clone(),
            stderr: state.stderr.clone(),
        }
    }

    pub(crate) fn view(&self) -> RunView {
        self.view_of(&lock(&self.state))
    }

    pub(crate) fn wait(&self, timeout: Duration) -> RunView {
        let guard = lock(&self.state);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, timeout, |state| !state.status.is_terminal())
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.view_of(&guard)
    }

    pub(crate) fn request_cancel(&self, reason: CancelReason) -> RunView {
        let mut state = lock(&self.state);
        if !state.status.is_terminal() && state.cancel.is_none() {
            state.cancel = Some(CancelInfo {
                reason,
                requested_at: SystemTime::now(),
                term_sent_at: None,
                kill_sent_at: None,
            });
        }
        self.view_of(&state)
    }

    pub(crate) fn read(
        &self,
        stream: Stream,
        offset: u64,
        max_bytes: usize,
    ) -> Result<OutputChunk, ReadError> {
        let (info, run_terminal) = {
            let state = lock(&self.state);
            let info = match stream {
                Stream::Stdout => state.stdout.clone(),
                Stream::Stderr => state.stderr.clone(),
            };
            (info, state.status.is_terminal())
        };
        if offset > info.retained_bytes {
            return Err(ReadError::OffsetBeyondRetained {
                offset,
                retained: info.retained_bytes,
            });
        }
        let len = (info.retained_bytes - offset).min(max_bytes as u64) as usize;
        let mut bytes = vec![0; len];
        if len > 0 {
            let io_error = |error: io::Error| ReadError::Io(error.to_string());
            let mut file = File::open(self.dir.join(stream.file_name())).map_err(io_error)?;
            file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
            file.read_exact(&mut bytes).map_err(io_error)?;
        }
        Ok(OutputChunk {
            stream,
            offset,
            bytes,
            info,
            run_terminal,
        })
    }

    fn cancel_requested(&self) -> bool {
        lock(&self.state).cancel.is_some()
    }

    fn note_signal(&self, signal: i32) {
        let mut state = lock(&self.state);
        if let Some(cancel) = state.cancel.as_mut() {
            let now = Some(SystemTime::now());
            if signal == libc::SIGKILL {
                cancel.kill_sent_at = now;
            } else {
                cancel.term_sent_at = now;
            }
        }
    }

    fn set_running(&self, pid: u32) -> SystemTime {
        let spawned_at = SystemTime::now();
        lock(&self.state).status = RunStatus::Running { pid, spawned_at };
        spawned_at
    }

    fn set_streams(&self, stdout: &StreamInfo, stderr: &StreamInfo) {
        let mut state = lock(&self.state);
        state.stdout = stdout.clone();
        state.stderr = stderr.clone();
    }

    fn not_started(&self, reason: String, running: &AtomicUsize) {
        running.fetch_sub(1, Ordering::SeqCst);
        lock(&self.state).status = RunStatus::NotStarted { reason };
        self.changed.notify_all();
    }

    fn finish(&self, status: RunStatus, streams: (StreamInfo, StreamInfo), running: &AtomicUsize) {
        // Free the process slot before waiters can observe the terminal state.
        running.fetch_sub(1, Ordering::SeqCst);
        let mut state = lock(&self.state);
        state.status = status;
        (state.stdout, state.stderr) = streams;
        self.changed.notify_all();
    }
}

/// Starts the accepted run and hands its process to a holder thread. Every
/// outcome ends in a terminal status that releases the reserved process slot.
pub(crate) fn dispatch(
    run: &Arc<RunShared>,
    executor: &Executor,
    code: &str,
    args: &[String],
    limits: &Limits,
    running: &Arc<AtomicUsize>,
) {
    let (script, stdout_file, stderr_file) = match prepare(&run.dir, code) {
        Ok(files) => files,
        Err(error) => {
            let reason = format!(
                "run files could not be prepared in `{}`: {error}",
                run.dir.display()
            );
            return run.not_started(reason, running);
        }
    };
    if run.cancel_requested() {
        return run.not_started(
            "cancel was requested before the process started".into(),
            running,
        );
    }
    let mut command = Command::new(&executor.program);
    command
        .args(&executor.args)
        .arg(&script)
        .args(args)
        .current_dir(&run.request.workdir_resolved)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return run.not_started(format!("process could not be started: {error}"), running);
        }
    };
    let pid = child.id();
    let spawned_at = run.set_running(pid);
    let limit = limits.max_stream_bytes;
    let pumps = [
        Pump::new(child.stdout.take().map(OwnedFd::from), stdout_file, limit),
        Pump::new(child.stderr.take().map(OwnedFd::from), stderr_file, limit),
    ];
    let holder = Holder {
        run: Arc::clone(run),
        child,
        pid,
        spawned_at,
        pumps,
        limits: limits.clone(),
        running: Arc::clone(running),
    };
    let held = thread::Builder::new()
        .name(format!("rho-run-{}", run.run_id))
        .spawn(move || holder.hold());
    if let Err(error) = held {
        // The holder and its Child were dropped unreaped, so the group id is still reserved.
        let pgid = pid as i32;
        let _ = os::signal_group(pgid, libc::SIGKILL);
        let exit = match os::reap(pgid) {
            Ok(raw) => exit_of(ExitStatus::from_raw(raw)),
            Err(error) => Exit::Unknown(error.to_string()),
        };
        let mut streams = {
            let state = lock(&run.state);
            (state.stdout.clone(), state.stderr.clone())
        };
        let note = format!("output not retained: no thread could hold this run: {error}");
        streams.0.retention_error = Some(note.clone());
        streams.1.retention_error = Some(note);
        let status = RunStatus::Finished {
            pid,
            spawned_at,
            exit,
            exit_observed_at: SystemTime::now(),
            group_released: !os::group_exists(pgid),
        };
        run.finish(status, streams, running);
    }
}

fn prepare(dir: &Path, code: &str) -> io::Result<(PathBuf, File, File)> {
    fs::create_dir(dir)?;
    let create = |name: &str| {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join(name))
    };
    let script = dir.join("script");
    create("script")?.write_all(code.as_bytes())?;
    Ok((script, create("stdout")?, create("stderr")?))
}

fn exit_of(status: ExitStatus) -> Exit {
    match (status.code(), status.signal()) {
        (Some(code), _) => Exit::Code(code),
        (None, Some(signal)) => Exit::Signal(signal),
        _ => Exit::Unknown(status.to_string()),
    }
}

/// Owns the child process: the only place that signals or reaps it.
struct Holder {
    run: Arc<RunShared>,
    child: Child,
    pid: u32,
    spawned_at: SystemTime,
    pumps: [Pump; 2],
    limits: Limits,
    running: Arc<AtomicUsize>,
}

impl Holder {
    fn hold(mut self) {
        let pgid = self.pid as i32;
        let mut buffer = vec![0; READ_BUFFER];
        let mut term_sent: Option<Instant> = None;
        let mut kill_sent = false;
        let mut exited: Option<(Exit, SystemTime)> = None;
        let mut close_deadline: Option<Instant> = None;
        let mut group_released = false;
        loop {
            let timeout = close_deadline.map_or(POLL_INTERVAL, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(POLL_INTERVAL)
            });
            if self.pump(&mut buffer, timeout) {
                self.run
                    .set_streams(&self.pumps[0].info, &self.pumps[1].info);
            }
            if let Some(deadline) = close_deadline {
                group_released = group_released || !os::group_exists(pgid);
                let closed = self.pumps.iter().all(Pump::closed);
                if (closed && group_released) || Instant::now() >= deadline {
                    break;
                }
                continue;
            }
            if self.run.cancel_requested() {
                match term_sent {
                    None => {
                        let _ = os::signal_group(pgid, libc::SIGTERM);
                        self.run.note_signal(libc::SIGTERM);
                        term_sent = Some(Instant::now());
                    }
                    Some(at) if !kill_sent && at.elapsed() >= self.limits.stop_grace => {
                        let _ = os::signal_group(pgid, libc::SIGKILL);
                        self.run.note_signal(libc::SIGKILL);
                        kill_sent = true;
                    }
                    Some(_) => {}
                }
            }
            if let Some(exit) = self.observe_exit() {
                exited = Some((exit, SystemTime::now()));
                close_deadline = Some(Instant::now() + self.limits.output_close_grace);
            }
        }
        let (exit, exit_observed_at) =
            exited.expect("holding ends only after the exit is observed");
        let status = RunStatus::Finished {
            pid: self.pid,
            spawned_at: self.spawned_at,
            exit,
            exit_observed_at,
            group_released,
        };
        let streams = (self.pumps[0].info.clone(), self.pumps[1].info.clone());
        self.run.finish(status, streams, &self.running);
    }

    /// Observes the main process's exit; if it exited, releases the rest of its
    /// process group while the unreaped leader still reserves the group id, then reaps.
    fn observe_exit(&mut self) -> Option<Exit> {
        let pgid = self.pid as i32;
        match os::has_exited(pgid) {
            Ok(false) => None,
            Ok(true) => {
                let _ = os::signal_group(pgid, libc::SIGKILL);
                Some(match self.child.wait() {
                    Ok(status) => exit_of(status),
                    Err(error) => Exit::Unknown(error.to_string()),
                })
            }
            Err(_) => match self.child.try_wait() {
                Ok(Some(status)) => Some(exit_of(status)),
                Ok(None) => None,
                Err(error) => Some(Exit::Unknown(error.to_string())),
            },
        }
    }

    /// Reads whatever output is ready, waiting at most `timeout`. Returns
    /// whether stream state changed.
    fn pump(&mut self, buffer: &mut [u8], timeout: Duration) -> bool {
        let open: Vec<usize> = (0..self.pumps.len())
            .filter(|&index| !self.pumps[index].closed())
            .collect();
        let fds: Vec<RawFd> = open
            .iter()
            .filter_map(|&index| self.pumps[index].fd())
            .collect();
        let ready = match os::poll_readable(&fds, timeout) {
            Ok(ready) => ready,
            Err(_) => {
                thread::sleep(timeout);
                return false;
            }
        };
        let mut changed = false;
        for (slot, &index) in open.iter().enumerate() {
            if ready[slot] {
                changed |= self.pumps[index].read_once(buffer);
            }
        }
        changed
    }
}

/// Copies one output pipe into its retained file, up to the stream limit.
struct Pump {
    pipe: Option<File>,
    file: File,
    info: StreamInfo,
}

impl Pump {
    fn new(pipe: Option<OwnedFd>, file: File, limit: u64) -> Self {
        Self {
            pipe: pipe.map(File::from),
            file,
            info: StreamInfo {
                retained_bytes: 0,
                observed_bytes: 0,
                limit_bytes: limit,
                eof: false,
                retention_error: None,
            },
        }
    }

    fn closed(&self) -> bool {
        self.pipe.is_none()
    }

    fn fd(&self) -> Option<RawFd> {
        self.pipe.as_ref().map(AsRawFd::as_raw_fd)
    }

    fn read_once(&mut self, buffer: &mut [u8]) -> bool {
        let Some(pipe) = self.pipe.as_mut() else {
            return false;
        };
        match pipe.read(buffer) {
            Ok(0) => {
                self.info.eof = true;
                self.pipe = None;
            }
            Ok(read) => {
                self.info.observed_bytes += read as u64;
                let room = self
                    .info
                    .limit_bytes
                    .saturating_sub(self.info.retained_bytes);
                let keep = (read as u64).min(room) as usize;
                if keep > 0 && self.info.retention_error.is_none() {
                    match self.file.write_all(&buffer[..keep]) {
                        Ok(()) => self.info.retained_bytes += keep as u64,
                        Err(error) => {
                            self.info.retention_error = Some(format!(
                                "retaining output failed after {} bytes: {error}",
                                self.info.retained_bytes
                            ));
                        }
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                return false;
            }
            Err(error) => {
                self.info
                    .retention_error
                    .get_or_insert_with(|| format!("reading output failed: {error}"));
                self.pipe = None;
            }
        }
        true
    }
}
