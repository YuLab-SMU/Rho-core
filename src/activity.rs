//! Retained observations of external activity. Receiving an event never executes
//! its content and does not make the external action a managed Core run.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

use crate::{Core, check_identity};

pub const ACTIVITY_CONTRACT: &str = include_str!("../docs/ACTIVITY.md");

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityLimits {
    pub max_streams: usize,
    pub max_events: usize,
    pub max_bytes: usize,
    pub max_event_bytes: usize,
    pub max_batch_events: usize,
    pub max_batch_bytes: usize,
    pub max_read_events: usize,
    pub max_read_bytes: usize,
}

impl Default for ActivityLimits {
    fn default() -> Self {
        Self {
            max_streams: 64,
            max_events: 4096,
            max_bytes: 4 << 20,
            max_event_bytes: 64 << 10,
            max_batch_events: 64,
            max_batch_bytes: 256 << 10,
            max_read_events: 128,
            max_read_bytes: 256 << 10,
        }
    }
}

impl ActivityLimits {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if [
            self.max_streams,
            self.max_events,
            self.max_bytes,
            self.max_event_bytes,
            self.max_batch_events,
            self.max_batch_bytes,
            self.max_read_events,
            self.max_read_bytes,
        ]
        .contains(&0)
            || self.max_event_bytes > self.max_bytes
        {
            return Err(
                "activity limits must be positive; an event must fit its stream byte budget".into(),
            );
        }
        Ok(())
    }
}

/// Immutable source selection. Metadata is inert: paths and links grant no access.
/// Adapters identify their normalization version here so a changed interpretation
/// cannot silently overwrite the old stream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivitySource {
    pub stream_id: String,
    pub producer: String,
    pub session_id: String,
    pub metadata: Value,
}

/// Sequence is the adapter's stable, contiguous ordinal, starting at zero.
/// Native sequence, timestamps and causal identifiers belong in `data` and keep
/// their own meaning. Filtering must not change these ordinals on replay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityEvent {
    pub sequence: u64,
    pub kind: String,
    pub data: Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEntry {
    /// Stream-local receipt position, independent of native clocks or causality.
    pub cursor: u64,
    pub received_at_ms: u64,
    pub event: ActivityEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityStream {
    pub source: ActivitySource,
    /// The next new ordinal; also the latest receipt cursor.
    pub next_sequence: u64,
    pub retained_from: u64,
    pub retained_events: usize,
    pub retained_bytes: usize,
    pub dropped_events: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityReceipt {
    pub sequence: u64,
    pub cursor: u64,
    pub existing: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityPage {
    pub stream: ActivityStream,
    pub entries: Vec<ActivityEntry>,
    pub next_cursor: u64,
    pub more: bool,
    /// The requested prefix has been evicted; never means that no actions ran.
    pub gap: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityError {
    Invalid(String),
    NotFound,
    Conflict,
    Sequence {
        expected: u64,
        received: u64,
    },
    Expired {
        sequence: u64,
        retained_from: u64,
    },
    Limit {
        resource: &'static str,
        limit: usize,
    },
    Storage(String),
}

impl ActivityError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_activity",
            Self::NotFound => "activity_not_found",
            Self::Conflict => "activity_conflict",
            Self::Sequence { .. } => "activity_sequence",
            Self::Expired { .. } => "activity_expired",
            Self::Limit { .. } => "activity_limit",
            Self::Storage(_) => "activity_storage",
        }
    }
}
impl fmt::Display for ActivityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) => write!(f, "invalid activity: {s}"),
            Self::NotFound => write!(f, "no activity stream in this caller scope"),
            Self::Conflict => write!(
                f,
                "activity identity already names different content; original retained"
            ),
            Self::Sequence { expected, received } => write!(
                f,
                "expected original sequence {expected}, received {received}; nothing appended"
            ),
            Self::Expired {
                sequence,
                retained_from,
            } => write!(
                f,
                "sequence {sequence} expired before retained cursor {retained_from}; do not renumber or replay external actions"
            ),
            Self::Limit { resource, limit } => write!(
                f,
                "activity {resource} limit {limit}; existing observations retained"
            ),
            Self::Storage(s) => write!(f, "activity storage unavailable: {s}"),
        }
    }
}
impl std::error::Error for ActivityError {}
impl From<rusqlite::Error> for ActivityError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e.to_string())
    }
}
impl From<serde_json::Error> for ActivityError {
    fn from(e: serde_json::Error) -> Self {
        Self::Storage(e.to_string())
    }
}

