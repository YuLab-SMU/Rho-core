//! Rho Core: managed local runs for the first selected flow.
//!
//! A caller submits code for a configured executor in a project directory under a
//! caller-held request identity. Core accepts each identity once, holds the
//! process independently of any waiter, and lets the caller find the original
//! request, its known state and bounded retained output again.
//!
//! Accepted records are durable in the state directory: a later `Core` opened on
//! the same directory finds them, never dispatches them again, and reports a
//! run its predecessor was holding as detached. The contract,
//! limits and failure semantics are in [`CONTRACT`] (`docs/MANAGED-RUN.md`);
//! `examples/managed_run.rs` is a runnable caller.

#[cfg(not(unix))]
compile_error!("rho-core currently supports Unix platforms only");

mod error;
mod fault;
mod os;
mod record;
mod run;
mod store;

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

pub use error::{ForgetError, LookupError, OpenError, ReadError, SubmitError, WorkdirProblem};
pub use record::{
    BlobRef, DispatchFact, ExecutionFact, FactSource, Knowledge, OperationRecord, OutputSet,
    OutputStream, RequestKey, RetentionState,
};
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
    /// Accepted request records kept in the state dir, including those of
    /// earlier instances; never evicted, only removed by [`Core::forget`].
    pub max_records: usize,
    pub max_code_bytes: usize,
    pub max_args: usize,
    /// Total bytes over all request arguments.
    pub max_arg_bytes: usize,
    /// Retained bytes per output stream; later bytes are counted, not kept.
    pub max_stream_bytes: u64,
    /// Maximum immutable output chunks per stream; bounds metadata and memory.
    pub max_output_chunks: usize,
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
            max_output_chunks: 4096,
            max_read_bytes: 64 << 10,
            stop_grace: Duration::from_secs(2),
            output_close_grace: Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CoreConfig {
    pub project_root: PathBuf,
    /// Durable run records; created if missing. One Core uses it at a time,
    /// and it stays bound to the first project opened on it.
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
    /// This submission durably accepted the request; native start may still fail.
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
    /// Durable run records, kept after the Core is dropped.
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
    state_dir: PathBuf,
    /// Distinguishes run ids of this instance from earlier ones.
    instance: String,
    /// Held for the Core's lifetime and by every holder thread.
    store: Arc<store::OperationStore>,
    executors: HashMap<String, Executor>,
    limits: Limits,
    registry: Mutex<Registry>,
    running: Arc<AtomicUsize>,
}

