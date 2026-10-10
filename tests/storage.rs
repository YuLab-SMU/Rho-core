//! Public-boundary checks of transaction failures and independent durable facts.
use std::fs;
use std::time::Duration;

use rho_core::{
    Core, CoreConfig, Disposition, ExecutionFact, Executor, ForgetError, Knowledge, LookupError,
    RunRequest, RunStatus, Stream, SubmitError,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

const CALLER: &str = "agent";
const PATIENCE: Duration = Duration::from_secs(10);
const CODE: &str = "echo $$ >> dispatches.log\nprintf 'output'\nprintf 'error' >&2\n";
struct Fixture {
    _tmp: tempfile::TempDir,
    config: CoreConfig,
    core: Core,
    db: Connection,
}
impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        fs::create_dir(&project).unwrap();
        let config = CoreConfig::new(&project, tmp.path().join("state"))
            .executor(Executor::new("sh", "/bin/sh"));
        let core = Core::open(config.clone()).unwrap();
        let db = Connection::open(core.capability().state_dir.join("core.sqlite3")).unwrap();
        Self {
            _tmp: tmp,
            config,
            core,
            db,
        }
    }
    fn dispatches(&self) -> usize {
        fs::read_to_string(self.config.project_root.join("dispatches.log"))
            .map_or(0, |s| s.lines().count())
    }
    fn run(&self, id: &str) -> rho_core::RunView {
        self.core.submit(CALLER, request(id, CODE)).unwrap();
        let view = self.core.wait(CALLER, id, PATIENCE).unwrap();
        assert!(view.is_terminal(), "{view:?}");
        view
    }
    fn trigger(&self, sql: &str) {
        self.db.execute_batch(sql).unwrap();
    }
}
fn request(id: &str, code: &str) -> RunRequest {
    RunRequest {
        request_id: id.into(),
        executor: "sh".into(),
        workdir: ".".into(),
        code: code.into(),
        args: vec![],
    }
}
fn reject_update(column: &str) -> String {
    format!(
        "CREATE TRIGGER reject_{column} BEFORE UPDATE OF {column} ON operations BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;"
    )
}

