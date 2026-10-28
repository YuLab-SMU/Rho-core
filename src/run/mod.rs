//! One accepted run: its durable record, the thread holding its process, and
//! retained output. Dispatch and holding are per platform (`platform`).

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::error::{LookupError, ReadError};
use crate::record::*;
use crate::store::{self, Fact, OperationStore, Stored};
use crate::{Limits, fault, os};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

pub(crate) mod platform {
    //! The dispatch and holding of one native process, per platform. Both
    //! modules implement the same contract: `dispatch` prepares the run dir,
    //! records the dispatch fact, starts the executor with the run lock as its
    //! stdin and piped output, and spawns a holder thread that stops cancelled
    //! runs, retains output, observes the exit and reports release.
    #[cfg(unix)]
    pub(crate) mod unix_impl;
    #[cfg(windows)]
    pub(crate) mod windows_impl;
    #[cfg(unix)]
    pub(crate) use unix_impl::dispatch;
    #[cfg(windows)]
    pub(crate) use windows_impl::dispatch;
}

pub(crate) use platform::dispatch;

pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(20);

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
    /// No Core held the run when its main process ended (with `pid: None`,
    /// possibly before it started); `observed_at` is when the absence was
    /// observed, not when it ended. The exit status is unknown. `group_released`
    /// is true once its process group was also observed to be empty.
    Gone {
        pid: Option<u32>,
        spawned_at: Option<SystemTime>,
        evidence: AbsenceEvidence,
        observed_at: SystemTime,
        group_released: bool,
    },
}

