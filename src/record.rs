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
    Attempted,
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
}

impl ExecutionFact {
    pub(crate) fn rank(&self) -> u8 {
        match self {
            Self::SpawnObserved { .. } => 1,
            Self::NotStarted { .. } | Self::ExitObserved { .. } => 2,
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
        matches!(
            self.execution.value(),
            Some(ExecutionFact::NotStarted { .. } | ExecutionFact::ExitObserved { .. })
        ) && self.group_released.value() == Some(&true)
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
