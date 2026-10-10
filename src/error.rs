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

/// A submission that was not accepted. No record was created and nothing was
/// started, except that [`SubmitError::Conflict`] reports an existing record.
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
        }
    }
}

impl std::error::Error for SubmitError {}

/// No accepted request is visible under this caller and request id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupError {
    InvalidIdentity(String),
    /// Nothing was accepted under this identity during this Core instance.
    /// This is not evidence that no process ran elsewhere.
    NotFound {
        caller: String,
        request_id: String,
    },
}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentity(reason) => write!(f, "invalid identity: {reason}"),
            Self::NotFound { caller, request_id } => write!(
                f,
                "no request `{request_id}` accepted for caller `{caller}` in this Core instance"
            ),
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
