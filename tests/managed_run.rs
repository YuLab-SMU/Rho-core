//! Behavior checks for the first managed flow, through the public boundary only.
//!
//! Fixture tasks are real `/bin/sh` processes. Each appends its pid to
//! `dispatches.log` in its working directory, so dispatch counts come from the
//! native side, not from Core. Timing is synchronised with FIFOs, not sleeps.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Barrier, mpsc};
use std::thread;
use std::time::Duration;

use rho_core::{
    CancelReason, Core, CoreConfig, Disposition, Executor, Exit, Limits, LookupError, ReadError,
    RunRequest, RunStatus, RunView, Stream, SubmitError, WorkdirProblem,
};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const CALLER: &str = "agent-a";
const PATIENCE: Duration = Duration::from_secs(20);

const SUM: &str = r#"echo "$$" >> dispatches.log
n="$1"; i=1; : > numbers.txt
while [ "$i" -le "$n" ]; do echo "$i" >> numbers.txt; i=$((i + 1)); done
awk '{ s += $1 } END { print "sum=" s }' numbers.txt
"#;

const GROUP_COUNT: &str = r#"echo "$$" >> dispatches.log
printf '%s\n' "$@" | sort | uniq -c | awk '{ print $2 "\t" $1 }' > counts.tsv
cat counts.tsv
"#;

/// Signals `$1` once running, then blocks until a line arrives on `$2`.
const GATED: &str = r#"echo "$$" >> dispatches.log
echo started > "$1"
read line < "$2"
echo "released:$line"
"#;

/// Leaves a background member in its process group, then waits like GATED.
const GATED_WITH_CHILD: &str = r#"echo "$$" >> dispatches.log
sleep 300 &
echo "$!" > background.pid
echo started > "$1"
read line < "$2"
"#;

struct Fixture {
    // Dropped first so Core stops its processes before the directories go.
    core: Core,
    work: PathBuf,
    root: PathBuf,
    _tmp: TempDir,
}

fn fixture() -> Fixture {
    fixture_with(Limits::default(), Executor::new("sh", "/bin/sh"))
}

fn fixture_with(limits: Limits, executor: Executor) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let project = root.join("project");
    let work = project.join("work");
    fs::create_dir_all(&work).unwrap();
    let config = CoreConfig::new(&project, root.join("state"))
        .executor(executor)
        .limits(limits);
    let core = Core::open(config).unwrap();
    Fixture {
        core,
        work,
        root,
        _tmp: tmp,
    }
}

fn request(id: &str, code: &str, args: &[&str]) -> RunRequest {
    RunRequest {
        request_id: id.into(),
        executor: "sh".into(),
        workdir: "work".into(),
        code: code.into(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    }
}

fn dispatches(dir: &Path) -> Vec<String> {
    match fs::read_to_string(dir.join("dispatches.log")) {
        Ok(text) => text.lines().map(str::to_owned).collect(),
        Err(_) => Vec::new(),
    }
}

fn pid_of(view: &RunView) -> u32 {
    match view.status {
        RunStatus::Running { pid, .. } | RunStatus::Finished { pid, .. } => pid,
        ref other => panic!("no process for {other:?}"),
    }
}

fn exit_of(view: &RunView) -> Exit {
    match &view.status {
        RunStatus::Finished { exit, .. } => exit.clone(),
        other => panic!("run not finished: {other:?}"),
    }
}

fn finished(core: &Core, id: &str) -> RunView {
    let view = core.wait(CALLER, id, PATIENCE).unwrap();
    assert!(view.is_terminal(), "run {id} did not finish: {view:?}");
    view
}

fn stdout(core: &Core, id: &str) -> String {
    let chunk = core
        .read_output(CALLER, id, Stream::Stdout, 0, usize::MAX)
        .unwrap();
    assert_eq!(chunk.bytes.len() as u64, chunk.info.retained_bytes);
    String::from_utf8(chunk.bytes).unwrap()
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// A started/gate FIFO pair; blocking FIFO operations run under a timeout.
struct Gate {
    started: PathBuf,
    release: PathBuf,
}

impl Gate {
    fn new(dir: &Path, name: &str) -> Self {
        let gate = Self {
            started: dir.join(format!("{name}.started")),
            release: dir.join(format!("{name}.release")),
        };
        for path in [&gate.started, &gate.release] {
            let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        }
        gate
    }

    fn args(&self) -> Vec<String> {
        vec![
            self.started.display().to_string(),
            self.release.display().to_string(),
        ]
    }

    fn wait_started(&self) {
        let path = self.started.clone();
        let text = within("task start", move || fs::read_to_string(path).unwrap());
        assert_eq!(text, "started\n");
    }

    fn open(&self, line: &str) {
        let path = self.release.clone();
        let line = format!("{line}\n");
        within("gate release", move || fs::write(path, line).unwrap());
    }
}

fn within<T: Send + 'static>(what: &str, action: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(action());
    });
    receiver
        .recv_timeout(PATIENCE)
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