#[test]
fn failed_acceptance_transaction_has_no_native_effect_and_can_be_retried() {
    let fx = Fixture::new();
    fx.trigger("CREATE TRIGGER reject_accept BEFORE INSERT ON operations BEGIN SELECT RAISE(ABORT,'fixture acceptance failure'); END;");
    assert!(matches!(
        fx.core.submit(CALLER, request("job", CODE)),
        Err(SubmitError::Storage(_))
    ));
    assert!(matches!(
        fx.core.lookup(CALLER, "job"),
        Err(LookupError::NotFound { .. })
    ));
    assert_eq!(fx.dispatches(), 0);
    let count: usize = fx
        .db
        .query_row("SELECT count(*) FROM operations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    fx.trigger("DROP TRIGGER reject_accept;");
    fx.run("job");
    assert_eq!(fx.dispatches(), 1);
}

#[test]
fn a_dispatch_marker_that_cannot_commit_never_starts_the_executor() {
    let fx = Fixture::new();
    fx.trigger(&reject_update("dispatch"));
    let submitted = fx.core.submit(CALLER, request("job", CODE)).unwrap();
    assert!(matches!(submitted.run.status, RunStatus::NotStarted { .. }));
    assert!(
        submitted
            .run
            .unsaved
            .as_deref()
            .unwrap()
            .contains("dispatch")
    );
    assert_eq!(fx.dispatches(), 0);
    fx.trigger("DROP TRIGGER reject_dispatch;");
    let again = fx.core.submit(CALLER, request("job", CODE)).unwrap();
    assert_eq!(again.disposition, Disposition::Existing);
    assert!(again.run.unsaved.is_none());
    assert_eq!(fx.dispatches(), 0);
}

#[test]
fn exit_write_failure_preserves_durable_output_and_does_not_reexecute() {
    let fx = Fixture::new();
    fx.trigger(&reject_update("execution"));
    let view = fx.run("job");
    assert!(matches!(
        view.operation.execution.value(),
        Some(ExecutionFact::ExitObserved { .. })
    ));
    assert!(view.unsaved.as_deref().unwrap().contains("execution"));
    assert_eq!(view.stdout.retained_bytes, 6);
    assert!(matches!(
        fx.core.forget(CALLER, "job"),
        Err(ForgetError::NotTerminal(_))
    ));
    let config = fx.config.clone();
    drop(fx.core);
    let core = Core::open(config).unwrap();
    let found = core.lookup(CALLER, "job").unwrap();
    assert!(matches!(
        found.operation.execution,
        Knowledge::Unknown { .. }
    ));
    assert!(!found.is_terminal());
    assert_eq!(
        core.read_output(CALLER, "job", Stream::Stdout, 0, 100)
            .unwrap()
            .bytes,
        b"output"
    );
    assert_eq!(
        core.submit(CALLER, request("job", CODE))
            .unwrap()
            .disposition,
        Disposition::Existing
    );
    assert_eq!(
        fs::read_to_string(fx.config.project_root.join("dispatches.log"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn a_failed_output_transaction_does_not_hide_exit_or_other_streams() {
    let fx = Fixture::new();
    fx.trigger(&reject_update("stdout"));
    let view = fx.run("job");
    assert!(matches!(view.status, RunStatus::Finished { .. }));
    assert!(view.unsaved.as_deref().unwrap().contains("stdout"));
    assert_eq!(view.stderr.retained_bytes, 5);
    let saved: String = fx
        .db
        .query_row("SELECT stdout FROM operations", [], |r| r.get(0))
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let info: rho_core::StreamInfo = serde_json::from_value(metadata["info"].clone()).unwrap();
    assert_eq!(info.retained_bytes, 0);
    assert!(matches!(
        fx.core.forget(CALLER, "job"),
        Err(ForgetError::NotTerminal(_))
    ));
    // Retry only the unsaved metadata, never the native process.
    fx.trigger("DROP TRIGGER reject_stdout;");
    let found = fx.core.lookup(CALLER, "job").unwrap();
    assert!(found.unsaved.is_none());
    assert_eq!(
        fx.core
            .read_output(CALLER, "job", Stream::Stdout, 0, 100)
            .unwrap()
            .bytes,
        b"output"
    );
    assert_eq!(fx.dispatches(), 1);
}

#[test]
fn output_blob_failure_reports_loss_but_preserves_native_exit() {
    let fx = Fixture::new();
    let digest: String = Sha256::digest(b"output")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    // Real filesystem failure for this output prefix, independent of SQLite.
    fs::write(
        fx.core
            .capability()
            .state_dir
            .join("blobs/sha256")
            .join(&digest[..2]),
        b"not a directory",
    )
    .unwrap();
    let view = fx.run("job");
    assert!(matches!(
        view.status,
        RunStatus::Finished {
            exit: rho_core::Exit::Code(0),
            ..
        }
    ));
    assert_eq!(view.stdout.retained_bytes, 0);
    assert_eq!(view.stdout.observed_bytes, 6);
    assert!(view.stdout.retention_error.is_some());
    assert_eq!(view.stderr.retained_bytes, 5);
    assert!(view.unsaved.is_none());
    assert_eq!(fx.dispatches(), 1);
}

#[test]
fn damaged_output_is_never_served_as_the_original_bytes() {
    let fx = Fixture::new();
    let view = fx.run("job");
    let blob = &view.operation.outputs.stdout.blobs[0];
    let path = fx
        .core
        .capability()
        .state_dir
        .join("blobs/sha256")
        .join(&blob.sha256[..2])
        .join(&blob.sha256);
    fs::write(path, b"broken").unwrap(); // same size; only digest detects this.
    let found = fx.core.lookup(CALLER, "job").unwrap();
    assert!(matches!(found.status, RunStatus::Finished { .. }));
    assert!(matches!(
        fx.core.read_output(CALLER, "job", Stream::Stdout, 0, 100),
        Err(rho_core::ReadError::Io(_))
    ));
    assert_eq!(
        fx.core
            .submit(CALLER, request("job", CODE))
            .unwrap()
            .disposition,
        Disposition::Existing
    );
    assert_eq!(fx.dispatches(), 1);
}

#[test]
fn corrupt_row_is_an_error_even_if_the_core_has_cached_the_acceptance() {
    let fx = Fixture::new();
    fx.run("job");
    fx.trigger("UPDATE operations SET accepted='{ damaged json';");
    assert!(matches!(
        fx.core.lookup(CALLER, "job"),
        Err(LookupError::RecordUnreadable { .. })
    ));
    assert!(matches!(
        fx.core.submit(CALLER, request("job", CODE)),
        Err(SubmitError::RecordUnreadable { .. })
    ));
    assert!(matches!(
        fx.core.read_output(CALLER, "job", Stream::Stdout, 0, 10),
        Err(rho_core::ReadError::Lookup(
            LookupError::RecordUnreadable { .. }
        ))
    ));
    assert!(matches!(
        fx.core.forget(CALLER, "job"),
        Err(ForgetError::Lookup(LookupError::RecordUnreadable { .. }))
    ));
    assert_eq!(fx.core.unreadable_records().len(), 1);
    assert_eq!(fx.dispatches(), 1);
}

#[test]
fn failed_delete_transaction_preserves_the_key_until_explicit_commit() {
    let fx = Fixture::new();
    let original = fx.run("job");
    fx.trigger("CREATE TRIGGER reject_delete BEFORE DELETE ON operations BEGIN SELECT RAISE(ABORT,'fixture deletion failure'); END;");
    assert!(matches!(
        fx.core.forget(CALLER, "job"),
        Err(ForgetError::Io(_))
    ));
    let found = fx.core.submit(CALLER, request("job", CODE)).unwrap();
    assert_eq!(found.disposition, Disposition::Existing);
    assert_eq!(found.run.run_id, original.run_id);
    assert_eq!(fx.dispatches(), 1);
    fx.trigger("DROP TRIGGER reject_delete;");
    fx.core.forget(CALLER, "job").unwrap();
    let next = fx.core.submit(CALLER, request("job", CODE)).unwrap();
    assert_eq!(next.disposition, Disposition::Accepted);
    assert_ne!(next.run.run_id, original.run_id);
    assert!(fx.core.wait(CALLER, "job", PATIENCE).unwrap().is_terminal());
    assert_eq!(fx.dispatches(), 2);
}

#[test]
fn a_corrupt_database_is_not_initialized_as_an_empty_store() {
    let fx = Fixture::new();
    fx.run("job");
    let config = fx.config.clone();
    drop(fx.core);
    drop(fx.db);
    fs::write(
        config.state_dir.join("core.sqlite3"),
        b"not a SQLite database",
    )
    .unwrap();
    assert!(Core::open(config.clone()).is_err());
    assert_eq!(
        fs::read_to_string(config.project_root.join("dispatches.log"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    fs::write(config.state_dir.join("core.sqlite3"), b"").unwrap();
    let error = Core::open(config).err().unwrap();
    assert!(error.to_string().contains("truncated"));
}

#[test]
fn an_unimported_file_store_is_never_treated_as_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    let state = tmp.path().join("state");
    fs::create_dir(&project).unwrap();
    fs::create_dir_all(state.join("records")).unwrap();
    let error = Core::open(CoreConfig::new(project, &state)).err().unwrap();
    assert!(error.to_string().contains("legacy file store"));
    assert!(!state.join("core.sqlite3").exists());
}

#[test]
fn sqlite_online_backup_keeps_committed_metadata_and_artifact_references() {
    let fx = Fixture::new();
    let view = fx.run("job");
    let mode: String = fx
        .db
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    // Hot backup through SQLite includes committed WAL frames; copying only
    // core.sqlite3 does not provide this guarantee. Full backups also need blobs.
    let path = fx._tmp.path().join("metadata-backup.sqlite3");
    fx.db.backup("main", &path, None).unwrap();
    let backup = Connection::open(path).unwrap();
    let (id, count): (String, usize) = backup
        .query_row(
            "SELECT operation_id,(SELECT count(*) FROM artifacts) FROM operations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(id, view.run_id);
    assert_eq!(count, 3); // immutable script, stdout and stderr.
    assert_eq!(
        backup
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn output_chunk_limit_bounds_metadata_and_reports_the_exhausted_resource() {
    let fx = Fixture::new();
    let mut config = fx.config.clone();
    drop(fx.core);
    config.limits.max_output_chunks = 1;
    config.limits.max_stream_bytes = 256 * 1024;
    let core = Core::open(config).unwrap();
    core.submit(
        CALLER,
        request("many", "perl -e 'print \"x\" x (128*1024)'\n"),
    )
    .unwrap();
    let view = core.wait(CALLER, "many", PATIENCE).unwrap();
    assert!(view.is_terminal());
    assert_eq!(view.operation.outputs.stdout.blobs.len(), 1);
    assert_eq!(view.stdout.observed_bytes, 128 * 1024);
    assert!(view.stdout.retained_bytes <= 64 * 1024);
    assert!(
        view.stdout
            .retention_error
            .as_deref()
            .unwrap()
            .contains("chunk limit")
    );
}

#[test]
fn callers_can_discover_their_original_operations_after_restart() {
    let fx = Fixture::new();
    fx.run("one");
    fx.run("two");
    fx.core.submit("other-agent", request("one", CODE)).unwrap();
    assert!(
        fx.core
            .wait("other-agent", "one", PATIENCE)
            .unwrap()
            .is_terminal()
    );
    let config = fx.config.clone();
    drop(fx.core);
    let core = Core::open(config).unwrap();
    let found = core.list_operations(CALLER).unwrap();
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|v| v.request.caller == CALLER));
    assert_eq!(core.list_operations("other-agent").unwrap().len(), 1);
    assert!(core.list_operations("unrelated-agent").unwrap().is_empty());
}

#[test]
fn forgetting_another_operation_preserves_blobs_needed_by_unsaved_output() {
    let fx = Fixture::new();
    fx.run("first");
    fx.trigger(&reject_update("stdout"));
    let pending = fx.run("pending");
    assert!(pending.unsaved.is_some());
    fx.core.forget(CALLER, "first").unwrap();
    assert_eq!(
        fx.core
            .read_output(CALLER, "pending", Stream::Stdout, 0, 100)
            .unwrap()
            .bytes,
        b"output"
    );
    fx.trigger("DROP TRIGGER reject_stdout;");
    assert!(fx.core.lookup(CALLER, "pending").unwrap().unsaved.is_none());
    fx.core.forget(CALLER, "pending").unwrap();
}

#[test]
fn empty_script_snapshots_are_referenced_and_survive_other_acceptances() {
    let fx = Fixture::new();
    fx.core.submit(CALLER, request("empty-one", "")).unwrap();
    fx.core.submit(CALLER, request("empty-two", "")).unwrap();
    for id in ["empty-one", "empty-two"] {
        let view = fx.core.wait(CALLER, id, PATIENCE).unwrap();
        assert!(
            matches!(
                view.status,
                RunStatus::Finished {
                    exit: rho_core::Exit::Code(0),
                    ..
                }
            ),
            "{view:?}"
        );
        assert_eq!(view.operation.script.bytes, 0);
    }
    assert_eq!(
        fx.db
            .query_row(
                "SELECT count(*) FROM artifacts WHERE kind='script' AND bytes=0",
                [],
                |r| r.get::<_, usize>(0)
            )
            .unwrap(),
        2
    );
    fx.core.forget(CALLER, "empty-one").unwrap();
    assert_eq!(
        fx.core
            .submit(CALLER, request("empty-two", ""))
            .unwrap()
            .disposition,
        Disposition::Existing
    );
}

#[test]
fn concurrent_new_keys_share_the_global_record_capacity() {
    let fx = Fixture::new();
    let mut config = fx.config.clone();
    drop(fx.core);
    config.limits.max_records = 1;
    let core = Core::open(config).unwrap();
    let barrier = std::sync::Barrier::new(8);
    let accepted = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let core = &core;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    core.submit(CALLER, request(&format!("job-{i}"), CODE))
                })
            })
            .collect();
        let mut accepted = Vec::new();
        for handle in handles {
            match handle.join().unwrap() {
                Ok(run) => {
                    assert_eq!(run.disposition, Disposition::Accepted);
                    accepted.push(run.run.request.request_id);
                }
                Err(SubmitError::Capacity {
                    resource: "accepted request records",
                    limit: 1,
                }) => {}
                result => panic!("unexpected capacity result: {result:?}"),
            }
        }
        accepted
    });
    assert_eq!(accepted.len(), 1);
    assert!(
        core.wait(CALLER, &accepted[0], PATIENCE)
            .unwrap()
            .is_terminal()
    );
    assert_eq!(
        fs::read_to_string(fx.config.project_root.join("dispatches.log"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn corrupt_metadata_does_not_prevent_shutdown_from_confirming_owned_process_stop() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let fx = Fixture::new();
    let mut config = fx.config.clone();
    drop(fx.core);
    config.limits.stop_grace = Duration::from_millis(50);
    let core = Core::open(config).unwrap();
    let ready = fx._tmp.path().join("ready");
    let release = fx._tmp.path().join("release");
    for path in [&ready, &release] {
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: valid NUL-terminated fixture path, no pointers retained.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
    let mut request = request(
        "owned",
        "trap '' TERM\necho ready > \"$1\"\nread line < \"$2\"\n",
    );
    request.args = vec![ready.display().to_string(), release.display().to_string()];
    let submitted = core.submit(CALLER, request).unwrap();
    let pid = match submitted.run.status {
        RunStatus::Running { pid, .. } => pid,
        ref status => panic!("{status:?}"),
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || sender.send(fs::read_to_string(ready).unwrap()).unwrap());
    assert_eq!(receiver.recv_timeout(PATIENCE).unwrap(), "ready\n");
    fx.db
        .execute_batch("UPDATE operations SET accepted='{ corrupted metadata';")
        .unwrap();
    let report = core.shutdown();
    assert_eq!(report.stopped, vec![submitted.run.run_id]);
    assert!(report.not_confirmed.is_empty());
    // SAFETY: signal 0 only observes existence; no process is signalled.
    assert_ne!(unsafe { libc::kill(pid as i32, 0) }, 0);
}