#[derive(Default)]
struct Registry {
    runs: HashMap<(String, String), Arc<RunShared>>,
    /// Record keys whose record could not be read, with the reason. Their
    /// identities are blocked: an unreadable record never authorizes a replay.
    unreadable: HashMap<(String, String), String>,
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
        let store = Arc::new(
            store::OperationStore::open(&config.state_dir, &project_root)
                .map_err(|e| error(e.to_string()))?,
        );
        let state_dir = store.dir.clone();
        let mut registry = Registry::default();
        for (caller, request_id) in store.identities().map_err(|e| error(e.to_string()))? {
            check_identity("stored caller", &caller).map_err(error)?;
            check_identity("stored request id", &request_id).map_err(error)?;
            match store.get(&caller, &request_id) {
                Ok(Some(stored)) => {
                    registry.runs.insert(
                        (caller, request_id),
                        Arc::new(RunShared::new(stored, Arc::clone(&store), false)),
                    );
                }
                Err(store::StoreError::Unreadable(reason)) => {
                    registry.unreadable.insert((caller, request_id), reason);
                }
                Err(e) => return Err(error(e.to_string())),
                Ok(None) => return Err(error("listed acceptance disappeared".into())),
            }
        }
        Ok(Self {
            project_root,
            state_dir,
            instance: instance_token(),
            store,
            executors,
            limits: config.limits,
            registry: Mutex::new(registry),
            running: Arc::default(),
        })
    }

    pub fn capability(&self) -> Capability {
        let mut executors: Vec<Executor> = self.executors.values().cloned().collect();
        executors.sort_by(|a, b| a.name.cmp(&b.name));
        Capability {
            contract: CONTRACT,
            project_root: self.project_root.clone(),
            state_dir: self.state_dir.clone(),
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
            return if existing.fingerprint == hex(&fingerprint) {
                Ok(Submitted {
                    disposition: Disposition::Existing,
                    run: existing.view().map_err(submit_lookup_error)?,
                })
            } else {
                Err(SubmitError::Conflict {
                    existing: Box::new(existing.view().map_err(submit_lookup_error)?),
                })
            };
        }
        if let Some(reason) = registry.unreadable.get(&key) {
            return Err(SubmitError::RecordUnreadable {
                reason: reason.clone(),
            });
        }
        // The database, not an absent cache entry, decides whether a key is free.
        if let Some(stored) = self
            .store
            .get(caller, &request.request_id)
            .map_err(submit_store_error)?
        {
            let run = Arc::new(RunShared::new(stored, Arc::clone(&self.store), false));
            registry.runs.insert(key, Arc::clone(&run));
            return existing_submission(&run, &fingerprint);
        }
        let (executor, workdir_resolved) = resolved?;
        if registry.shutting_down {
            return Err(SubmitError::ShuttingDown);
        }
        // Only submissions increment, and only under the registry lock.
        if self.running.load(Ordering::SeqCst) >= self.limits.max_running {
            return Err(SubmitError::Capacity {
                resource: "running processes",
                limit: self.limits.max_running,
            });
        }
        registry.next_seq += 1;
        let seq = registry.next_seq;
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
        let record = store::Accepted {
            run_id: format!("{}.{seq}", self.instance),
            fingerprint: hex(&fingerprint),
            key: RequestKey {
                scope_id: self.store.scope_id.clone(),
                caller_id: caller.into(),
                request_id: request.request_id.clone(),
            },
            script: BlobRef {
                sha256: info.code_sha256.clone(),
                bytes: info.code_bytes as u64,
            },
            request: info,
            accepted_at: SystemTime::now(),
            stream_limit: self.limits.max_stream_bytes,
            chunk_limit: self.limits.max_output_chunks,
        };
        let stored = match self
            .store
            .accept(record, &request.code, self.limits.max_records)
            .map_err(submit_store_error)?
        {
            store::Acceptance::New(stored) => *stored,
            store::Acceptance::Existing(stored) => {
                let run = Arc::new(RunShared::new(*stored, Arc::clone(&self.store), false));
                registry.runs.insert(key, Arc::clone(&run));
                return existing_submission(&run, &fingerprint);
            }
        };
        fault::point("committed");
        self.running.fetch_add(1, Ordering::SeqCst);
        let run = Arc::new(RunShared::new(stored, Arc::clone(&self.store), true));
        registry.runs.insert(key, Arc::clone(&run));
        drop(registry);
        run::dispatch(&run, executor, &request.args, &self.limits, &self.running);
        Ok(Submitted {
            disposition: Disposition::Accepted,
            run: run.view().map_err(submit_lookup_error)?,
        })
    }

    /// Current known facts of an accepted request in this caller's scope.
    pub fn lookup(&self, caller: &str, request_id: &str) -> Result<RunView, LookupError> {
        self.find(caller, request_id)?.view()
    }

    /// Finds this caller's retained operations after a restart, without needing
    /// to remember each request id. Bounded by the store's retained records.
    pub fn list_operations(&self, caller: &str) -> Result<Vec<RunView>, LookupError> {
        check_identity("caller", caller).map_err(LookupError::InvalidIdentity)?;
        let mut views = Vec::new();
        for (owner, id) in self.store.identities().map_err(run::lookup_error)? {
            if owner == caller {
                views.push(self.lookup(caller, &id)?);
            }
        }
        views.sort_by(|a, b| a.run_id.cmp(&b.run_id));
        Ok(views)
    }

    /// Waits up to `timeout` for the run to become terminal and returns the
    /// facts at that moment. A waiter giving up does not affect the run.
    pub fn wait(
        &self,
        caller: &str,
        request_id: &str,
        timeout: Duration,
    ) -> Result<RunView, LookupError> {
        self.find(caller, request_id)?.wait(timeout)
    }

    /// Requests a stop: SIGTERM to the run's process group, SIGKILL after
    /// `stop_grace`. The returned view records the request; the stop is
    /// confirmed by native execution facts, independently of resource release.
    pub fn cancel(&self, caller: &str, request_id: &str) -> Result<RunView, LookupError> {
        self.find(caller, request_id)?
            .request_cancel(CancelReason::Caller)
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

    /// Removes a completed, released record with no unsaved facts. The
    /// identity is then unknown: a later submission under it runs again.
    pub fn forget(&self, caller: &str, request_id: &str) -> Result<(), ForgetError> {
        check_identity("caller", caller).map_err(LookupError::InvalidIdentity)?;
        check_identity("request id", request_id).map_err(LookupError::InvalidIdentity)?;
        let mut registry = lock(&self.registry);
        let key = (caller.to_owned(), request_id.to_owned());
        if let Some(reason) = registry.unreadable.get(&key) {
            return Err(LookupError::RecordUnreadable {
                reason: reason.clone(),
            }
            .into());
        }
        if !registry.runs.contains_key(&key)
            && self
                .store
                .get(caller, request_id)
                .map_err(run::lookup_error)?
                .is_some()
        {
            return Err(LookupError::RecordUnreadable {
                reason: "stored acceptance is not loaded".into(),
            }
            .into());
        }
        let run = registry
            .runs
            .get(&key)
            .ok_or_else(|| LookupError::NotFound {
                caller: caller.into(),
                request_id: request_id.into(),
            })?;
        let view = run.view()?;
        if view.operation.retention != RetentionState::Removable {
            return Err(ForgetError::NotTerminal(Box::new(view)));
        }
        self.store
            .discard(&view.operation)
            .map_err(|e| ForgetError::Io(e.to_string()))?;
        registry.runs.remove(&key);
        Ok(())
    }

    /// Unreadable stored rows or database queries, by database path and reason.
    /// Their identities are rejected until the record is repaired or removed
    /// by hand; Core never re-dispatches them.
    pub fn unreadable_records(&self) -> Vec<(PathBuf, String)> {
        let path = self.state_dir.join(store::DATABASE);
        let mut list = Vec::new();
        match self.store.identities() {
            Ok(identities) => {
                for (caller, id) in identities {
                    if let Err(error) = self.store.get(&caller, &id) {
                        list.push((path.clone(), format!("{caller}/{id}: {error}")));
                    }
                }
            }
            Err(error) => list.push((path, error.to_string())),
        }
        list.sort();
        list
    }

    /// Rejects new requests, asks every held run to stop and waits for
    /// confirmation. Records stay readable until the Core is dropped.
    pub fn shutdown(&self) -> ShutdownReport {
        let runs: Vec<Arc<RunShared>> = {
            let mut registry = lock(&self.registry);
            registry.shutting_down = true;
            registry
                .runs
                .values()
                .filter(|run| run.held())
                .cloned()
                .collect()
        };
        let held: Vec<Arc<RunShared>> = runs
            .into_iter()
            .filter(|run| !run.shutdown_stop().is_terminal())
            .collect();
        let deadline = Instant::now()
            + self.limits.stop_grace
            + self.limits.output_close_grace
            + SHUTDOWN_MARGIN;
        let mut report = ShutdownReport::default();
        for run in held {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if run.wait_held(remaining).is_terminal() {
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
        let registry = lock(&self.registry);
        if let Some(run) = registry.runs.get(&key) {
            return Ok(Arc::clone(run));
        }
        if let Some(reason) = registry.unreadable.get(&key) {
            return Err(LookupError::RecordUnreadable {
                reason: reason.clone(),
            });
        }
        // An unavailable database is not an absent request.
        if self
            .store
            .get(caller, request_id)
            .map_err(run::lookup_error)?
            .is_some()
        {
            return Err(LookupError::RecordUnreadable {
                reason: "acceptance exists outside this instance's loaded registry".into(),
            });
        }
        Err(LookupError::NotFound {
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
    /// Stops held runs; records stay in the state dir. The store stays locked
    /// until every holder thread has saved its last fact and let go, so a Core
    /// opened right afterwards never races a predecessor's writes.
    fn drop(&mut self) {
        let report = self.shutdown();
        if !report.not_confirmed.is_empty() {
            // Those holders keep the store locked until they finish.
            return;
        }
        lock(&self.registry).runs.clear();
        let deadline = Instant::now() + SHUTDOWN_MARGIN;
        while Arc::strong_count(&self.store) > 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
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

fn instance_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}-{}", std::process::id())
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
        limits.max_output_chunks,
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

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn submit_lookup_error(error: LookupError) -> SubmitError {
    match error {
        LookupError::RecordUnreadable { reason } => SubmitError::RecordUnreadable { reason },
        error => SubmitError::Storage(error.to_string()),
    }
}
fn submit_store_error(error: store::StoreError) -> SubmitError {
    match error {
        store::StoreError::Capacity(limit) => SubmitError::Capacity {
            resource: "accepted request records",
            limit,
        },
        store::StoreError::Unreadable(reason) => SubmitError::RecordUnreadable { reason },
        error => SubmitError::Storage(format!("storage failed; nothing started: {error}")),
    }
}
fn existing_submission(run: &RunShared, fingerprint: &[u8; 32]) -> Result<Submitted, SubmitError> {
    let view = run.view().map_err(submit_lookup_error)?;
    if run.fingerprint == hex(fingerprint) {
        Ok(Submitted {
            disposition: Disposition::Existing,
            run: view,
        })
    } else {
        Err(SubmitError::Conflict {
            existing: Box::new(view),
        })
    }
}
