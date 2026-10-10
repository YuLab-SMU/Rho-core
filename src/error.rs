use std::fmt;
use std::path::PathBuf;

use crate::run::RunView;

/// The configuration could not be used to open a Core instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenError(pub(crate) String);

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot open Core: {}", self.0)
    }
}

impl std::error::Error for OpenError {}

/// Why a requested working directory was not accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkdirProblem {
    /// Absolute or empty; workdirs are project-relative.
    NotRelative,
    NotFound,
    NotADirectory,
    /// Resolves (after symlinks) outside the project root.
    OutsideProject {
        resolved: PathBuf,
    },
    Unreadable(String),
}

/// A submission could not return a usable accepted view. An acceptance or
/// native execution may already exist when a commit reply or later read failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubmitError {
    InvalidRequest(String),
    UnknownExecutor(String),
    Workdir {
        workdir: PathBuf,
        problem: WorkdirProblem,
    },
    /// The request id is already bound to a different request in this scope.
    /// The original record is unchanged and was not dispatched again.
    Conflict {
        existing: Box<RunView>,
    },
    /// A bounded resource is full; see [`crate::Limits`].
    Capacity {
        resource: &'static str,
        limit: usize,
    },
    ShuttingDown,
    /// The record for this identity exists but cannot be read. Nothing is
    /// started: it may already have run.
    RecordUnreadable {
        reason: String,
    },
    /// A storage transaction or query failed. The stage of failure determines
    /// whether acceptance or native execution has already happened.
    Storage(String),
}

impl fmt::Display for SubmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(reason) => write!(f, "invalid request, nothing started: {reason}"),
            Self::UnknownExecutor(name) => {
                write!(f, "executor `{name}` is not configured, nothing started")
            }
            Self::Workdir { workdir, problem } => {
                write!(
                    f,
                    "workdir `{}` rejected, nothing started: ",
                    workdir.display()
                )?;
                match problem {
                    WorkdirProblem::NotRelative => {
                        write!(f, "must be a non-empty project-relative path")
                    }
                    WorkdirProblem::NotFound => write!(f, "not found"),
                    WorkdirProblem::NotADirectory => write!(f, "not a directory"),
                    WorkdirProblem::OutsideProject { resolved } => {
                        write!(
                            f,
                            "resolves outside the project to `{}`",
                            resolved.display()
                        )
                    }
                    WorkdirProblem::Unreadable(error) => write!(f, "cannot be resolved: {error}"),
                }
            }
            Self::Conflict { existing } => write!(
                f,
                "request id `{}` is already bound to a different request (run {}); nothing was \
                 started again. Use a new request id to run again",
                existing.request.request_id, existing.run_id
            ),
            Self::Capacity { resource, limit } => {
                write!(f, "limit of {limit} {resource} reached, nothing started")
            }
            Self::ShuttingDown => write!(f, "Core is shutting down, nothing started"),
            Self::RecordUnreadable { reason } => write!(
                f,
                "the record for this request id cannot be read ({reason}); it may already have \
                 run, so nothing was started"
            ),
            Self::Storage(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for SubmitError {}

/// An acceptance lookup failed; storage uncertainty is distinct from absence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupError {
    InvalidIdentity(String),
    /// A successful query found no acceptance under this key. Store/read errors
    /// are distinct and this does not prove that no process ran elsewhere.
    NotFound {
        caller: String,
        request_id: String,
    },
    /// An acceptance exists but its stored facts cannot be interpreted safely.
    RecordUnreadable {
        reason: String,
    },
    /// The store could not be queried; this is never evidence of absence.
    Storage(String),
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentity(reason) => write!(f, "invalid identity: {reason}"),
            Self::NotFound { caller, request_id } => write!(
                f,
                "no accepted record of request `{request_id}` for caller `{caller}`"
            ),
            Self::RecordUnreadable { reason } => {
                write!(f, "accepted record is unreadable: {reason}")
            }
            Self::Storage(reason) => write!(f, "operation store could not be queried: {reason}"),
        }
    }
}

impl std::error::Error for LookupError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    Lookup(LookupError),
    /// Only `retained` bytes are held; reading cannot start beyond them.
    OffsetBeyondRetained {
        offset: u64,
        retained: u64,
    },
    /// The retained file could not be read; the run itself is unaffected.
    Io(String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lookup(error) => error.fmt(f),
            Self::OffsetBeyondRetained { offset, retained } => {
                write!(f, "offset {offset} is beyond the {retained} retained bytes")
            }
            Self::Io(error) => write!(f, "retained output could not be read: {error}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<LookupError> for ReadError {
    fn from(error: LookupError) -> Self {
        Self::Lookup(error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForgetError {
    Lookup(LookupError),
    /// Completion, release or saved facts are unconfirmed; the record is protected.
    NotTerminal(Box<RunView>),
    Io(String),
}

impl fmt::Display for ForgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lookup(error) => error.fmt(f),
            Self::NotTerminal(run) => {
                write!(
                    f,
                    "run {} is protected; completion, release or saved facts are unconfirmed; nothing removed",
                    run.run_id
                )
            }
            Self::Io(error) => write!(f, "record could not be removed: {error}"),
        }
    }
}

impl std::error::Error for ForgetError {}

impl From<LookupError> for ForgetError {
    fn from(error: LookupError) -> Self {
        Self::Lookup(error)
    }
}
