//! Rho Core: managed local runs for the first selected flow.
//!
//! A caller submits code for a configured executor in a project directory under a
//! caller-held request identity. Core accepts each identity once, holds the
//! process independently of any waiter, and lets the caller find the original
//! request, its known state and bounded retained output again.
//!
//! Guarantees last for the lifetime of the [`Core`] value only. The contract,
//! limits and failure semantics are in [`CONTRACT`] (`docs/MANAGED-RUN.md`);
//! `examples/managed_run.rs` is a runnable caller.

#[cfg(not(unix))]
compile_error!("rho-core currently supports Unix platforms only");

mod error;
mod os;
mod run;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

pub use error::{LookupError, OpenError, ReadError, SubmitError, WorkdirProblem};
pub use run::{
    CancelInfo, CancelReason, Exit, OutputChunk, RequestInfo, RunStatus, RunView, Stream,
    StreamInfo,
};
use run::{RunShared, lock};

/// The managed-run contract, readable at run time.
pub const CONTRACT: &str = include_str!("../docs/MANAGED-RUN.md");

const IDENTITY_MAX_BYTES: usize = 128;
const SHUTDOWN_MARGIN: Duration = Duration::from_secs(1);

/// A native program Core may start, chosen by the application, not the request.
/// A run executes `program args... <script> <request args...>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Executor {
    pub name: String,
    /// Absolute path; no `PATH` lookup.
    pub program: PathBuf,
    pub args: Vec<String>,
}

impl Executor {
    pub fn new(name: impl Into<String>, program: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            program: program.into(),
            args: Vec::new(),
        }
    }

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }
}

/// Bounds on resources Core owns. Exceeding one rejects or limits explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Processes held at once; a new request beyond it is rejected, not queued.
    pub max_running: usize,
    /// Accepted request records kept by this instance; never evicted.
    pub max_records: usize,
    pub max_code_bytes: usize,
    pub max_args: usize,
    /// Total bytes over all request arguments.
    pub max_arg_bytes: usize,
    /// Retained bytes per output stream; later bytes are counted, not kept.
    pub max_stream_bytes: u64,
    /// Largest chunk returned by one output read.
    pub max_read_bytes: usize,
    /// Wait between SIGTERM and SIGKILL when stopping a run.
    pub stop_grace: Duration,
    /// Wait for output to close after the main process exits.
    pub output_close_grace: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_running: 4,
            max_records: 256,
            max_code_bytes: 1 << 20,
            max_args: 256,
            max_arg_bytes: 64 << 10,
            max_stream_bytes: 1 << 20,
            max_read_bytes: 64 << 10,
            stop_grace: Duration::from_secs(2),
            output_close_grace: Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CoreConfig {
    pub project_root: PathBuf,
    /// Parent of this instance's run files; created if missing.
    pub state_dir: PathBuf,
    pub executors: Vec<Executor>,
    pub limits: Limits,
}

impl CoreConfig {
    pub fn new(project_root: impl Into<PathBuf>, state_dir: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            state_dir: state_dir.into(),
            executors: Vec::new(),
            limits: Limits::default(),
        }
    }

    pub fn executor(mut self, executor: Executor) -> Self {
        self.executors.push(executor);
        self
    }

    pub fn limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
}

/// One managed run. All fields are compared byte for byte when an identity
/// is submitted again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRequest {
    pub request_id: String,
    pub executor: String,
    /// Project-relative directory the process starts in.
    pub workdir: PathBuf,
    /// Script text, snapshotted at acceptance and passed to the executor as a file.
    pub code: String,
    pub args: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// This submission created the record and dispatched the run.
    Accepted,
    /// The identity was already accepted with the same request; nothing was dispatched.
    Existing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submitted {
    pub disposition: Disposition,
    pub run: RunView,
}

