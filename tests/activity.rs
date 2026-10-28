use rho_core::{ActivityError, ActivityEvent, ActivitySource, Core, CoreConfig, Limits};
use serde_json::json;
use std::sync::{Arc, Barrier};

fn source() -> ActivitySource {
    ActivitySource {
        stream_id: "session-a".into(),
        producer: "fixture".into(),
        session_id: "native-a".into(),
        metadata: json!({"normalizer":"fixture/1"}),
    }
}
fn event(sequence: u64) -> ActivityEvent {
    ActivityEvent {
        sequence,
        kind: "tool.returned".into(),
        data: json!({"call_id":format!("call-{sequence}"),"command":"touch must-not-exist", "result":"结果 α 🧪", "exit_code":7}),
    }
}
fn open(root: &std::path::Path, limits: Limits) -> Core {
    Core::open(CoreConfig::new(root, root.join("state")).limits(limits)).unwrap()
}

#[test]
fn observation_is_not_execution_and_scopes_are_independent() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(dir.path(), Limits::default());
    assert!(matches!(
        core.read_activity("a", "session-a", 0, 10, 65536),
        Err(ActivityError::NotFound)
    ));
    core.open_activity("a", source()).unwrap();
    let receipts = core.append_activity("a", "session-a", &[event(0)]).unwrap();
    assert!(!receipts[0].existing);
    assert!(!dir.path().join("must-not-exist").exists());
    assert!(core.list_operations("a").unwrap().is_empty());
    assert!(core.list_activity("b").unwrap().is_empty());
    assert!(matches!(
        core.append_activity("b", "session-a", &[event(0)]),
        Err(ActivityError::NotFound)
    ));
    let mut changed = source();
    changed.session_id = "different".into();
    assert_eq!(
        core.open_activity("a", changed),
        Err(ActivityError::Conflict)
    );
    core.open_activity("b", source()).unwrap();
    assert_eq!(
        core.read_activity("b", "session-a", 0, 10, 65536)
            .unwrap()
            .stream
            .next_sequence,
        0
    );
}

#[test]
fn concurrent_delivery_returns_one_original_receipt_and_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let core = Arc::new(open(dir.path(), Limits::default()));
    core.open_activity("a", source()).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let jobs: Vec<_> = (0..8)
        .map(|_| {
            let c = Arc::clone(&core);
            let b = Arc::clone(&barrier);
            std::thread::spawn(move || {
                b.wait();
                c.append_activity("a", "session-a", &[event(0)]).unwrap()[0].clone()
            })
        })
        .collect();
    let results: Vec<_> = jobs.into_iter().map(|j| j.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| !r.existing).count(), 1);
    assert!(results.iter().all(|r| r.cursor == 1));
    let before = core.read_activity("a", "session-a", 0, 10, 65536).unwrap();
    drop(core);
    let core = open(dir.path(), Limits::default());
    assert_eq!(
        core.read_activity("a", "session-a", 0, 10, 65536).unwrap(),
        before
    );
    assert!(core.append_activity("a", "session-a", &[event(0)]).unwrap()[0].existing);
    core.append_activity("a", "session-a", &[event(1)]).unwrap();
    assert_eq!(
        core.read_activity("a", "session-a", 1, 10, 65536)
            .unwrap()
            .entries[0]
            .event,
        event(1)
    );
}

#[test]
fn conflict_and_sequence_gaps_roll_back_whole_batches() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(dir.path(), Limits::default());
    core.open_activity("a", source()).unwrap();
    core.append_activity("a", "session-a", &[event(0)]).unwrap();
    let mut conflict = event(0);
    conflict.data = json!({"different":true});
    assert_eq!(
        core.append_activity("a", "session-a", &[event(1), conflict]),
        Err(ActivityError::Conflict)
    );
    assert!(matches!(
        core.append_activity("a", "session-a", &[event(1), event(3)]),
        Err(ActivityError::Sequence {
            expected: 2,
            received: 3
        })
    ));
    assert_eq!(core.list_activity("a").unwrap()[0].next_sequence, 1);
    core.append_activity("a", "session-a", &[event(1), event(2)])
        .unwrap();
    assert_eq!(
        core.forget_activity("a", "session-a", 1),
        Err(ActivityError::Conflict)
    );
    core.forget_activity("a", "session-a", 3).unwrap();
    assert!(core.list_activity("a").unwrap().is_empty());
}