fn gated(id: &str, gate: &Gate, code: &str) -> RunRequest {
    RunRequest {
        args: gate.args(),
        ..request(id, code, &[])
    }
}

#[test]
fn two_task_scripts_share_one_capability_and_return_real_products() {
    let fx = fixture();
    let sum = fx
        .core
        .submit(CALLER, request("sum-100", SUM, &["100"]))
        .unwrap();
    let groups = fx
        .core
        .submit(
            CALLER,
            request("groups", GROUP_COUNT, &["b", "a", "b", "c", "b"]),
        )
        .unwrap();
    assert_eq!(sum.disposition, Disposition::Accepted);
    assert_eq!(groups.disposition, Disposition::Accepted);

    let sum_view = finished(&fx.core, "sum-100");
    assert_eq!(exit_of(&sum_view), Exit::Code(0));
    assert_eq!(stdout(&fx.core, "sum-100"), "sum=5050\n");
    let numbers = fs::read_to_string(fx.work.join("numbers.txt")).unwrap();
    assert_eq!(numbers.lines().count(), 100);
    assert_eq!(
        numbers
            .lines()
            .map(|n| n.parse::<u64>().unwrap())
            .sum::<u64>(),
        5050
    );

    let groups_view = finished(&fx.core, "groups");
    assert_eq!(exit_of(&groups_view), Exit::Code(0));
    assert_eq!(stdout(&fx.core, "groups"), "a\t1\nb\t3\nc\t1\n");
    assert_eq!(
        fs::read_to_string(fx.work.join("counts.tsv")).unwrap(),
        "a\t1\nb\t3\nc\t1\n"
    );

    // The original request is disclosed as accepted.
    let request = &sum_view.request;
    assert_eq!(
        (request.caller.as_str(), request.request_id.as_str()),
        (CALLER, "sum-100")
    );
    assert_eq!(request.executor, "sh");
    assert_eq!(request.program, PathBuf::from("/bin/sh"));
    assert_eq!(request.workdir, PathBuf::from("work"));
    assert_eq!(request.workdir_resolved, fx.work);
    assert_eq!(request.args, vec!["100".to_string()]);
    assert_eq!(request.code_bytes, SUM.len());
    let digest: String = Sha256::digest(SUM.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(request.code_sha256, digest);
    assert_ne!(sum_view.run_id, groups_view.run_id);

    // Exactly one native dispatch each, and the pids match what Core reports.
    let mut expected = vec![
        pid_of(&sum_view).to_string(),
        pid_of(&groups_view).to_string(),
    ];
    let mut logged = dispatches(&fx.work);
    expected.sort();
    logged.sort();
    assert_eq!(logged, expected);
}

#[test]
fn a_waiter_can_leave_and_another_finds_the_same_run() {
    let fx = fixture();
    let gate = Gate::new(&fx.root, "job");
    let core = &fx.core;

    // Waiter A submits, waits briefly while the task is held at its gate, and leaves.
    let (submitted, seen_by_a) = thread::scope(|scope| {
        scope
            .spawn(|| {
                let submitted = core.submit(CALLER, gated("job-1", &gate, GATED)).unwrap();
                gate.wait_started();
                let seen = core
                    .wait(CALLER, "job-1", Duration::from_millis(100))
                    .unwrap();
                (submitted, seen)
            })
            .join()
            .unwrap()
    });
    assert_eq!(submitted.disposition, Disposition::Accepted);
    assert!(matches!(seen_by_a.status, RunStatus::Running { .. }));

    // Waiter B, on another thread, locates the original run by the caller-held identity.
    let seen_by_b = thread::scope(|scope| {
        scope
            .spawn(|| {
                let current = core.lookup(CALLER, "job-1").unwrap();
                assert!(matches!(current.status, RunStatus::Running { .. }));
                gate.open("go");
                core.wait(CALLER, "job-1", PATIENCE).unwrap()
            })
            .join()
            .unwrap()
    });
    assert_eq!(seen_by_b.run_id, submitted.run.run_id);
    assert_eq!(pid_of(&seen_by_b), pid_of(&seen_by_a));
    assert_eq!(exit_of(&seen_by_b), Exit::Code(0));
    assert_eq!(stdout(core, "job-1"), "released:go\n");
    assert_eq!(dispatches(&fx.work), vec![pid_of(&seen_by_b).to_string()]);
}

#[test]
fn concurrent_duplicates_share_one_acceptance_and_one_dispatch() {
    let fx = fixture();
    let gate = Gate::new(&fx.root, "dup");
    let contenders = 16;
    let barrier = Barrier::new(contenders);
    let results: Vec<_> = thread::scope(|scope| {
        let handles: Vec<_> = (0..contenders)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    fx.core.submit(CALLER, gated("dup", &gate, GATED)).unwrap()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect()
    });
    let accepted = results
        .iter()
        .filter(|result| result.disposition == Disposition::Accepted)
        .count();
    assert_eq!(accepted, 1);
    assert!(
        results
            .iter()
            .all(|result| result.run.run_id == results[0].run.run_id)
    );

    gate.wait_started();
    gate.open("once");
    let view = finished(&fx.core, "dup");
    assert_eq!(dispatches(&fx.work), vec![pid_of(&view).to_string()]);

    // A sequential retry after completion still returns the original.
    let again = fx.core.submit(CALLER, gated("dup", &gate, GATED)).unwrap();
    assert_eq!(again.disposition, Disposition::Existing);
    assert_eq!(again.run.run_id, view.run_id);
    assert_eq!(stdout(&fx.core, "dup"), "released:once\n");
    assert_eq!(dispatches(&fx.work).len(), 1);
}

#[test]
fn same_identity_with_a_different_request_is_rejected() {
    let fx = fixture();
    fx.core.submit(CALLER, request("sum", SUM, &["5"])).unwrap();
    let original = finished(&fx.core, "sum");

    let changed_args = fx
        .core
        .submit(CALLER, request("sum", SUM, &["6"]))
        .unwrap_err();
    let changed_code = fx
        .core
        .submit(CALLER, request("sum", GROUP_COUNT, &["5"]))
        .unwrap_err();
    let changed_workdir = fx
        .core
        .submit(
            CALLER,
            RunRequest {
                workdir: "./work".into(),
                ..request("sum", SUM, &["5"])
            },
        )
        .unwrap_err();
    for error in [changed_args, changed_code, changed_workdir] {
        match error {
            SubmitError::Conflict { existing } => assert_eq!(*existing, original),
            other => panic!("expected conflict, got {other:?}"),
        }
    }
    assert_eq!(fx.core.lookup(CALLER, "sum").unwrap(), original);
    assert_eq!(stdout(&fx.core, "sum"), "sum=15\n");
    assert_eq!(dispatches(&fx.work).len(), 1);
}

#[test]
fn a_duplicate_is_matched_on_the_original_request_not_on_current_files() {
    let fx = fixture();
    let scratch = fx.work.parent().unwrap().join("scratch");
    fs::create_dir(&scratch).unwrap();
    let request = RunRequest {
        workdir: "scratch".into(),
        ..request("once", SUM, &["4"])
    };
    fx.core.submit(CALLER, request.clone()).unwrap();
    let original = finished(&fx.core, "once");
    fs::remove_dir_all(&scratch).unwrap();

    // The directory is gone now; the identity still resolves to the accepted run.
    let again = fx.core.submit(CALLER, request).unwrap();
    assert_eq!(again.disposition, Disposition::Existing);
    assert_eq!(again.run, original);
    assert_eq!(stdout(&fx.core, "once"), "sum=10\n");
}

#[test]
fn wrong_objects_and_scopes_are_rejected_without_execution() {
    let fx = fixture();
    let outside = fx.root.join("outside");
    fs::create_dir(&outside).unwrap();
    let project = fx.work.parent().unwrap();
    std::os::unix::fs::symlink(&outside, project.join("escape")).unwrap();
    fs::write(project.join("file.txt"), "not a directory").unwrap();

    let attempt = |workdir: &str| {
        let request = RunRequest {
            workdir: workdir.into(),
            ..request("probe", SUM, &["3"])
        };
        fx.core.submit(CALLER, request).unwrap_err()
    };
    let problem = |error: SubmitError| match error {
        SubmitError::Workdir { problem, .. } => problem,
        other => panic!("expected workdir rejection, got {other:?}"),
    };
    assert_eq!(
        problem(attempt("../outside")),
        WorkdirProblem::OutsideProject {
            resolved: outside.clone()
        }
    );
    assert_eq!(
        problem(attempt("escape")),
        WorkdirProblem::OutsideProject {
            resolved: outside.clone()
        }
    );
    assert_eq!(problem(attempt("missing")), WorkdirProblem::NotFound);
    assert_eq!(problem(attempt("file.txt")), WorkdirProblem::NotADirectory);
    assert_eq!(
        problem(attempt(outside.to_str().unwrap())),
        WorkdirProblem::NotRelative
    );
    assert_eq!(problem(attempt("")), WorkdirProblem::NotRelative);

    let unknown = RunRequest {
        executor: "python".into(),
        ..request("probe", SUM, &["3"])
    };
    assert_eq!(
        fx.core.submit(CALLER, unknown).unwrap_err(),
        SubmitError::UnknownExecutor("python".into())
    );
    assert!(matches!(
        fx.core.submit(CALLER, request("bad id", SUM, &["3"])),
        Err(SubmitError::InvalidRequest(_))
    ));
    assert!(matches!(
        fx.core.submit("", request("probe", SUM, &["3"])),
        Err(SubmitError::InvalidRequest(_))
    ));

    // Rejections leave no record and no native effect.
    assert!(matches!(
        fx.core.lookup(CALLER, "probe"),
        Err(LookupError::NotFound { .. })
    ));
    assert!(dispatches(&outside).is_empty());
    assert!(dispatches(&fx.work).is_empty());

    // Identities are scoped per caller: another caller neither sees nor reuses this one.
    fx.core
        .submit(CALLER, request("shared-id", SUM, &["2"]))
        .unwrap();
    finished(&fx.core, "shared-id");
    assert!(matches!(
        fx.core.lookup("agent-b", "shared-id"),
        Err(LookupError::NotFound { .. })
    ));
    assert!(matches!(
        fx.core.cancel("agent-b", "shared-id"),
        Err(LookupError::NotFound { .. })
    ));
    let other = fx
        .core
        .submit("agent-b", request("shared-id", SUM, &["2"]))
        .unwrap();
    assert_eq!(other.disposition, Disposition::Accepted);
    let other_view = fx.core.wait("agent-b", "shared-id", PATIENCE).unwrap();
    assert_ne!(
        other_view.run_id,
        fx.core.lookup(CALLER, "shared-id").unwrap().run_id
    );
    assert_eq!(dispatches(&fx.work).len(), 2);
}

#[test]
fn output_beyond_the_limit_is_reported_and_reads_stay_bounded() {
    let limits = Limits {
        max_stream_bytes: 1000,
        max_read_bytes: 300,
        ..Limits::default()
    };
    let fx = fixture_with(limits, Executor::new("sh", "/bin/sh"));
    let code = r#"i=0; while [ "$i" -lt 500 ]; do echo 0123456789; i=$((i + 1)); done
echo "to stderr" >&2"#;
    fx.core.submit(CALLER, request("loud", code, &[])).unwrap();
    let view = finished(&fx.core, "loud");
    assert_eq!(view.stdout.observed_bytes, 5500);
    assert_eq!(view.stdout.retained_bytes, 1000);
    assert!(view.stdout.truncated() && view.stdout.eof);
    assert_eq!(view.stderr.retained_bytes, 10);
    assert!(!view.stderr.truncated());

    let mut retained = Vec::new();
    loop {
        let chunk = fx
            .core
            .read_output(
                CALLER,
                "loud",
                Stream::Stdout,
                retained.len() as u64,
                usize::MAX,
            )
            .unwrap();
        assert!(chunk.bytes.len() <= 300);
        if chunk.bytes.is_empty() {
            break;
        }
        retained.extend_from_slice(&chunk.bytes);
    }
    assert_eq!(retained, "0123456789\n".repeat(500).as_bytes()[..1000]);
    assert_eq!(
        fx.core
            .read_output(CALLER, "loud", Stream::Stdout, 1001, 10)
            .unwrap_err(),
        ReadError::OffsetBeyondRetained {
            offset: 1001,
            retained: 1000
        }
    );
    let stderr = fx
        .core
        .read_output(CALLER, "loud", Stream::Stderr, 0, 100)
        .unwrap();
    assert_eq!(stderr.bytes, b"to stderr\n");
}

#[test]
fn capacity_limits_reject_new_work_without_dispatch_or_eviction() {
    let limits = Limits {
        max_running: 1,
        max_records: 2,
        ..Limits::default()
    };
    let fx = fixture_with(limits, Executor::new("sh", "/bin/sh"));
    let gate = Gate::new(&fx.root, "slot");
    fx.core
        .submit(CALLER, gated("first", &gate, GATED))
        .unwrap();
    gate.wait_started();

    assert_eq!(
        fx.core
            .submit(CALLER, request("second", SUM, &["3"]))
            .unwrap_err(),
        SubmitError::Capacity {
            resource: "running processes",
            limit: 1
        }
    );
    assert!(matches!(
        fx.core.lookup(CALLER, "second"),
        Err(LookupError::NotFound { .. })
    ));
    assert_eq!(dispatches(&fx.work).len(), 1);

    gate.open("free");
    finished(&fx.core, "first");
    // The slot is free once the first run is terminal; nothing was queued.
    fx.core
        .submit(CALLER, request("second", SUM, &["3"]))
        .unwrap();
    finished(&fx.core, "second");
    assert_eq!(dispatches(&fx.work).len(), 2);

    assert_eq!(
        fx.core
            .submit(CALLER, request("third", SUM, &["3"]))
            .unwrap_err(),
        SubmitError::Capacity {
            resource: "accepted request records",
            limit: 2
        }
    );
    // Full capacity never evicts an identity: duplicates still find their original.
    let again = fx
        .core
        .submit(CALLER, gated("first", &gate, GATED))
        .unwrap();
    assert_eq!(again.disposition, Disposition::Existing);
    assert_eq!(dispatches(&fx.work).len(), 2);
}

#[test]
fn cancel_is_a_request_and_stop_is_confirmed_separately() {
    let fx = fixture();
    let gate = Gate::new(&fx.root, "cancel");
    fx.core
        .submit(CALLER, gated("long", &gate, GATED_WITH_CHILD))
        .unwrap();
    gate.wait_started();
    let background: u32 = fs::read_to_string(fx.work.join("background.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(process_alive(background));

    let requested = fx.core.cancel(CALLER, "long").unwrap();
    let cancel = requested.cancel.clone().expect("cancel recorded");
    assert_eq!(cancel.reason, CancelReason::Caller);
    assert!(
        !requested.is_terminal(),
        "a cancel request alone is not a confirmed stop"
    );

    let view = finished(&fx.core, "long");
    assert_eq!(exit_of(&view), Exit::Signal(libc::SIGTERM));
    assert!(view.cancel.unwrap().term_sent_at.is_some());
    assert!(matches!(
        view.status,
        RunStatus::Finished {
            group_released: true,
            ..
        }
    ));
    assert!(
        !process_alive(background),
        "group member {background} survived"
    );

    // Cancelling a finished run changes nothing.
    let after = fx.core.cancel(CALLER, "long").unwrap();
    assert_eq!(after.status, view.status);
    assert_eq!(dispatches(&fx.work).len(), 1);
}

#[test]
fn a_task_ignoring_sigterm_is_killed_after_the_stop_grace() {
    let limits = Limits {
        stop_grace: Duration::from_millis(200),
        ..Limits::default()
    };
    let fx = fixture_with(limits, Executor::new("sh", "/bin/sh"));
    let gate = Gate::new(&fx.root, "stubborn");
    let code = format!("trap '' TERM\n{GATED}");
    fx.core
        .submit(CALLER, gated("stubborn", &gate, &code))
        .unwrap();
    gate.wait_started();
    fx.core.cancel(CALLER, "stubborn").unwrap();

    let view = finished(&fx.core, "stubborn");
    assert_eq!(exit_of(&view), Exit::Signal(libc::SIGKILL));
    let cancel = view.cancel.unwrap();
    let (term, kill) = (cancel.term_sent_at.unwrap(), cancel.kill_sent_at.unwrap());
    assert!(kill.duration_since(term).unwrap() >= Duration::from_millis(200));
}

/// Kills a process that deliberately left the run's process group.
struct Escaped(u32);

impl Drop for Escaped {
    fn drop(&mut self) {
        unsafe { libc::kill(self.0 as i32, libc::SIGKILL) };
    }
}

#[test]
fn output_held_open_outside_the_group_is_reported_not_awaited() {
    let limits = Limits {
        output_close_grace: Duration::from_millis(300),
        ..Limits::default()
    };
    let fx = fixture_with(limits, Executor::new("sh", "/bin/sh"));
    // perl leaves the process group but keeps the inherited stdout open.
    let code = r#"mkfifo escaped.ready
perl -e 'setpgrp(0, 0); open(my $f, ">", "escaped.ready") or die; print $f "$$\n"; close $f; sleep 300' &
read escaped < escaped.ready
echo "$escaped" > escaped.pid
echo "main done""#;
    fx.core
        .submit(CALLER, request("escape", code, &[]))
        .unwrap();
    let view = finished(&fx.core, "escape");
    let escaped = fs::read_to_string(fx.work.join("escaped.pid")).unwrap();
    let escaped = Escaped(escaped.trim().parse().unwrap());
    assert!(
        process_alive(escaped.0),
        "the escaped process is outside Core's hold"
    );

    assert_eq!(exit_of(&view), Exit::Code(0));
    assert!(matches!(
        view.status,
        RunStatus::Finished {
            group_released: true,
            ..
        }
    ));
    assert!(
        !view.stdout.eof,
        "stdout is still held open outside the group"
    );
    assert_eq!(stdout(&fx.core, "escape"), "main done\n");
}

#[test]
fn main_process_exit_releases_the_rest_of_its_process_group() {
    let fx = fixture();
    let code = r#"sleep 300 &
echo "$!" > background.pid
echo "main done""#;
    fx.core
        .submit(CALLER, request("leaves-child", code, &[]))
        .unwrap();
    let view = finished(&fx.core, "leaves-child");
    assert_eq!(exit_of(&view), Exit::Code(0));
    assert!(matches!(
        view.status,
        RunStatus::Finished {
            group_released: true,
            ..
        }
    ));
    assert!(view.stdout.eof, "background member held stdout open");
    assert_eq!(stdout(&fx.core, "leaves-child"), "main done\n");
    let background: u32 = fs::read_to_string(fx.work.join("background.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!process_alive(background));
}

#[test]
fn a_known_start_failure_is_recorded_and_never_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let program = fs::canonicalize(tmp.path()).unwrap().join("not-executable");
    fs::write(&program, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o644)).unwrap();
    let fx = fixture_with(Limits::default(), Executor::new("sh", &program));

    let first = fx
        .core
        .submit(CALLER, request("broken", SUM, &["3"]))
        .unwrap();
    assert_eq!(first.disposition, Disposition::Accepted);
    let view = finished(&fx.core, "broken");
    let RunStatus::NotStarted { reason } = &view.status else {
        panic!("expected NotStarted, got {:?}", view.status);
    };
    assert!(reason.contains("could not be started"), "{reason}");

    let again = fx
        .core
        .submit(CALLER, request("broken", SUM, &["3"]))
        .unwrap();
    assert_eq!(again.disposition, Disposition::Existing);
    assert_eq!(again.run, view);
    assert!(dispatches(&fx.work).is_empty());
}

#[test]
fn run_files_that_cannot_be_prepared_mean_no_dispatch() {
    let fx = fixture();
    let state = fx.core.capability().state_dir;
    fs::set_permissions(&state, fs::Permissions::from_mode(0o500)).unwrap();
    let submitted = fx.core.submit(CALLER, request("unprepared", SUM, &["3"]));
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();

    let view = finished(&fx.core, "unprepared");
    assert_eq!(submitted.unwrap().disposition, Disposition::Accepted);
    let RunStatus::NotStarted { reason } = &view.status else {
        panic!("expected NotStarted, got {:?}", view.status);
    };
    assert!(reason.contains("could not be prepared"), "{reason}");
    assert!(dispatches(&fx.work).is_empty());
}

#[test]
fn shutdown_stops_held_runs_and_drop_removes_instance_files() {
    let fx = fixture();
    let gate = Gate::new(&fx.root, "held");
    fx.core.submit(CALLER, gated("held", &gate, GATED)).unwrap();
    gate.wait_started();
    let pid = pid_of(&fx.core.lookup(CALLER, "held").unwrap());
    let state = fx.core.capability().state_dir;
    assert!(state.is_dir());

    let report = fx.core.shutdown();
    let view = fx.core.lookup(CALLER, "held").unwrap();
    assert_eq!(report.stopped, vec![view.run_id.clone()]);
    assert!(report.not_confirmed.is_empty());
    assert_eq!(view.cancel.as_ref().unwrap().reason, CancelReason::Shutdown);
    assert_eq!(exit_of(&view), Exit::Signal(libc::SIGTERM));
    assert!(!process_alive(pid));

    assert_eq!(
        fx.core
            .submit(CALLER, request("late", SUM, &["1"]))
            .unwrap_err(),
        SubmitError::ShuttingDown
    );
    assert_eq!(
        fx.core
            .submit(CALLER, gated("held", &gate, GATED))
            .unwrap()
            .disposition,
        Disposition::Existing
    );

    drop(fx.core);
    assert!(!state.exists());
    assert_eq!(dispatches(&fx.work).len(), 1);
}

#[test]
fn capability_reports_the_contract_and_effective_limits() {
    let limits = Limits {
        max_running: 2,
        ..Limits::default()
    };
    let fx = fixture_with(
        limits.clone(),
        Executor::new("sh", "/bin/sh").with_args(["-e"]),
    );
    let capability = fx.core.capability();
    assert_eq!(capability.contract, rho_core::CONTRACT);
    assert!(capability.contract.contains("request_id"));
    assert_eq!(capability.limits, limits);
    assert_eq!(capability.executors.len(), 1);
    assert_eq!(capability.executors[0].args, vec!["-e".to_string()]);
    assert_eq!(capability.project_root, fx.work.parent().unwrap());

    let relative =
        CoreConfig::new(&fx.root, fx.root.join("s2")).executor(Executor::new("sh", "sh"));
    assert!(Core::open(relative).is_err());
    let zero = CoreConfig::new(&fx.root, fx.root.join("s3")).limits(Limits {
        max_running: 0,
        ..Limits::default()
    });
    assert!(Core::open(zero).is_err());
}