/// What this instance offers and how it is bounded.
#[derive(Clone, Debug)]
pub struct Capability {
    pub contract: &'static str,
    pub project_root: PathBuf,
    /// This instance's run files; removed when the Core is dropped after all
    /// runs stopped.
    pub state_dir: PathBuf,
    pub executors: Vec<Executor>,
    pub limits: Limits,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Runs still held when shutdown began whose stop was then confirmed.
    pub stopped: Vec<String>,
    /// Runs whose stop was not confirmed before the shutdown deadline.
    pub not_confirmed: Vec<String>,
}

/// The programmatic boundary. Share it across threads by reference or `Arc`.
pub struct Core {
    project_root: PathBuf,
    instance_dir: PathBuf,
    executors: HashMap<String, Executor>,
    limits: Limits,
    registry: Mutex<Registry>,
    running: Arc<AtomicUsize>,
}

#[derive(Default)]
struct Registry {
    runs: HashMap<(String, String), Arc<RunShared>>,
    next_seq: u64,
    shutting_down: bool,
}

impl Core {
    pub fn open(config: CoreConfig) -> Result<Self, OpenError> {
        let error = |message: String| OpenError(message);
        let project_root = fs::canonicalize(&config.project_root).map_err(|e| {
            error(format!(
                "project root `{}`: {e}",
                config.project_root.display()
            ))
        })?;
        if !project_root.is_dir() {
            return Err(error(format!(
                "project root `{}` is not a directory",
                project_root.display()
            )));
        }
        check_limits(&config.limits).map_err(error)?;
        let mut executors = HashMap::new();
        for executor in config.executors {
            check_identity("executor name", &executor.name).map_err(error)?;
            if !executor.program.is_absolute() || !executor.program.is_file() {
                return Err(error(format!(
                    "executor `{}`: program `{}` must be an absolute path to an existing file",
                    executor.name,
                    executor.program.display()
                )));
            }
            if executors.insert(executor.name.clone(), executor).is_some() {
                return Err(error("executor names must be unique".into()));
            }
        }
        fs::create_dir_all(&config.state_dir)
            .map_err(|e| error(format!("state dir `{}`: {e}", config.state_dir.display())))?;
        let instance_dir = create_instance_dir(&config.state_dir)
            .map_err(|e| error(format!("state dir `{}`: {e}", config.state_dir.display())))?;
        Ok(Self {
            project_root,
            instance_dir,
            executors,
            limits: config.limits,
            registry: Mutex::default(),
            running: Arc::default(),
        })
    }

    pub fn capability(&self) -> Capability {
        let mut executors: Vec<Executor> = self.executors.values().cloned().collect();
        executors.sort_by(|a, b| a.name.cmp(&b.name));
        Capability {
            contract: CONTRACT,
            project_root: self.project_root.clone(),
            state_dir: self.instance_dir.clone(),
            executors,
            limits: self.limits.clone(),
        }
    }

    /// Accepts and starts a run, or returns the run already accepted under this
    /// identity. The decision for one identity is atomic across threads.
    pub fn submit(&self, caller: &str, request: RunRequest) -> Result<Submitted, SubmitError> {
        check_identity("caller", caller).map_err(SubmitError::InvalidRequest)?;
        check_identity("request id", &request.request_id).map_err(SubmitError::InvalidRequest)?;
        self.check_shape(&request)?;
        let fingerprint = fingerprint(&request);
        // Object checks precede acceptance so that a rejected request leaves no record.
        let resolved = self.resolve(&request);

        let mut registry = lock(&self.registry);
        let key = (caller.to_owned(), request.request_id.clone());
        if let Some(existing) = registry.runs.get(&key) {
            return if existing.fingerprint == fingerprint {
                Ok(Submitted {
                    disposition: Disposition::Existing,
                    run: existing.view(),
                })
            } else {
                Err(SubmitError::Conflict {
                    existing: Box::new(existing.view()),
                })
            };
        }
        let (executor, workdir_resolved) = resolved?;
        if registry.shutting_down {
            return Err(SubmitError::ShuttingDown);
        }
        if registry.runs.len() >= self.limits.max_records {
            return Err(SubmitError::Capacity {
                resource: "accepted request records",
                limit: self.limits.max_records,
            });
        }
        // Only submissions increment, and only under the registry lock.
        if self.running.load(Ordering::SeqCst) >= self.limits.max_running {
            return Err(SubmitError::Capacity {
                resource: "running processes",
                limit: self.limits.max_running,
            });
        }
        self.running.fetch_add(1, Ordering::SeqCst);
        registry.next_seq += 1;
        let seq = registry.next_seq;
        let instance = self
            .instance_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let info = RequestInfo {
            caller: caller.to_owned(),
            request_id: request.request_id.clone(),
            executor: executor.name.clone(),
            program: executor.program.clone(),
            program_args: executor.args.clone(),
            workdir: request.workdir.clone(),
            workdir_resolved,
            code_sha256: hex(&Sha256::digest(request.code.as_bytes())),
            code_bytes: request.code.len(),
            args: request.args.clone(),
        };
        let run = Arc::new(RunShared::new(
            format!("{instance}.{seq}"),
            fingerprint,
            info,
            self.instance_dir.join(seq.to_string()),
            self.limits.max_stream_bytes,
        ));
        registry.runs.insert(key, Arc::clone(&run));
        drop(registry);

        run::dispatch(
            &run,
            executor,
            &request.code,
            &request.args,
            &self.limits,
            &self.running,
        );
        Ok(Submitted {
            disposition: Disposition::Accepted,
            run: run.view(),
        })
    }