impl RunStatus {
    /// The selected native execution has a known end. RunView also checks the
    /// current holder; retention separately requires known resource release.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::NotStarted { .. } | Self::Finished { .. } | Self::Gone { .. }
        )
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
            && state
                .record
                .dispatch
                .value()
                .is_some_and(DispatchFact::attempted)
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
                value:
                    ExecutionFact::Gone {
                        pid,
                        spawned_at,
                        evidence,
                    },
                observed_at,
                ..
            } => RunStatus::Gone {
                pid: *pid,
                spawned_at: *spawned_at,
                evidence: evidence.clone(),
                observed_at: *observed_at,
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
        if state.held
            || !state
                .record
                .dispatch
                .value()
                .is_some_and(DispatchFact::attempted)
        {
            return Ok(());
        }
        if matches!(
            state.record.execution.value(),
            None | Some(ExecutionFact::SpawnObserved { .. })
        ) {
            state.record.run_lock = match run_present(state, &self.dir) {
                Some(present) => Knowledge::known(present, FactSource::RunLock),
                None => {
                    Knowledge::unknown("run lock cannot be observed".to_string(), SystemTime::now())
                }
            };
        }
        self.observe_absence(state);
        Ok(())
    }

    /// For a dispatched run no Core holds, records only what the OS shows to be
    /// gone: the main process, its process group, or the whole boot session.
    /// Pids are compared only within the same boot identity, scope and pid
    /// namespace; nothing is signalled and no exit status is inferred.
    fn observe_absence(&self, state: &mut State) {
        if state.record.removable() {
            return;
        }
        let Some(DispatchFact::Attempted {
            context: Some(dispatched),
        }) = state.record.dispatch.value()
        else {
            return;
        };
        let Ok(current) = os::process_context() else {
            return;
        };
        let execution = state.record.execution.value();
        let released = state.record.group_released.value() == Some(&true);
        let mut gone = None;
        let mut release = None;
        if kernel_boot_changed(dispatched, &current) {
            if !execution.is_some_and(|fact| fact.rank() == 2) {
                let (pid, spawned_at) = match execution {
                    Some(ExecutionFact::SpawnObserved { pid, spawned_at }) => {
                        (Some(*pid), Some(*spawned_at))
                    }
                    _ => (None, None),
                };
                let evidence = AbsenceEvidence::BootChanged {
                    dispatched: dispatched.boot_id.clone(),
                    current: current.boot_id,
                };
                gone = Some((pid, spawned_at, evidence, FactSource::BootSession));
            }
            if !released {
                release = Some(FactSource::BootSession);
            }
        } else if dispatched == &current {
            if let Some(ExecutionFact::SpawnObserved { pid, spawned_at }) = execution
                && os::process_absent(*pid)
            {
                let evidence = AbsenceEvidence::MainPidAbsent;
                gone = Some((
                    Some(*pid),
                    Some(*spawned_at),
                    evidence,
                    FactSource::ProcessTable,
                ));
            }
            // The group identity (pgid or job object) is not reused while any
            // member remains.
            if !released
                && let Some(pgid) = execution.and_then(ExecutionFact::pid)
                && os::group_absent(&self.run_id, pgid)
            {
                release = Some(FactSource::ProcessTable);
            }
        }
        if let Some((pid, spawned_at, evidence, source)) = gone {
            state.record.execution = Knowledge::known(
                ExecutionFact::Gone {
                    pid,
                    spawned_at,
                    evidence,
                },
                source,
            );
            mark(state, Fact::Execution);
        }
        if let Some(source) = release {
            state.record.group_released = Knowledge::known(true, source);
            mark(state, Fact::Release);
        }
        if !state.pending.is_empty() {
            self.flush(state);
        }
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
        let context = os::process_context().ok();
        state.record.dispatch =
            Knowledge::known(DispatchFact::Attempted { context }, FactSource::Core);
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

    pub(crate) fn cancel_requested(&self) -> bool {
        lock(&self.state).record.cancellation.is_some()
    }

    /// Records that a stop reached the run. `kill` selects the second stop
    /// (the Unix SIGKILL / the Windows second terminate) for the timestamp.
    pub(crate) fn note_signal(&self, kill: bool, result: io::Result<()>) {
        let mut state = lock(&self.state);
        if let Some(cancel) = state.record.cancellation.as_mut() {
            match result {
                Ok(()) => {
                    if kill {
                        cancel.kill_sent_at.get_or_insert_with(SystemTime::now);
                    } else {
                        cancel.term_sent_at.get_or_insert_with(SystemTime::now);
                    }
                }
                Err(error) => {
                    cancel.signal_error =
                        Some(format!("stop {} failed: {error}", signal_name(kill)));
                }
            }
            mark(&mut state, Fact::Cancel);
            self.flush(&mut state);
        }
    }

    pub(crate) fn held(&self) -> bool {
        lock(&self.state).held
    }

    pub(crate) fn set_running(&self, pid: u32) -> SystemTime {
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

    pub(crate) fn record_output(
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

    pub(crate) fn not_started(&self, reason: String, running: &AtomicUsize) {
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

    pub(crate) fn observe_exit(&self, pid: u32, spawned_at: SystemTime, exit: Exit) {
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

    pub(crate) fn finish(&self, released: bool, running: &AtomicUsize) {
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

fn kernel_boot_changed(dispatched: &ProcessContext, current: &ProcessContext) -> bool {
    dispatched.boot_id_scope == BootIdScope::Kernel
        && current.boot_id_scope == BootIdScope::Kernel
        && dispatched.boot_id != current.boot_id
}

/// The reported name of the second stop stage, for error strings.
fn signal_name(kill: bool) -> &'static str {
    if kill { "SIGKILL" } else { "SIGTERM" }
}

fn probe(dir: &Path) -> io::Result<bool> {
    if !fs::symlink_metadata(dir)?.file_type().is_dir() {
        return Err(io::Error::other("run lock directory was replaced"));
    }
    let file = open_nofollow(dir.join(store::RUN_LOCK))?;
    Ok(!store::try_lock(&file)?)
}

/// Whether the run's native side still holds its run dir. `None` when it
/// cannot be observed.
///
/// On Unix the holder's flock survives in every process that inherited the
/// lock file descriptor, so a still-locked file proves presence. On Windows
/// a byte-range lock dies with its last handle and MSYS children do not keep
/// it, so presence is proven by the recorded main process's existence; a run
/// whose spawn pid is unknown reports nothing.
fn run_present(state: &State, dir: &Path) -> Option<bool> {
    #[cfg(unix)]
    {
        match probe(dir) {
            Ok(present) => Some(present),
            Err(_) => None,
        }
    }
    #[cfg(windows)]
    {
        match state.record.execution.value() {
            Some(ExecutionFact::SpawnObserved { pid, spawned_at }) => {
                // The pid probe can guard against Windows pid reuse by
                // comparing the kernel's process creation time against this
                // run's spawn observation.
                let _ = spawned_at;
                Some(!os::pid_absent_since(*pid, None))
            }
            // Dispatched but not spawned: the run lock file existing under a
            // real dir proves the dispatch reached its file preparation.
            _ => probe(dir).ok(),
        }
    }
}

/// Opens a file refusing to follow a symlink at the final component.
pub(crate) fn open_nofollow(path: PathBuf) -> io::Result<File> {
    open_nofollow_impl(path)
}

#[cfg(unix)]
fn open_nofollow_impl(path: PathBuf) -> io::Result<File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn open_nofollow_impl(path: PathBuf) -> io::Result<File> {
    // Windows opens by name without following symlinks when
    // FILE_FLAG_OPEN_REPARSE_POINT is not needed for plain files: a symlinked
    // final component is still resolved by the OS. The damage a replaced lock
    // could do is bounded by the advisory lock inside, so plain open is kept.
    File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_kernel_boot_changes_prove_absence_across_namespaces() {
        for dispatched_scope in [BootIdScope::Kernel, BootIdScope::LocalView] {
            for current_scope in [BootIdScope::Kernel, BootIdScope::LocalView] {
                let dispatched = ProcessContext {
                    boot_id: "before".into(),
                    boot_id_scope: dispatched_scope,
                    pid_namespace: Some("pid:[before]".into()),
                };
                let mut current = ProcessContext {
                    boot_id: "after".into(),
                    boot_id_scope: current_scope,
                    pid_namespace: Some("pid:[after]".into()),
                };
                assert_eq!(
                    kernel_boot_changed(&dispatched, &current),
                    dispatched_scope == BootIdScope::Kernel && current_scope == BootIdScope::Kernel
                );
                // A namespace change within the same kernel boot proves no loss.
                current.boot_id = dispatched.boot_id.clone();
                assert!(!kernel_boot_changed(&dispatched, &current));
                assert_ne!(dispatched, current);
            }
        }
    }
}
