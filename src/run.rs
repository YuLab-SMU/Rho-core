//! One accepted run: its durable record, the thread holding its process, and
//! retained output.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::error::{LookupError, ReadError};
use crate::record::*;
use crate::store::{self, Fact, OperationStore, Stored};
use crate::{Executor, Limits, fault, os};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const READ_BUFFER: usize = store::BLOB_CHUNK;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn kind(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// The original request as accepted, plus what Core resolved for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Exit {
    Code(i32),
    Signal(i32),
    /// The process was no longer held but its status could not be read.
    Unknown(String),
}

/// What Core knows about the native process. Times are Core observation times.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Dispatched by an earlier Core instance that stopped before recording an
    /// outcome. No Core holds it: it is not signalled, reaped or stopped. `pid`
    /// and `spawned_at` are `None` when that instance stopped between recording
    /// the dispatch and recording the spawn, so whether a process started is
    /// unknown. `present` is whether a process of this run still held the run
    /// lock at `observed_at`; the exit status is not known either way.
    Detached {
        dispatched_at: SystemTime,
        pid: Option<u32>,
        spawned_at: Option<SystemTime>,
        present: bool,
        observed_at: SystemTime,
    },
}

impl RunStatus {
    /// The selected native execution has a known end. RunView also checks the
    /// current holder; retention separately requires known resource release.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::NotStarted { .. } | Self::Finished { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelReason {
    Caller,
    Shutdown,
}

/// A stop request and the signals actually sent. Stop is confirmed only by
/// [`RunStatus::Finished`]; nothing is rolled back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelInfo {
    pub reason: CancelReason,
    pub requested_at: SystemTime,
    pub term_sent_at: Option<SystemTime>,
    pub kill_sent_at: Option<SystemTime>,
    /// A failed signal syscall is not reported as a successfully sent signal.
    pub signal_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// A known fact that could not be saved yet. Saving is retried on later
    /// reads; until then a restart would not see that fact.
    pub unsaved: Option<String>,
    pub operation: OperationRecord,
    /// This Core still owns the native process or is draining its output.
    pub held: bool,
}

