use super::*;
use crate::activity::*;

pub(super) const SCHEMA: &str = "
CREATE TABLE activity_streams (
 caller TEXT NOT NULL, stream TEXT NOT NULL, source TEXT NOT NULL,
 next_sequence INTEGER NOT NULL CHECK(next_sequence>=0),
 PRIMARY KEY(caller,stream));
CREATE TABLE activity_events (
 caller TEXT NOT NULL, stream TEXT NOT NULL, sequence INTEGER NOT NULL CHECK(sequence>=0),
 event TEXT NOT NULL, received_at_ms INTEGER NOT NULL CHECK(received_at_ms>=0),
 PRIMARY KEY(caller,stream,sequence),
 FOREIGN KEY(caller,stream) REFERENCES activity_streams(caller,stream) ON DELETE CASCADE);
";

type ActivityResult<T> = std::result::Result<T, ActivityError>;

fn stream_on(db: &Connection, caller: &str, stream: &str) -> ActivityResult<ActivityStream> {
    let row: Option<(String, u64)> = db
        .query_row(
            "SELECT source,next_sequence FROM activity_streams WHERE caller=?1 AND stream=?2",
            params![caller, stream],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (source, next_sequence) = row.ok_or(ActivityError::NotFound)?;
    let source: ActivitySource = serde_json::from_str(&source)?;
    let (count, bytes, first, last): (usize, usize, Option<u64>, Option<u64>) = db.query_row(
        "SELECT count(*),coalesce(sum(length(cast(event AS BLOB))),0),min(sequence),max(sequence)
         FROM activity_events WHERE caller=?1 AND stream=?2",
        params![caller, stream],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if source.stream_id != stream
        || count as u64 > next_sequence
        || (next_sequence > 0
            && (last != Some(next_sequence - 1) || first != Some(next_sequence - count as u64)))
    {
        return Err(ActivityError::Storage(
            "inconsistent activity identity or retained suffix".into(),
        ));
    }
    Ok(ActivityStream {
        source,
        next_sequence,
        retained_from: first.map_or(next_sequence + 1, |n| n + 1),
        retained_events: count,
        retained_bytes: bytes,
        dropped_events: next_sequence - count as u64,
    })
}

impl OperationStore {
    pub(crate) fn open_activity(
        &self,
        caller: &str,
        source: &ActivitySource,
        limits: &ActivityLimits,
    ) -> ActivityResult<ActivityStream> {
        let mut inner = lock(&self.inner);
        let tx = inner
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        match stream_on(&tx, caller, &source.stream_id) {
            Ok(existing) => {
                return if existing.source == *source {
                    Ok(existing)
                } else {
                    Err(ActivityError::Conflict)
                };
            }
            Err(ActivityError::NotFound) => {}
            Err(e) => return Err(e),
        }
        let count: usize =
            tx.query_row("SELECT count(*) FROM activity_streams", [], |r| r.get(0))?;
        if count >= limits.max_streams {
            return Err(ActivityError::Limit {
                resource: "streams",
                limit: limits.max_streams,
            });
        }
        tx.execute(
            "INSERT INTO activity_streams VALUES (?1,?2,?3,0)",
            params![caller, source.stream_id, serde_json::to_string(source)?],
        )?;
        let result = stream_on(&tx, caller, &source.stream_id)?;
        tx.commit()?;
        Ok(result)
    }

    pub(crate) fn append_activity(
        &self,
        caller: &str,
        stream: &str,
        events: &[ActivityEvent],
        limits: &ActivityLimits,
    ) -> ActivityResult<Vec<ActivityReceipt>> {
        let mut inner = lock(&self.inner);
        let tx = inner
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = stream_on(&tx, caller, stream)?;
        let mut next = current.next_sequence;
        let mut receipts = Vec::new();
        let received = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| ActivityError::Storage(e.to_string()))?
            .as_millis() as u64;
        for event in events {
            let encoded = serde_json::to_string(event)?;
            if event.sequence < next {
                let previous:Option<String>=tx.query_row("SELECT event FROM activity_events WHERE caller=?1 AND stream=?2 AND sequence=?3",params![caller,stream,event.sequence],|r|r.get(0)).optional()?;
                let previous = previous.ok_or(ActivityError::Expired {
                    sequence: event.sequence,
                    retained_from: current.retained_from,
                })?;
                let previous: ActivityEvent = serde_json::from_str(&previous)?;
                if previous != *event {
                    return Err(ActivityError::Conflict);
                }
                receipts.push(ActivityReceipt {
                    sequence: event.sequence,
                    cursor: event.sequence + 1,
                    existing: true,
                });
                continue;
            }
            if event.sequence != next {
                return Err(ActivityError::Sequence {
                    expected: next,
                    received: event.sequence,
                });
            }
            tx.execute(
                "INSERT INTO activity_events VALUES (?1,?2,?3,?4,?5)",
                params![caller, stream, event.sequence, encoded, received],
            )?;
            next += 1;
            receipts.push(ActivityReceipt {
                sequence: event.sequence,
                cursor: next,
                existing: false,
            });
        }
        tx.execute(
            "UPDATE activity_streams SET next_sequence=?3 WHERE caller=?1 AND stream=?2",
            params![caller, stream, next],
        )?;
        let mut state = stream_on(&tx, caller, stream)?;
        while state.retained_events > limits.max_events || state.retained_bytes > limits.max_bytes {
            tx.execute(
                "DELETE FROM activity_events WHERE caller=?1 AND stream=?2 AND sequence=?3",
                params![caller, stream, state.retained_from - 1],
            )?;
            state = stream_on(&tx, caller, stream)?;
        }
        crate::fault::point("activity_before_commit");
        tx.commit()?;
        crate::fault::point("activity_committed");
        Ok(receipts)
    }

    pub(crate) fn read_activity(
        &self,
        caller: &str,
        stream: &str,
        after: u64,
        max_events: usize,
        max_bytes: usize,
    ) -> ActivityResult<ActivityPage> {
        let inner = lock(&self.inner);
        let state = stream_on(&inner.db, caller, stream)?;
        if after > state.next_sequence {
            return Err(ActivityError::Invalid(
                "cursor is beyond this stream".into(),
            ));
        }
        let start = after.max(state.retained_from - 1);
        let mut page = ActivityPage {
            gap: after < state.retained_from - 1,
            more: start < state.next_sequence,
            next_cursor: start,
            stream: state,
            entries: Vec::new(),
        };
        if serde_json::to_vec(&page)?.len() > max_bytes {
            return Err(ActivityError::Limit {
                resource: "page bytes",
                limit: max_bytes,
            });
        }
        let mut stmt=inner.db.prepare("SELECT sequence,event,received_at_ms FROM activity_events WHERE caller=?1 AND stream=?2 AND sequence>=?3 ORDER BY sequence LIMIT ?4")?;
        let rows = stmt.query_map(params![caller, stream, start, max_events], |r| {
            Ok((
                r.get::<_, u64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
            ))
        })?;
        for row in rows {
            let (sequence, encoded, received_at_ms) = row?;
            let event: ActivityEvent = serde_json::from_str(&encoded)?;
            if event.sequence != sequence {
                return Err(ActivityError::Storage(
                    "event sequence disagrees with stored identity".into(),
                ));
            }
            page.entries.push(ActivityEntry {
                cursor: sequence + 1,
                received_at_ms,
                event,
            });
            let old_cursor = page.next_cursor;
            page.next_cursor = sequence + 1;
            page.more = page.next_cursor < page.stream.next_sequence;
            if serde_json::to_vec(&page)?.len() > max_bytes {
                page.entries.pop();
                page.next_cursor = old_cursor;
                page.more = true;
                if page.entries.is_empty() {
                    return Err(ActivityError::Limit {
                        resource: "complete event page bytes",
                        limit: max_bytes,
                    });
                }
                break;
            }
        }
        Ok(page)
    }

    pub(crate) fn list_activity(&self, caller: &str) -> ActivityResult<Vec<ActivityStream>> {
        let inner = lock(&self.inner);
        let mut stmt = inner
            .db
            .prepare("SELECT stream FROM activity_streams WHERE caller=?1 ORDER BY stream")?;
        let rows = stmt.query_map([caller], |r| r.get::<_, String>(0))?;
        rows.map(|r| stream_on(&inner.db, caller, &r?)).collect()
    }

    pub(crate) fn forget_activity(
        &self,
        caller: &str,
        stream: &str,
        expected: u64,
    ) -> ActivityResult<()> {
        let mut inner = lock(&self.inner);
        let tx = inner
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = stream_on(&tx, caller, stream)?;
        if state.next_sequence != expected {
            return Err(ActivityError::Conflict);
        }
        tx.execute(
            "DELETE FROM activity_streams WHERE caller=?1 AND stream=?2",
            params![caller, stream],
        )?;
        tx.commit()?;
        Ok(())
    }
}