    /// Current known facts of an accepted request in this caller's scope.
    pub fn lookup(&self, caller: &str, request_id: &str) -> Result<RunView, LookupError> {
        Ok(self.find(caller, request_id)?.view())
    }

    /// Waits up to `timeout` for the run to become terminal and returns the
    /// facts at that moment. A waiter giving up does not affect the run.
    pub fn wait(
        &self,
        caller: &str,
        request_id: &str,
        timeout: Duration,
    ) -> Result<RunView, LookupError> {
        Ok(self.find(caller, request_id)?.wait(timeout))
    }

    /// Requests a stop: SIGTERM to the run's process group, SIGKILL after
    /// `stop_grace`. The returned view records the request; the stop is
    /// confirmed only when the status becomes terminal.
    pub fn cancel(&self, caller: &str, request_id: &str) -> Result<RunView, LookupError> {
        Ok(self
            .find(caller, request_id)?
            .request_cancel(CancelReason::Caller))
    }

    /// Reads retained output from `offset`, at most `min(max_bytes, max_read_bytes)` bytes.
    pub fn read_output(
        &self,
        caller: &str,
        request_id: &str,
        stream: Stream,
        offset: u64,
        max_bytes: usize,
    ) -> Result<OutputChunk, ReadError> {
        let max_bytes = max_bytes.min(self.limits.max_read_bytes);
        self.find(caller, request_id)?
            .read(stream, offset, max_bytes)
    }

    /// Rejects new requests, asks every held run to stop and waits for
    /// confirmation. Records stay readable until the Core is dropped.
    pub fn shutdown(&self) -> ShutdownReport {
        let runs: Vec<Arc<RunShared>> = {
            let mut registry = lock(&self.registry);
            registry.shutting_down = true;
            registry.runs.values().cloned().collect()
        };
        let held: Vec<Arc<RunShared>> = runs
            .into_iter()
            .filter(|run| !run.request_cancel(CancelReason::Shutdown).is_terminal())
            .collect();
        let deadline = Instant::now()
            + self.limits.stop_grace
            + self.limits.output_close_grace
            + SHUTDOWN_MARGIN;
        let mut report = ShutdownReport::default();
        for run in held {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if run.wait(remaining).is_terminal() {
                report.stopped.push(run.run_id.clone());
            } else {
                report.not_confirmed.push(run.run_id.clone());
            }
        }
        report
    }

    fn find(&self, caller: &str, request_id: &str) -> Result<Arc<RunShared>, LookupError> {
        check_identity("caller", caller).map_err(LookupError::InvalidIdentity)?;
        check_identity("request id", request_id).map_err(LookupError::InvalidIdentity)?;
        let key = (caller.to_owned(), request_id.to_owned());
        lock(&self.registry)
            .runs
            .get(&key)
            .cloned()
            .ok_or_else(|| LookupError::NotFound {
                caller: caller.to_owned(),
                request_id: request_id.to_owned(),
            })
    }