impl RunView {
    pub fn is_terminal(&self) -> bool {
        !self.held && self.status.is_terminal()
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
    pub(crate) fingerprint: String,
    pub(crate) request: RequestInfo,
    pub(crate) dir: PathBuf,
    store: Arc<OperationStore>,
    chunk_limit: usize,
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    record: OperationRecord,
    held: bool,
    pending: Vec<Fact>,
    unsaved: Option<String>,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn mark(state: &mut State, fact: Fact) {
    if !state.pending.contains(&fact) {
        state.pending.push(fact);
    }
}

pub(crate) fn empty_stream(limit_bytes: u64) -> StreamInfo {
    StreamInfo {
        retained_bytes: 0,
        observed_bytes: 0,
        limit_bytes,
        eof: false,
        retention_error: None,
    }
}

impl RunShared {
    pub(crate) fn new(stored: Stored, store: Arc<OperationStore>, held: bool) -> Self {
        let a = stored.accepted;
        let mut state = State {
            record: stored.record,
            held,
            pending: Vec::new(),
            unsaved: None,
        };
        // Only a durable NotAttempted fact authorizes the conclusion that no
        // process was started. No recovered record is ever dispatched.
        if !held
            && state.record.dispatch.value() == Some(&DispatchFact::NotAttempted)
            && state.record.execution.value().is_none()
        {
            state.record.execution = Knowledge::known(ExecutionFact::NotStarted {
                reason: "the Core instance stopped before dispatching this request; no process was started".into()
            }, FactSource::StoreRecovery);
            state.record.group_released = Knowledge::known(true, FactSource::StoreRecovery);
            mark(&mut state, Fact::Execution);
            mark(&mut state, Fact::Release);
        }
        if !held
            && !state.record.removable()
            && state.record.dispatch.value() == Some(&DispatchFact::Attempted)
        {
            if !state.record.outputs.stdout.info.eof
                && state.record.outputs.stdout.info.retention_error.is_none()
            {
                state.record.outputs.stdout.info.retention_error =
                    Some("the previous holder stopped; later output cannot be retained".into());
                state.record.outputs.stdout.observed_at = SystemTime::now();
                state.record.outputs.stdout.source = FactSource::StoreRecovery;
                mark(&mut state, Fact::Stdout);
            }
            if !state.record.outputs.stderr.info.eof
                && state.record.outputs.stderr.info.retention_error.is_none()
            {
                state.record.outputs.stderr.info.retention_error =
                    Some("the previous holder stopped; later output cannot be retained".into());
                state.record.outputs.stderr.observed_at = SystemTime::now();
                state.record.outputs.stderr.source = FactSource::StoreRecovery;
                mark(&mut state, Fact::Stderr);
            }
        }
        let run = Self {
            run_id: a.run_id.clone(),
            fingerprint: a.fingerprint,
            request: a.request,
            dir: store.run_dir(&a.run_id),
            chunk_limit: a.chunk_limit,
            store,
            state: Mutex::new(state),
            changed: Condvar::new(),
        };
        let mut state = lock(&run.state);
        run.flush(&mut state);
        drop(state);
        run
    }

    fn status(state: &State) -> RunStatus {
        match &state.record.execution {
            Knowledge::Known {
                value: ExecutionFact::NotStarted { reason },
                ..
            } => RunStatus::NotStarted {
                reason: reason.clone(),
            },
            Knowledge::Known {
                value:
                    ExecutionFact::ExitObserved {
                        pid,
                        spawned_at,
                        exit,
                    },
                observed_at,
                ..
            } => RunStatus::Finished {
                pid: *pid,
                spawned_at: *spawned_at,
                exit: exit.clone(),
                exit_observed_at: *observed_at,
                group_released: state.record.group_released.value() == Some(&true),
            },
            Knowledge::Known {
                value: ExecutionFact::SpawnObserved { pid, spawned_at },
                ..
            } if state.held => RunStatus::Running {
                pid: *pid,
                spawned_at: *spawned_at,
            },
            _ if state.held => RunStatus::Starting,
            _ => {
                let (pid, spawned_at) = match state.record.execution.value() {
                    Some(ExecutionFact::SpawnObserved { pid, spawned_at }) => {
                        (Some(*pid), Some(*spawned_at))
                    }
                    _ => (None, None),
                };
                let at = match state.record.dispatch {
                    Knowledge::Known { observed_at, .. } => observed_at,
                    _ => state.record.accepted_at,
                };
                let observed_at = match state.record.run_lock {
                    Knowledge::Known { observed_at, .. } => observed_at,
                    _ => state.record.accepted_at,
                };
                RunStatus::Detached {
                    dispatched_at: at,
                    pid,
                    spawned_at,
                    present: state.record.run_lock.value() == Some(&true),
                    observed_at,
                }
            }
        }
    }

    fn view_of(&self, state: &State) -> RunView {
        let mut operation = state.record.clone();
        operation.retention(state.held, !state.pending.is_empty());
        RunView {
            run_id: self.run_id.clone(),
            request: self.request.clone(),
            accepted_at: operation.accepted_at,
            status: Self::status(state),
            cancel: operation.cancellation.clone(),
            stdout: operation.outputs.stdout.info.clone(),
            stderr: operation.outputs.stderr.info.clone(),
            unsaved: state.unsaved.clone(),
            operation,
            held: state.held,
        }
    }

    pub(crate) fn view(&self) -> Result<RunView, LookupError> {
        let mut state = lock(&self.state);
        self.refresh(&mut state)?;
        Ok(self.view_of(&state))
    }

    pub(crate) fn wait(&self, timeout: Duration) -> Result<RunView, LookupError> {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        loop {
            self.refresh(&mut state)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if (!state.held && Self::status(&state).is_terminal()) || remaining.is_zero() {
                return Ok(self.view_of(&state));
            }
            // Detached observations have no holder to send a notification.
            let slice = if state.held {
                remaining
            } else {
                remaining.min(POLL_INTERVAL)
            };
            state = self
                .changed
                .wait_timeout(state, slice)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    pub(crate) fn request_cancel(&self, reason: CancelReason) -> Result<RunView, LookupError> {
        let mut state = lock(&self.state);
        self.refresh(&mut state)?;
        if state.held
            && !matches!(
                state.record.execution.value(),
                Some(ExecutionFact::ExitObserved { .. })
            )
            && state.record.cancellation.is_none()
        {
            state.record.cancellation = Some(CancelInfo {
                reason,
                requested_at: SystemTime::now(),
                term_sent_at: None,
                kill_sent_at: None,
                signal_error: None,
            });
            mark(&mut state, Fact::Cancel);
            self.flush(&mut state);
            fault::point("cancel-saved");
        }
        Ok(self.view_of(&state))
    }

    /// Shutdown must still stop owned native resources when their store is damaged.
    pub(crate) fn shutdown_stop(&self) -> RunView {
        let mut state = lock(&self.state);
        if state.held
            && !matches!(
                state.record.execution.value(),
                Some(ExecutionFact::ExitObserved { .. })
            )
            && state.record.cancellation.is_none()
        {
            state.record.cancellation = Some(CancelInfo {
                reason: CancelReason::Shutdown,
                requested_at: SystemTime::now(),
                term_sent_at: None,
                kill_sent_at: None,
                signal_error: None,
            });
            mark(&mut state, Fact::Cancel);
            self.flush(&mut state);
        }
        self.view_of(&state)
    }

    /// Wait on the actually owned holder even when durable queries fail.
    pub(crate) fn wait_held(&self, timeout: Duration) -> RunView {
        let deadline = Instant::now() + timeout;
        let mut state = lock(&self.state);
        while state.held {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
        self.view_of(&state)
    }

    pub(crate) fn read(
        &self,
        stream: Stream,
        offset: u64,
        max_bytes: usize,
    ) -> Result<OutputChunk, ReadError> {
        let mut state = lock(&self.state);
        self.refresh(&mut state)?;
        let output = match stream {
            Stream::Stdout => &state.record.outputs.stdout,
            Stream::Stderr => &state.record.outputs.stderr,
        };
        if offset > output.info.retained_bytes {
            return Err(ReadError::OffsetBeyondRetained {
                offset,
                retained: output.info.retained_bytes,
            });
        }
        let end = offset
            .saturating_add(max_bytes as u64)
            .min(output.info.retained_bytes);
        let mut bytes = Vec::with_capacity((end - offset) as usize);
        let mut at = 0;
        for blob in &output.blobs {
            let blob_end = at + blob.bytes;
            if at < end && blob_end > offset {
                let data = self
                    .store
                    .read_blob(blob)
                    .map_err(|e| ReadError::Io(e.to_string()))?;
                let from = offset.saturating_sub(at) as usize;
                let to = (end - at).min(blob.bytes) as usize;
                bytes.extend_from_slice(&data[from..to]);
            }
            at = blob_end;
            if at >= end {
                break;
            }
        }
        Ok(OutputChunk {
            stream,
            offset,
            bytes,
            info: output.info.clone(),
            run_terminal: !state.held && Self::status(&state).is_terminal(),
        })
    }

    fn refresh(&self, state: &mut State) -> Result<(), LookupError> {
        // Always check the durable row: a damaged/missing record cannot be
        // silently hidden by a cache, even during a running process.
        let stored = self
            .store
            .get(&self.request.caller, &self.request.request_id)
            .map_err(lookup_error)?
            .ok_or_else(|| LookupError::RecordUnreadable {
                reason: "accepted record disappeared; execution may already have happened".into(),
            })?;
        if stored.accepted.run_id != self.run_id
            || stored.accepted.fingerprint != self.fingerprint
            || stored.accepted.request != self.request
        {
            return Err(LookupError::RecordUnreadable {
                reason: "immutable acceptance changed".into(),
            });
        }
        self.flush(state);
        if !state.held
            && matches!(
                state.record.execution.value(),
                None | Some(ExecutionFact::SpawnObserved { .. })
            )
            && state.record.dispatch.value() == Some(&DispatchFact::Attempted)
        {
            state.record.run_lock = match probe(&self.dir) {
                Ok(present) => Knowledge::known(present, FactSource::RunLock),
                Err(error) => Knowledge::unknown(
                    format!("run lock cannot be observed: {error}"),
                    SystemTime::now(),
                ),
            };
        }
        Ok(())
    }

    fn flush(&self, state: &mut State) {
        let mut errors = Vec::new();
        for fact in std::mem::take(&mut state.pending) {
            match self.store.save(&state.record, fact) {
                Ok(revision) => state.record.revision = revision,
                Err(error) => {
                    errors.push(format!("{} could not be saved: {error}", fact.column()));
                    state.pending.push(fact);
                }
            }
        }
        state.unsaved = (!errors.is_empty()).then(|| errors.join("; "));
    }

    fn begin_dispatch(&self) -> Result<(), String> {
        let mut state = lock(&self.state);
        state.record.dispatch = Knowledge::known(DispatchFact::Attempted, FactSource::Core);
        // A failure here never calls the executor, even if the commit's reply
        // was lost. A later submit reads the acceptance and never replays it.
        mark(&mut state, Fact::Dispatch);
        self.flush(&mut state);
        if state.pending.contains(&Fact::Dispatch) {
            Err(state
                .unsaved
                .clone()
                .unwrap_or_else(|| "dispatch marker was not saved".into()))
        } else {
            Ok(())
        }
    }

    fn cancel_requested(&self) -> bool {
        lock(&self.state).record.cancellation.is_some()
    }

    fn note_signal(&self, signal: i32, result: io::Result<()>) {
        let mut state = lock(&self.state);
        if let Some(cancel) = state.record.cancellation.as_mut() {
            match result {
                Ok(()) => {
                    if signal == libc::SIGKILL {
                        cancel.kill_sent_at.get_or_insert_with(SystemTime::now);
                    } else {
                        cancel.term_sent_at.get_or_insert_with(SystemTime::now);
                    }
                }
                Err(error) => {
                    cancel.signal_error = Some(format!("signal {signal} failed: {error}"));
                }
            }
            mark(&mut state, Fact::Cancel);
            self.flush(&mut state);
        }
    }

    pub(crate) fn held(&self) -> bool {
        lock(&self.state).held
    }

    fn set_running(&self, pid: u32) -> SystemTime {
        let spawned_at = SystemTime::now();
        let mut state = lock(&self.state);
        state.record.execution = Knowledge::Known {
            value: ExecutionFact::SpawnObserved { pid, spawned_at },
            observed_at: spawned_at,
            source: FactSource::NativeProcess,
        };
        mark(&mut state, Fact::Execution);
        fault::point("spawned");
        self.flush(&mut state);
        spawned_at
    }

    fn record_output(
        &self,
        stream: Stream,
        data: &[u8],
        eof: bool,
        error: Option<String>,
    ) -> StreamInfo {
        let mut state = lock(&self.state);
        let output = match stream {
            Stream::Stdout => &mut state.record.outputs.stdout,
            Stream::Stderr => &mut state.record.outputs.stderr,
        };
        output.observed_at = SystemTime::now();
        output.source = FactSource::NativeProcess;
        output.info.observed_bytes = output.info.observed_bytes.saturating_add(data.len() as u64);
        output.info.eof |= eof;
        if let Some(error) = error {
            output.info.retention_error.get_or_insert(error);
        }
        let keep = (output
            .info
            .limit_bytes
            .saturating_sub(output.info.retained_bytes))
        .min(data.len() as u64) as usize;
        if keep > 0 && output.info.retention_error.is_none() {
            if output.blobs.len() >= self.chunk_limit {
                output.info.retention_error = Some(format!(
                    "output artifact chunk limit {} reached; further bytes are counted but not retained",
                    self.chunk_limit
                ));
            } else {
                match self.store.blob(&self.run_id, stream.kind(), &data[..keep]) {
                    Ok(blob) => {
                        output.info.retained_bytes += blob.bytes;
                        output.blobs.push(blob);
                    }
                    Err(error) => {
                        output.info.retention_error =
                            Some(format!("retaining output failed: {error}"));
                    }
                }
            }
        }
        let info = output.info.clone();
        mark(
            &mut state,
            match stream {
                Stream::Stdout => Fact::Stdout,
                Stream::Stderr => Fact::Stderr,
            },
        );
        self.flush(&mut state);
        info
    }

    fn not_started(&self, reason: String, running: &AtomicUsize) {
        running.fetch_sub(1, Ordering::SeqCst);
        let mut state = lock(&self.state);
        state.held = false;
        state.record.execution =
            Knowledge::known(ExecutionFact::NotStarted { reason }, FactSource::Core);
        state.record.group_released = Knowledge::known(true, FactSource::Core);
        mark(&mut state, Fact::Execution);
        mark(&mut state, Fact::Release);
        self.flush(&mut state);
        self.changed.notify_all();
    }

    fn observe_exit(&self, pid: u32, spawned_at: SystemTime, exit: Exit) {
        let mut state = lock(&self.state);
        state.record.execution = Knowledge::known(
            ExecutionFact::ExitObserved {
                pid,
                spawned_at,
                exit,
            },
            FactSource::NativeProcess,
        );
        mark(&mut state, Fact::Execution);
        fault::point("exited");
        self.flush(&mut state);
        fault::point("exit-saved");
        self.changed.notify_all();
    }

    fn finish(&self, released: bool, running: &AtomicUsize) {
        running.fetch_sub(1, Ordering::SeqCst);
        let mut state = lock(&self.state);
        state.held = false;
        state.record.group_released = Knowledge::known(released, FactSource::NativeProcess);
        mark(&mut state, Fact::Release);
        self.flush(&mut state);
        self.changed.notify_all();
    }
}

pub(crate) fn lookup_error(error: store::StoreError) -> LookupError {
    match error {
        store::StoreError::Unreadable(reason) => LookupError::RecordUnreadable { reason },
        error => LookupError::Storage(error.to_string()),
    }
}

fn probe(dir: &Path) -> io::Result<bool> {
    if !fs::symlink_metadata(dir)?.file_type().is_dir() {
        return Err(io::Error::other("run lock directory was replaced"));
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(store::RUN_LOCK))?;
    Ok(!store::try_lock(&file)?)
}

pub(crate) fn dispatch(
    run: &Arc<RunShared>,
    executor: &Executor,
    args: &[String],
    limits: &Limits,
    running: &Arc<AtomicUsize>,
) {
    let prepared = (|| -> Result<File, String> {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&run.dir)
            .map_err(|e| e.to_string())?;
        let lock_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(run.dir.join(store::RUN_LOCK))
            .map_err(|e| e.to_string())?;
        if !store::try_lock(&lock_file).map_err(|e| e.to_string())? {
            return Err("another process holds this run's lock".into());
        }
        let script = lock(&run.state).record.script.clone();
        run.store.verify_blob(&script).map_err(|e| e.to_string())?;
        run.begin_dispatch()?;
        Ok(lock_file)
    })();
    let lock_file = match prepared {
        Ok(file) => file,
        Err(error) => {
            return run.not_started(format!("run files could not be prepared: {error}"), running);
        }
    };
    fault::point("dispatched");
    if run.cancel_requested() {
        return run.not_started(
            "cancel was requested before the process started".into(),
            running,
        );
    }
    let stdin = match lock_file.try_clone() {
        Ok(file) => file,
        Err(error) => {
            return run.not_started(format!("run lock could not be shared: {error}"), running);
        }
    };
    let script = lock(&run.state).record.script.clone();
    let script_path = match run.store.blob_path(&script) {
        Ok(path) => path,
        Err(error) => {
            return run.not_started(
                format!("script snapshot could not be resolved: {error}"),
                running,
            );
        }
    };
    let mut command = Command::new(&run.request.program);
    command
        .args(&executor.args)
        .arg(script_path)
        .args(args)
        .current_dir(&run.request.workdir_resolved)
        .stdin(Stdio::from(stdin))
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
    let pumps = [
        Pump::new(
            child.stdout.take().map(OwnedFd::from),
            Arc::clone(run),
            Stream::Stdout,
            limits.max_stream_bytes,
        ),
        Pump::new(
            child.stderr.take().map(OwnedFd::from),
            Arc::clone(run),
            Stream::Stderr,
            limits.max_stream_bytes,
        ),
    ];
    let holder = Holder {
        run: Arc::clone(run),
        child,
        pid,
        spawned_at,
        pumps,
        limits: limits.clone(),
        running: Arc::clone(running),
        lock: Some(lock_file),
    };
    let held = thread::Builder::new()
        .name(format!("rho-run-{}", run.run_id))
        .spawn(move || holder.hold());
    if let Err(error) = held {
        let pgid = pid as i32;
        let _ = os::signal_group(pgid, libc::SIGKILL);
        let exit = match os::reap(pgid) {
            Ok(raw) => exit_of(ExitStatus::from_raw(raw)),
            Err(error) => Exit::Unknown(error.to_string()),
        };
        run.observe_exit(pid, spawned_at, exit);
        for stream in [Stream::Stdout, Stream::Stderr] {
            run.record_output(
                stream,
                &[],
                false,
                Some(format!("output not retained: no holder thread: {error}")),
            );
        }
        run.finish(!os::group_exists(pgid), running);
    }
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
    /// Shared with the process group; releasing this is not a native exit fact.
    lock: Option<File>,
}

impl Holder {
    fn hold(mut self) {
        let pgid = self.pid as i32;
        let mut buffer = vec![0; READ_BUFFER];
        let mut term_sent: Option<Instant> = None;
        let mut kill_sent = false;
        let mut close_deadline: Option<Instant> = None;
        let mut group_released = false;
        loop {
            let timeout = close_deadline.map_or(POLL_INTERVAL, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(POLL_INTERVAL)
            });
            self.pump(&mut buffer, timeout);
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
                        let result = os::signal_group(pgid, libc::SIGTERM);
                        self.run.note_signal(libc::SIGTERM, result);
                        term_sent = Some(Instant::now());
                    }
                    Some(at) if !kill_sent && at.elapsed() >= self.limits.stop_grace => {
                        let result = os::signal_group(pgid, libc::SIGKILL);
                        self.run.note_signal(libc::SIGKILL, result);
                        kill_sent = true;
                    }
                    Some(_) => {}
                }
            }
            if let Some(exit) = self.observe_exit() {
                self.run.observe_exit(self.pid, self.spawned_at, exit);
                close_deadline = Some(Instant::now() + self.limits.output_close_grace);
            }
        }
        for pump in &mut self.pumps {
            if !pump.closed() {
                pump.info = self.run.record_output(
                    pump.stream,
                    &[],
                    false,
                    Some("output close grace exceeded; later bytes are not retained".into()),
                );
                pump.pipe = None;
            }
        }
        drop(self.lock.take());
        self.run.finish(group_released, &self.running);
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

/// Drains one native pipe. Retained bytes and counters are committed separately
/// from the exit, cancellation and release dimensions.
struct Pump {
    pipe: Option<File>,
    run: Arc<RunShared>,
    stream: Stream,
    info: StreamInfo,
}

impl Pump {
    fn new(pipe: Option<OwnedFd>, run: Arc<RunShared>, stream: Stream, limit: u64) -> Self {
        Self {
            pipe: pipe.map(File::from),
            run,
            stream,
            info: empty_stream(limit),
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
                self.pipe = None;
                self.info = self.run.record_output(self.stream, &[], true, None);
            }
            Ok(read) => {
                self.info = self
                    .run
                    .record_output(self.stream, &buffer[..read], false, None);
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
                self.pipe = None;
                self.info = self.run.record_output(
                    self.stream,
                    &[],
                    false,
                    Some(format!("reading output failed: {error}")),
                );
            }
        }
        true
    }
}