#[test]
fn retention_exposes_loss_and_never_reaccepts_an_evicted_identity() {
    let dir = tempfile::tempdir().unwrap();
    let mut limits = Limits::default();
    limits.activity.max_events = 2;
    let core = open(dir.path(), limits.clone());
    core.open_activity("a", source()).unwrap();
    core.append_activity("a", "session-a", &[event(0), event(1), event(2)])
        .unwrap();
    let page = core.read_activity("a", "session-a", 0, 1, 65536).unwrap();
    assert!(page.gap && page.more);
    assert_eq!(page.next_cursor, 2);
    assert_eq!(page.stream.dropped_events, 1);
    assert_eq!(page.entries[0].event, event(1));
    drop(core);
    let core = open(dir.path(), limits);
    assert!(matches!(
        core.append_activity("a", "session-a", &[event(0)]),
        Err(ActivityError::Expired { sequence: 0, .. })
    ));
    assert!(core.append_activity("a", "session-a", &[event(2)]).unwrap()[0].existing);
    assert_eq!(
        core.read_activity("a", "session-a", 2, 10, 65536)
            .unwrap()
            .entries[0]
            .event,
        event(2)
    );
}

#[test]
fn byte_limits_preserve_complete_unicode_and_stream_watermarks() {
    let dir = tempfile::tempdir().unwrap();
    let mut limits = Limits::default();
    limits.activity.max_streams = 1;
    limits.activity.max_event_bytes = 1024;
    limits.activity.max_bytes = 1500;
    let core = open(dir.path(), limits);
    core.open_activity("a", source()).unwrap();
    assert!(matches!(
        core.open_activity("b", source()),
        Err(ActivityError::Limit {
            resource: "streams",
            ..
        })
    ));
    let mut big = event(0);
    big.data = json!("结果🧪".repeat(150));
    assert!(matches!(
        core.append_activity("a", "session-a", &[big]),
        Err(ActivityError::Limit {
            resource: "event bytes",
            ..
        })
    ));
    assert_eq!(core.list_activity("a").unwrap()[0].next_sequence, 0);
    let events: Vec<_> = (0..3)
        .map(|i| ActivityEvent {
            sequence: i,
            kind: "tool.returned".into(),
            data: json!("结果🧪".repeat(60)),
        })
        .collect();
    core.append_activity("a", "session-a", &events).unwrap();
    let page = core.read_activity("a", "session-a", 0, 100, 65536).unwrap();
    assert!(page.gap);
    assert!(page.stream.retained_bytes <= 1500);
    assert_eq!(page.entries.last().unwrap().event, events[2]);
    assert!(matches!(
        core.read_activity("a", "session-a", 0, 100, 400),
        Err(ActivityError::Limit { .. })
    ));
    let bytes = serde_json::to_vec(&page).unwrap().len();
    let bounded = core.read_activity("a", "session-a", 0, 100, bytes).unwrap();
    assert_eq!(bounded, page);
}

#[test]
fn corrupted_activity_is_not_an_empty_history() {
    let dir = tempfile::tempdir().unwrap();
    let core = open(dir.path(), Limits::default());
    core.open_activity("a", source()).unwrap();
    core.append_activity("a", "session-a", &[event(0)]).unwrap();
    drop(core);
    let db = rusqlite::Connection::open(dir.path().join("state/core.sqlite3")).unwrap();
    db.execute("UPDATE activity_events SET event='{}'", [])
        .unwrap();
    drop(db);
    let core = open(dir.path(), Limits::default());
    assert!(matches!(
        core.read_activity("a", "session-a", 0, 10, 65536),
        Err(ActivityError::Storage(_))
    ));
    assert!(matches!(
        core.append_activity("a", "session-a", &[event(0)]),
        Err(ActivityError::Storage(_))
    ));
}

#[test]
fn activity_crash_child() {
    let Ok(path) = std::env::var("RHO_ACTIVITY_FIXTURE") else {
        return;
    };
    let core = open(std::path::Path::new(&path), Limits::default());
    core.append_activity("a", "session-a", &[event(1)]).unwrap();
    panic!("fault point did not stop the child");
}

#[test]
fn crash_before_and_after_commit_preserves_the_original_source_position() {
    for (point, expected) in [("activity_before_commit", 1), ("activity_committed", 2)] {
        let dir = tempfile::tempdir().unwrap();
        let core = open(dir.path(), Limits::default());
        core.open_activity("a", source()).unwrap();
        core.append_activity("a", "session-a", &[event(0)]).unwrap();
        drop(core);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "activity_crash_child", "--nocapture"])
            .env("RHO_ACTIVITY_FIXTURE", dir.path())
            .env("RHO_CORE_CRASH_AT", point)
            .status()
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(status.signal(), Some(9));
        }
        #[cfg(windows)]
        {
            assert_eq!(status.code(), Some(0x4000_000F));
        }
        let core = open(dir.path(), Limits::default());
        assert_eq!(core.list_activity("a").unwrap()[0].next_sequence, expected);
        let receipt = core.append_activity("a", "session-a", &[event(1)]).unwrap();
        assert_eq!(receipt[0].existing, expected == 2);
        assert_eq!(core.list_activity("a").unwrap()[0].next_sequence, 2);
        assert!(core.list_operations("a").unwrap().is_empty());
    }
}