fn identity(caller: &str, stream: &str) -> Result<(), ActivityError> {
    check_identity("caller", caller).map_err(ActivityError::Invalid)?;
    check_identity("activity stream", stream).map_err(ActivityError::Invalid)
}

impl Core {
    pub fn open_activity(
        &self,
        caller: &str,
        source: ActivitySource,
    ) -> Result<ActivityStream, ActivityError> {
        identity(caller, &source.stream_id)?;
        check_identity("producer", &source.producer).map_err(ActivityError::Invalid)?;
        check_identity("session", &source.session_id).map_err(ActivityError::Invalid)?;
        if serde_json::to_vec(&source)?.len() > 4096 {
            return Err(ActivityError::Limit {
                resource: "source bytes",
                limit: 4096,
            });
        }
        self.store
            .open_activity(caller, &source, &self.limits.activity)
    }

    /// Atomic batch. A lost acknowledgement can be retried with the same events;
    /// an expired prefix is refused, never accepted again as new activity.
    pub fn append_activity(
        &self,
        caller: &str,
        stream: &str,
        events: &[ActivityEvent],
    ) -> Result<Vec<ActivityReceipt>, ActivityError> {
        identity(caller, stream)?;
        let limits = &self.limits.activity;
        if events.is_empty() || events.len() > limits.max_batch_events {
            return Err(ActivityError::Limit {
                resource: "batch events",
                limit: limits.max_batch_events,
            });
        }
        if serde_json::to_vec(events)?.len() > limits.max_batch_bytes {
            return Err(ActivityError::Limit {
                resource: "batch bytes",
                limit: limits.max_batch_bytes,
            });
        }
        for e in events {
            check_identity("event kind", &e.kind).map_err(ActivityError::Invalid)?;
            if e.sequence >= i64::MAX as u64 {
                return Err(ActivityError::Invalid("sequence exhausted".into()));
            }
            if serde_json::to_vec(e)?.len() > limits.max_event_bytes {
                return Err(ActivityError::Limit {
                    resource: "event bytes",
                    limit: limits.max_event_bytes,
                });
            }
        }
        self.store.append_activity(caller, stream, events, limits)
    }

    pub fn read_activity(
        &self,
        caller: &str,
        stream: &str,
        after: u64,
        max_events: usize,
        max_bytes: usize,
    ) -> Result<ActivityPage, ActivityError> {
        identity(caller, stream)?;
        if max_events == 0 || max_bytes == 0 {
            return Err(ActivityError::Invalid(
                "read bounds must be positive".into(),
            ));
        }
        self.store.read_activity(
            caller,
            stream,
            after,
            max_events.min(self.limits.activity.max_read_events),
            max_bytes.min(self.limits.activity.max_read_bytes),
        )
    }

    pub fn list_activity(&self, caller: &str) -> Result<Vec<ActivityStream>, ActivityError> {
        check_identity("caller", caller).map_err(ActivityError::Invalid)?;
        self.store.list_activity(caller)
    }

    /// Explicit deletion ends this stream's retention and duplicate protection.
    /// A concurrent append changes the cursor and prevents deletion.
    pub fn forget_activity(
        &self,
        caller: &str,
        stream: &str,
        expected_cursor: u64,
    ) -> Result<(), ActivityError> {
        identity(caller, stream)?;
        self.store.forget_activity(caller, stream, expected_cursor)
    }
}