    fn check_shape(&self, request: &RunRequest) -> Result<(), SubmitError> {
        let invalid = |reason: String| Err(SubmitError::InvalidRequest(reason));
        if request.code.len() > self.limits.max_code_bytes {
            return invalid(format!("code exceeds {} bytes", self.limits.max_code_bytes));
        }
        if request.args.len() > self.limits.max_args {
            return invalid(format!("more than {} arguments", self.limits.max_args));
        }
        let arg_bytes: usize = request.args.iter().map(String::len).sum();
        if arg_bytes > self.limits.max_arg_bytes {
            return invalid(format!(
                "arguments exceed {} bytes",
                self.limits.max_arg_bytes
            ));
        }
        if request.args.iter().any(|arg| arg.contains('\0')) {
            return invalid("arguments cannot contain NUL bytes".into());
        }
        Ok(())
    }

    fn resolve(&self, request: &RunRequest) -> Result<(&Executor, PathBuf), SubmitError> {
        let executor = self
            .executors
            .get(&request.executor)
            .ok_or_else(|| SubmitError::UnknownExecutor(request.executor.clone()))?;
        let workdir = resolve_workdir(&self.project_root, &request.workdir).map_err(|problem| {
            SubmitError::Workdir {
                workdir: request.workdir.clone(),
                problem,
            }
        })?;
        Ok((executor, workdir))
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        if self.shutdown().not_confirmed.is_empty() {
            let _ = fs::remove_dir_all(&self.instance_dir);
        }
    }
}

/// Containment check only; it is not a sandbox and the directory can still
/// change between this check and process start.
fn resolve_workdir(project_root: &Path, workdir: &Path) -> Result<PathBuf, WorkdirProblem> {
    if workdir.as_os_str().is_empty() || workdir.is_absolute() {
        return Err(WorkdirProblem::NotRelative);
    }
    let resolved = fs::canonicalize(project_root.join(workdir)).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            WorkdirProblem::NotFound
        } else {
            WorkdirProblem::Unreadable(error.to_string())
        }
    })?;
    if !resolved.starts_with(project_root) {
        return Err(WorkdirProblem::OutsideProject { resolved });
    }
    if !resolved.is_dir() {
        return Err(WorkdirProblem::NotADirectory);
    }
    Ok(resolved)
}

fn create_instance_dir(state_dir: &Path) -> io::Result<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..16u32 {
        let name = format!("{:x}-{}-{attempt}", nanos, std::process::id());
        let dir = state_dir.join(name);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no fresh instance directory name",
    ))
}

fn check_identity(kind: &str, value: &str) -> Result<(), String> {
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '@' | '-');
    if value.is_empty() || value.len() > IDENTITY_MAX_BYTES || !value.chars().all(allowed) {
        return Err(format!(
            "{kind} must be 1-{IDENTITY_MAX_BYTES} characters of [A-Za-z0-9._:@-], got {value:?}"
        ));
    }
    Ok(())
}

fn check_limits(limits: &Limits) -> Result<(), String> {
    let counts = [
        limits.max_running,
        limits.max_records,
        limits.max_code_bytes,
        limits.max_args,
        limits.max_arg_bytes,
        limits.max_read_bytes,
    ];
    if counts.contains(&0) || limits.max_stream_bytes == 0 {
        return Err("limits must be greater than zero".into());
    }
    Ok(())
}

/// Length-prefixed digest of every request field compared for deduplication.
fn fingerprint(request: &RunRequest) -> [u8; 32] {
    let mut hasher = Sha256::new();
    let mut field = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    field(b"rho-core/run-request/v1");
    field(request.executor.as_bytes());
    field(request.workdir.as_os_str().as_encoded_bytes());
    field(request.code.as_bytes());
    field(&(request.args.len() as u64).to_le_bytes());
    for arg in &request.args {
        field(arg.as_bytes());
    }
    hasher.finalize().into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
