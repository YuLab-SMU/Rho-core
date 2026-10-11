//! Independent facts of the selected managed process, not a workflow state machine.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::{CancelInfo, Exit, RequestInfo, StreamInfo};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestKey {
    pub scope_id: String,
    pub caller_id: String,
    pub request_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FactSource {
    Core,
    NativeProcess,
    StoreRecovery,
    RunLock,
    /// `kill(pid, 0)` or `kill(-pgid, 0)` found no such process or group.
    ProcessTable,
    /// The OS boot session differs from the one the run was dispatched in.
    BootSession,
}

/// Where a pid names a process: the OS boot session and, on Linux, the pid
/// namespace. Absence of a pid only says something within the same context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessContext {
    /// `kern.bootsessionuuid` on macOS, `/proc/sys/kernel/random/boot_id` on Linux.
    pub boot_id: String,
    /// The `/proc/self/ns/pid` link on Linux; `None` where pids are not namespaced.
    pub pid_namespace: Option<String>,
}

/// Why a run nobody holds is known to have no main process any more.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbsenceEvidence {
    /// No process had the recorded main pid, in the dispatch's boot session and
    /// pid namespace. A reused pid reads as present, never as absent.
    MainPidAbsent,
    /// The OS has booted again since dispatch; no process of that boot remains.
    BootChanged { dispatched: String, current: String },
}

/// A known observation retains its source and time. Unknown is never permission
/// to dispatch; a lost holder does not erase an earlier native observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Knowledge<T> {
    Known {
        value: T,
        observed_at: SystemTime,
        source: FactSource,
    },
    Unknown {
        reason: String,
        since: SystemTime,
    },
}

impl<T> Knowledge<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Known { value, .. } => Some(value),
            Self::Unknown { .. } => None,
        }
    }

    pub(crate) fn known(value: T, source: FactSource) -> Self {
        Self::Known {
            value,
            observed_at: SystemTime::now(),
            source,
        }
    }

    pub(crate) fn unknown(reason: impl Into<String>, since: SystemTime) -> Self {
        Self::Unknown {
            reason: reason.into(),
            since,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DispatchFact {
    NotAttempted,
    /// Saved before calling the native executor; it does not prove a spawn.
    /// `context` is `None` when it could not be read; pids then prove nothing.
    Attempted {
        context: Option<ProcessContext>,
    },
}

impl DispatchFact {
    pub(crate) fn attempted(&self) -> bool {
        matches!(self, Self::Attempted { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionFact {
    NotStarted {
        reason: String,
    },
    SpawnObserved {
        pid: u32,
        spawned_at: SystemTime,
    },
    ExitObserved {
        pid: u32,
        spawned_at: SystemTime,
        exit: Exit,
    },
    /// The main process ended, or with `pid: None` possibly never started,
    /// while no Core held it. Its exit status is unknown.
    Gone {
        pid: Option<u32>,
        spawned_at: Option<SystemTime>,
        evidence: AbsenceEvidence,
    },
}

impl ExecutionFact {
    pub(crate) fn rank(&self) -> u8 {
        match self {
            Self::SpawnObserved { .. } => 1,
            Self::NotStarted { .. } | Self::ExitObserved { .. } | Self::Gone { .. } => 2,
        }
    }

    /// The main pid, which is also the run's process group id.
    pub(crate) fn pid(&self) -> Option<u32> {
        match self {
            Self::SpawnObserved { pid, .. } | Self::ExitObserved { pid, .. } => Some(*pid),
            Self::Gone { pid, .. } => *pid,
            Self::NotStarted { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputStream {
    pub info: StreamInfo,
    pub blobs: Vec<BlobRef>,
    pub observed_at: SystemTime,
    pub source: FactSource,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSet {
    pub stdout: OutputStream,
    pub stderr: OutputStream,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetentionState {
    Protected { reason: String },
    Removable,
}

/// The authoritative facts returned with each RunView. `status` on RunView is
/// only a convenience projection; no combined workflow status is stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRecord {
    pub operation_id: String,
    pub key: RequestKey,
    pub request: RequestInfo,
    pub script: BlobRef,
    pub accepted_at: SystemTime,
    pub dispatch: Knowledge<DispatchFact>,
    pub execution: Knowledge<ExecutionFact>,
    pub cancellation: Option<CancelInfo>,
    pub outputs: OutputSet,
    pub group_released: Knowledge<bool>,
    /// Only whether the inherited descriptor still holds its lock. False does
    /// not prove that a process exited: native code can close the descriptor.
    pub run_lock: Knowledge<bool>,
    pub retention: RetentionState,
    /// Revision of the persisted facts; unsaved observations are marked by RunView.
    pub revision: u64,
}

impl OperationRecord {
    pub(crate) fn removable(&self) -> bool {
        self.execution.value().is_some_and(|fact| fact.rank() == 2)
            && self.group_released.value() == Some(&true)
    }

    pub(crate) fn retention(&mut self, held: bool, unsaved: bool) {
        self.retention = if !held && !unsaved && self.removable() {
            RetentionState::Removable
        } else {
            RetentionState::Protected {
                reason: "native completion/release or durable facts are not confirmed".into(),
            }
        };
    }
}
