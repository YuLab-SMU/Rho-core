//! Fault experiments across Core lifetimes. A host process (this test binary
//! re-run as `host_process`) opens a Core and submits one gated task; it is
//! then killed with SIGKILL, either by a crash point inside Core
//! (`RHO_CORE_CRASH_AT`) or by the test. A new Core opened on the same state
//! dir must find the request and never dispatch it again.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;
use std::time::Duration;

use rho_core::{
    AbsenceEvidence, BootIdScope, Core, CoreConfig, DispatchFact, Disposition, ExecutionFact,
    Executor, FactSource, ForgetError, Knowledge, LookupError, ProcessContext, RetentionState,
    RunRequest, RunStatus, Stream, SubmitError,
};

const CALLER: &str = "agent-a";
const ID: &str = "job";
const PATIENCE: Duration = Duration::from_secs(20);

/// Signals `$1` once running, then blocks until a line arrives on `$2`. It
/// writes nothing to stdout before release, so a dead host's closed pipe does
/// not end it early.
const GATED: &str = r#"echo "$$" >> dispatches.log
echo started > "$1"
read line < "$2"
echo "released:$line" > released.txt
echo "released:$line"
"#;

struct Paths {
    root: PathBuf,
    project: PathBuf,
    work: PathBuf,
    state: PathBuf,
    started: PathBuf,
    release: PathBuf,
}

impl Paths {
    fn new(root: &Path) -> Self {
        let project = root.join("project");
        Self {
            root: root.to_owned(),
            work: project.join("work"),
            project,
            state: root.join("state"),
            started: root.join("gate.started"),
            release: root.join("gate.release"),
        }
    }

    fn config(&self) -> CoreConfig {
        CoreConfig::new(&self.project, &self.state).executor(Executor::new("sh", "/bin/sh"))
    }

    fn request(&self) -> RunRequest {
        RunRequest {
            request_id: ID.into(),
            executor: "sh".into(),
            workdir: "work".into(),
            code: fs::read_to_string(self.root.join("task.code")).unwrap_or_else(|_| GATED.into()),
            args: vec![
                self.started.display().to_string(),
                self.release.display().to_string(),
            ],
        }
    }

    fn dispatches(&self) -> usize {
        fs::read_to_string(self.work.join("dispatches.log")).map_or(0, |t| t.lines().count())
    }

    fn wait_started(&self) {
        let path = self.started.clone();
        let text = within("task start", move || fs::read_to_string(path).unwrap());
        assert_eq!(text, "started\n");
    }

    fn open_gate(&self) {
        let path = self.release.clone();
        within("gate release", move || fs::write(path, "go\n").unwrap());
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

struct Experiment {
    paths: Paths,
    _tmp: tempfile::TempDir,
}

fn experiment() -> Experiment {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let paths = Paths::new(&root);
    fs::create_dir_all(&paths.work).unwrap();
    for fifo in [&paths.started, &paths.release] {
        let c_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    }
    Experiment { paths, _tmp: tmp }
}

/// Starts a host that submits the gated task and then idles until killed.
fn start_host(paths: &Paths, crash_at: Option<&str>) -> Child {
    start_host_with(paths, crash_at, false)
}

fn start_host_with(paths: &Paths, crash_at: Option<&str>, cancel: bool) -> Child {
    host_command(paths, crash_at, cancel).spawn().unwrap()
}

fn host_command(paths: &Paths, crash_at: Option<&str>, cancel: bool) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "host_process", "--nocapture", "--test-threads=1"])
        .env("RHO_TEST_HOST_ROOT", &paths.root)
        .env_remove("RHO_CORE_CRASH_AT")
        .env_remove("RHO_CORE_FAIL_AT")
        .env_remove("RHO_TEST_HOST_SCENARIO")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if let Some(point) = crash_at {
        command.env("RHO_CORE_CRASH_AT", point);
    }
    if cancel {
        command.env("RHO_TEST_HOST_CANCEL", "1");
    } else {
        command.env_remove("RHO_TEST_HOST_CANCEL");
    }
    command
}

/// Waits for the host to die by SIGKILL, as a crash point or the test made it.
fn reap_killed(mut host: Child) {
    use std::os::unix::process::ExitStatusExt;
    let status = within("host exit", move || host.wait().unwrap());
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "host ended with {status:?}"
    );
}

/// Kills the host once its submit call has returned.
fn kill(host: &Child, paths: &Paths) {
    let ready = paths.root.join("host.ready");
    let deadline = std::time::Instant::now() + PATIENCE;
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "host never became ready"
        );
        thread::sleep(Duration::from_millis(10));
    }
    unsafe { libc::kill(host.id() as i32, libc::SIGKILL) };
}

/// Not a check: the entry point of a host process started by the experiments.
#[test]
fn host_process() {
    let Some(root) = std::env::var_os("RHO_TEST_HOST_ROOT") else {
        return;
    };
    let paths = Paths::new(Path::new(&root));
    let scenario = std::env::var("RHO_TEST_HOST_SCENARIO").unwrap_or_default();
    let mut config = paths.config();
    if scenario == "output" {
        config.limits.max_stream_bytes = 10;
    }
    if scenario == "signal" {
        config.limits.stop_grace = Duration::from_millis(50);
    }
    let core = Core::open(config).unwrap();
    core.submit(CALLER, paths.request()).unwrap();
    if scenario == "delete" {
        assert!(core.wait(CALLER, ID, PATIENCE).unwrap().is_terminal());
        core.forget(CALLER, ID).unwrap();
        panic!("delete crash point did not fire");
    }
    if scenario == "signal" {
        paths.wait_started();
        core.cancel(CALLER, ID).unwrap();
        let view = core.wait(CALLER, ID, PATIENCE).unwrap();
        assert!(view.is_terminal());
        fs::write(
            paths.root.join("signal-result.json"),
            serde_json::to_vec(&view.operation).unwrap(),
        )
        .unwrap();
        return;
    }
    if scenario == "output" {
        let deadline = std::time::Instant::now() + PATIENCE;
        while core.lookup(CALLER, ID).unwrap().stdout.observed_bytes < 20 {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }
    if std::env::var_os("RHO_TEST_HOST_CANCEL").is_some() {
        paths.wait_started();
        core.cancel(CALLER, ID).unwrap();
    }
    // submit returned, so the spawn fact (if any) is already saved.
    fs::write(paths.root.join("host.ready"), b"").unwrap();
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

fn reopen(paths: &Paths) -> Core {
    Core::open(paths.config()).unwrap()
}

/// A duplicate after restart finds the original and dispatches nothing,
/// including when several arrive at once.
fn duplicates_find_the_original(core: &Core, paths: &Paths) {
    let before = paths.dispatches();
    let original = core.lookup(CALLER, ID).unwrap();
    let barrier = Arc::new(Barrier::new(4));
    thread::scope(|scope| {
        for _ in 0..4 {
            let barrier = Arc::clone(&barrier);
            let run_id = &original.run_id;
            scope.spawn(move || {
                barrier.wait();
                let again = core.submit(CALLER, paths.request()).unwrap();
                assert_eq!(again.disposition, Disposition::Existing);
                assert_eq!(&again.run.run_id, run_id);
            });
        }
    });
    assert_eq!(paths.dispatches(), before);
}

#[test]
fn crash_before_the_record_is_committed_leaves_no_acceptance() {
    let ex = experiment();
    reap_killed(start_host(&ex.paths, Some("staged")));

    let core = reopen(&ex.paths);
    let found = core.lookup(CALLER, ID);
    assert!(
        matches!(found, Err(LookupError::NotFound { .. })),
        "{found:?}"
    );
    assert_eq!(ex.paths.dispatches(), 0);
    // The identity was never accepted, so it is free and runs exactly once.
    assert_eq!(
        core.submit(CALLER, ex.paths.request()).unwrap().disposition,
        Disposition::Accepted
    );
    ex.paths.wait_started();
    ex.paths.open_gate();
    assert!(core.wait(CALLER, ID, PATIENCE).unwrap().is_terminal());
    assert_eq!(ex.paths.dispatches(), 1);
}

#[test]
fn crash_after_commit_before_dispatch_is_known_not_started() {
    let ex = experiment();
    reap_killed(start_host(&ex.paths, Some("committed")));

    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    let RunStatus::NotStarted { reason } = &view.status else {
        panic!("expected NotStarted, got {:?}", view.status);
    };
    assert!(reason.contains("before dispatching"), "{reason}");
    assert_eq!(view.request.args, ex.paths.request().args);
    duplicates_find_the_original(&core, &ex.paths);
    assert_eq!(ex.paths.dispatches(), 0);
    drop(core);
    // The conclusion was saved; a third instance reads the same facts.
    assert_eq!(reopen(&ex.paths).lookup(CALLER, ID).unwrap(), view);
}

#[test]
fn crash_after_dispatch_before_spawn_is_reported_as_unknown_start() {
    let ex = experiment();
    reap_killed(start_host(&ex.paths, Some("dispatched")));

    let core = reopen(&ex.paths);
    let view = core.wait(CALLER, ID, Duration::from_millis(10)).unwrap();
    assert!(
        !view.is_terminal(),
        "an unknown start is not confirmed completion"
    );
    assert!(
        matches!(
            view.status,
            RunStatus::Detached {
                pid: None,
                present: false,
                ..
            }
        ),
        "{:?}",
        view.status
    );
    duplicates_find_the_original(&core, &ex.paths);
    assert_eq!(ex.paths.dispatches(), 0);
}

#[test]
fn crash_after_spawn_before_it_is_saved_keeps_the_process_detached() {
    let ex = experiment();
    let host = start_host(&ex.paths, Some("spawned"));
    ex.paths.wait_started();
    reap_killed(host);

    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    assert!(
        matches!(
            view.status,
            RunStatus::Detached {
                pid: None,
                present: true,
                ..
            }
        ),
        "{:?}",
        view.status
    );
    assert!(!view.is_terminal(), "the task is still running");
    // No Core holds it: cancel records nothing and shutdown leaves it alone.
    assert!(core.cancel(CALLER, ID).unwrap().cancel.is_none());
    duplicates_find_the_original(&core, &ex.paths);

    ex.paths.open_gate();
    let view = wait_lock_absent(&core);
    assert!(
        !view.is_terminal(),
        "lock release does not prove native exit"
    );
    assert!(
        matches!(view.status, RunStatus::Detached { present: false, .. }),
        "{:?}",
        view.status
    );
    assert_eq!(
        fs::read_to_string(ex.paths.work.join("released.txt")).unwrap(),
        "released:go\n"
    );
    assert_eq!(ex.paths.dispatches(), 1);
}

#[test]
fn host_killed_while_holding_a_run_leaves_it_detached_not_rerun() {
    let ex = experiment();
    let host = start_host(&ex.paths, None);
    ex.paths.wait_started();
    kill(&host, &ex.paths);
    reap_killed(host);

    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    let RunStatus::Detached {
        pid: Some(pid),
        present: true,
        ..
    } = view.status
    else {
        panic!("expected a present detached run, got {:?}", view.status);
    };
    assert!(
        unsafe { libc::kill(pid as i32, 0) } == 0,
        "task {pid} is alive"
    );
    assert!(view.stdout.retention_error.is_some());
    duplicates_find_the_original(&core, &ex.paths);
    let report = core.shutdown();
    assert!(report.stopped.is_empty() && report.not_confirmed.is_empty());
    drop(core);

    ex.paths.open_gate();
    let core = reopen(&ex.paths);
    let view = core.wait(CALLER, ID, PATIENCE).unwrap();
    // Only the absence of its pid is known; no exit status is claimed.
    assert!(
        matches!(
            view.status,
            RunStatus::Gone {
                pid: Some(gone),
                evidence: AbsenceEvidence::MainPidAbsent,
                ..
            } if gone == pid
        ),
        "{:?}",
        view.status
    );
    assert_eq!(ex.paths.dispatches(), 1);
}

/// Waits until no process has `pid`, as `kill -0` from a shell would see it.
fn wait_pid_absent(pid: i32) {
    let deadline = std::time::Instant::now() + PATIENCE;
    while unsafe { libc::kill(pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "process {pid} never ended"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_detached_run_that_ended_can_be_forgotten_to_recover_capacity() {
    let ex = experiment();
    let host = start_host(&ex.paths, None);
    ex.paths.wait_started();
    kill(&host, &ex.paths);
    reap_killed(host);

    let mut config = ex.paths.config();
    config.limits.max_records = 1;
    let core = Core::open(config.clone()).unwrap();
    let pid = saved_pid(&core);
    let mut other = ex.paths.request();
    other.request_id = "other".into();
    other.code = "exit 0\n".into();
    other.args.clear();
    assert!(matches!(
        core.submit(CALLER, other.clone()),
        Err(SubmitError::Capacity { .. })
    ));
    assert!(matches!(
        core.forget(CALLER, ID),
        Err(ForgetError::NotTerminal(_))
    ));

    // The task ends with no Core holding it; a native probe sees it gone.
    ex.paths.open_gate();
    wait_pid_absent(pid as i32);
    let view = core.wait(CALLER, ID, PATIENCE).unwrap();
    assert!(view.is_terminal(), "{:?}", view.status);
    assert!(
        matches!(
            view.status,
            RunStatus::Gone {
                pid: Some(gone),
                evidence: AbsenceEvidence::MainPidAbsent,
                group_released: true,
                ..
            } if gone == pid
        ),
        "{:?}",
        view.status
    );
    assert!(matches!(
        view.operation.execution,
        Knowledge::Known {
            source: FactSource::ProcessTable,
            ..
        }
    ));
    assert_eq!(view.operation.retention, RetentionState::Removable);
    assert_eq!(view.unsaved, None);
    duplicates_find_the_original(&core, &ex.paths);
    drop(core);

    // The observation was saved: the next instance reads the same facts.
    let core = Core::open(config).unwrap();
    assert_eq!(
        core.lookup(CALLER, ID).unwrap().operation.execution,
        view.operation.execution
    );
    core.forget(CALLER, ID).unwrap();
    assert_eq!(ex.paths.dispatches(), 1);
    assert_eq!(
        core.submit(CALLER, other).unwrap().disposition,
        Disposition::Accepted
    );
    assert!(core.wait(CALLER, "other", PATIENCE).unwrap().is_terminal());
}

fn saved_pid(core: &Core) -> u32 {
    let view = core.lookup(CALLER, ID).unwrap();
    let Some(ExecutionFact::SpawnObserved { pid, .. }) = view.operation.execution.value() else {
        panic!("the dead host saved the spawn: {:?}", view.status);
    };
    *pid
}

/// Rewrites the stored dispatch context, standing in for a run dispatched in
/// an earlier boot, another pid namespace or with an unreadable context.
fn edit_dispatch_context(paths: &Paths, edit: impl FnOnce(&mut Option<ProcessContext>)) {
    let db = rusqlite::Connection::open(paths.state.join("core.sqlite3")).unwrap();
    let text: String = db
        .query_row("SELECT dispatch FROM operations", [], |row| row.get(0))
        .unwrap();
    let mut dispatch: Knowledge<DispatchFact> = serde_json::from_str(&text).unwrap();
    let Knowledge::Known {
        value: DispatchFact::Attempted { context },
        ..
    } = &mut dispatch
    else {
        panic!("dispatch was not attempted: {text}");
    };
    edit(context);
    let text = serde_json::to_string(&dispatch).unwrap();
    db.execute("UPDATE operations SET dispatch=?1", [text])
        .unwrap();
}

#[test]
fn a_changed_local_boot_view_does_not_prove_a_live_run_gone() {
    let ex = experiment();
    let host = start_host(&ex.paths, None);
    ex.paths.wait_started();
    kill(&host, &ex.paths);
    reap_killed(host);
    let pid: i32 = fs::read_to_string(ex.paths.work.join("dispatches.log"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // A remounted /proc can expose a different boot-id view in the same
    // process namespace while the original task is still alive.
    edit_dispatch_context(&ex.paths, |context| {
        let context = context.as_mut().unwrap();
        context.boot_id = "another-local-view".into();
        context.boot_id_scope = BootIdScope::LocalView;
    });
    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    let dispatches = ex.paths.dispatches();
    // Release the fixture before checking the snapshot so an expected
    // regression failure does not leave its native task behind.
    drop(core);
    ex.paths.open_gate();
    wait_pid_absent(pid);

    assert!(
        matches!(view.status, RunStatus::Detached { pid: Some(_), .. }),
        "a changed view cannot prove this live process gone: {:?}",
        view.status
    );
    assert_ne!(view.operation.group_released.value(), Some(&true));
    assert_eq!(dispatches, 1);
}

#[test]
fn only_a_changed_kernel_boot_proves_a_run_gone_without_a_pid() {
    let ex = experiment();
    reap_killed(start_host(&ex.paths, Some("dispatched")));
    // Injected store fact: the dispatch is recorded under another boot session.
    // Nothing was spawned, so no process of this run exists in the current one.
    let mut current = String::new();
    let mut scope = None;
    edit_dispatch_context(&ex.paths, |context| {
        let context = context.as_mut().expect("context was readable");
        current = std::mem::replace(&mut context.boot_id, "earlier-boot".into());
        scope = Some(context.boot_id_scope);
        // A genuine kernel reboot proves absence even when namespaces differ.
        context.pid_namespace = Some("pid:[earlier-boot]".into());
    });

    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    if scope == Some(BootIdScope::LocalView) {
        assert!(matches!(view.status, RunStatus::Detached { pid: None, .. }));
        assert_ne!(view.operation.group_released.value(), Some(&true));
        duplicates_find_the_original(&core, &ex.paths);
        assert!(matches!(
            core.forget(CALLER, ID),
            Err(ForgetError::NotTerminal(_))
        ));
        assert_eq!(ex.paths.dispatches(), 0);
        return;
    }
    let RunStatus::Gone {
        pid: None,
        spawned_at: None,
        evidence:
            AbsenceEvidence::BootChanged {
                dispatched,
                current: now,
            },
        group_released: true,
        ..
    } = &view.status
    else {
        panic!("expected a run gone with its boot, got {:?}", view.status);
    };
    assert_eq!((dispatched.as_str(), now), ("earlier-boot", &current));
    assert!(matches!(
        view.operation.group_released,
        Knowledge::Known {
            source: FactSource::BootSession,
            ..
        }
    ));
    duplicates_find_the_original(&core, &ex.paths);
    core.forget(CALLER, ID).unwrap();
    assert_eq!(ex.paths.dispatches(), 0);
}

#[test]
fn pids_prove_nothing_without_the_dispatch_process_context() {
    let ex = experiment();
    let host = start_host(&ex.paths, None);
    ex.paths.wait_started();
    kill(&host, &ex.paths);
    reap_killed(host);
    let mut recorded = None;
    edit_dispatch_context(&ex.paths, |context| recorded = context.take());
    let recorded = recorded.expect("context was readable");
    ex.paths.open_gate();
    let pid: i32 = fs::read_to_string(ex.paths.work.join("dispatches.log"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    wait_pid_absent(pid);

    let undecided = |paths: &Paths| {
        let core = reopen(paths);
        let view = core.lookup(CALLER, ID).unwrap();
        assert!(
            matches!(view.status, RunStatus::Detached { pid: Some(_), .. }),
            "{:?}",
            view.status
        );
        assert_ne!(view.operation.group_released.value(), Some(&true));
        assert!(matches!(
            core.forget(CALLER, ID),
            Err(ForgetError::NotTerminal(_))
        ));
    };
    // An unreadable context at dispatch.
    undecided(&ex.paths);
    // Same boot, another pid namespace: a free pid here says nothing there.
    let mut elsewhere = recorded.clone();
    elsewhere.pid_namespace = Some(match &recorded.pid_namespace {
        Some(namespace) => format!("{namespace}-elsewhere"),
        None => "pid:[elsewhere]".into(),
    });
    edit_dispatch_context(&ex.paths, |context| *context = Some(elsewhere));
    undecided(&ex.paths);

    // Same boot text and namespace from a different source prove nothing.
    let mut other_scope = recorded.clone();
    other_scope.boot_id_scope = match recorded.boot_id_scope {
        BootIdScope::Kernel => BootIdScope::LocalView,
        BootIdScope::LocalView => BootIdScope::Kernel,
    };
    edit_dispatch_context(&ex.paths, |context| *context = Some(other_scope));
    undecided(&ex.paths);

    edit_dispatch_context(&ex.paths, |context| *context = Some(recorded));
    let core = reopen(&ex.paths);
    assert!(matches!(
        core.lookup(CALLER, ID).unwrap().status,
        RunStatus::Gone {
            evidence: AbsenceEvidence::MainPidAbsent,
            group_released: true,
            ..
        }
    ));
    core.forget(CALLER, ID).unwrap();
    assert_eq!(ex.paths.dispatches(), 1);
}

#[test]
fn a_group_member_that_outlives_the_main_process_keeps_the_record() {
    let ex = experiment();
    let member_gate = ex.paths.work.join("member.gate");
    let c_path = CString::new(member_gate.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
    // The member does not hold the run lock or the output pipes.
    fs::write(
        ex.paths.root.join("task.code"),
        "echo \"$$\" >> dispatches.log\n\
         (read line < member.gate; echo member > member.txt) </dev/null >/dev/null 2>&1 &\n\
         echo started > \"$1\"\n\
         read line < \"$2\"\n",
    )
    .unwrap();
    let host = start_host(&ex.paths, None);
    ex.paths.wait_started();
    kill(&host, &ex.paths);
    reap_killed(host);
    let core = reopen(&ex.paths);
    let pid = saved_pid(&core);

    ex.paths.open_gate();
    wait_pid_absent(pid as i32);
    let view = core.wait(CALLER, ID, PATIENCE).unwrap();
    assert!(
        matches!(
            view.status,
            RunStatus::Gone {
                evidence: AbsenceEvidence::MainPidAbsent,
                group_released: false,
                ..
            }
        ),
        "{:?}",
        view.status
    );
    assert!(matches!(
        view.operation.retention,
        RetentionState::Protected { .. }
    ));
    assert!(matches!(
        core.forget(CALLER, ID),
        Err(ForgetError::NotTerminal(_))
    ));
    duplicates_find_the_original(&core, &ex.paths);

    within("member release", move || {
        fs::write(member_gate, "go\n").unwrap()
    });
    let deadline = std::time::Instant::now() + PATIENCE;
    while core
        .lookup(CALLER, ID)
        .unwrap()
        .operation
        .group_released
        .value()
        != Some(&true)
    {
        assert!(std::time::Instant::now() < deadline, "group never released");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        fs::read_to_string(ex.paths.work.join("member.txt")).unwrap(),
        "member\n"
    );
    core.forget(CALLER, ID).unwrap();
    assert_eq!(ex.paths.dispatches(), 1);
}

#[test]
fn closing_inherited_stdin_does_not_confirm_exit_after_host_crash() {
    let ex = experiment();
    fs::write(
        ex.paths.root.join("task.code"),
        format!("exec 0</dev/null\n{GATED}"),
    )
    .unwrap();
    let host = start_host(&ex.paths, None);
    ex.paths.wait_started();
    kill(&host, &ex.paths);
    reap_killed(host);
    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    let forget = core.forget(CALLER, ID);
    let pid: i32 = fs::read_to_string(ex.paths.work.join("dispatches.log"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    // Release the fixture before asserting, including on the former implementation.
    ex.paths.open_gate();
    assert!(alive, "the native task is still alive");
    assert_eq!(view.operation.run_lock.value(), Some(&false));
    assert!(
        !view.is_terminal(),
        "closing stdin is not evidence of native exit: {:?}",
        view.status
    );
    assert!(matches!(forget, Err(ForgetError::NotTerminal(_))));
    // The end of the process, not of its lock, is what is observed.
    wait_pid_absent(pid);
    let view = core.wait(CALLER, ID, PATIENCE).unwrap();
    assert!(
        matches!(
            view.status,
            RunStatus::Gone {
                evidence: AbsenceEvidence::MainPidAbsent,
                ..
            }
        ),
        "{:?}",
        view.status
    );
}

#[test]
fn crash_after_exit_before_the_outcome_is_saved_keeps_retained_output() {
    let ex = experiment();
    let host = start_host(&ex.paths, Some("exited"));
    ex.paths.wait_started();
    ex.paths.open_gate();
    reap_killed(host);

    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    // The exit status was observed only by the dead host, so it is not
    // claimed; the reaped pid is observed absent.
    assert!(
        matches!(
            view.status,
            RunStatus::Gone {
                pid: Some(_),
                evidence: AbsenceEvidence::MainPidAbsent,
                ..
            }
        ),
        "{:?}",
        view.status
    );
    let chunk = core
        .read_output(CALLER, ID, Stream::Stdout, 0, 1024)
        .unwrap();
    assert_eq!(chunk.bytes, b"released:go\n");
    duplicates_find_the_original(&core, &ex.paths);
    assert_eq!(ex.paths.dispatches(), 1);
}

#[test]
fn a_finished_run_is_found_with_its_outcome_after_restart() {
    let ex = experiment();
    let core = reopen(&ex.paths);
    core.submit(CALLER, ex.paths.request()).unwrap();
    ex.paths.wait_started();
    ex.paths.open_gate();
    let finished = core.wait(CALLER, ID, PATIENCE).unwrap();
    assert!(matches!(finished.status, RunStatus::Finished { .. }));
    drop(core);

    let core = reopen(&ex.paths);
    assert_eq!(core.lookup(CALLER, ID).unwrap(), finished);
    let chunk = core
        .read_output(CALLER, ID, Stream::Stdout, 0, 1024)
        .unwrap();
    assert_eq!(chunk.bytes, b"released:go\n");
    duplicates_find_the_original(&core, &ex.paths);
    assert_eq!(ex.paths.dispatches(), 1);
}

#[test]
fn an_unreadable_record_blocks_its_identity_instead_of_rerunning() {
    let ex = experiment();
    reap_killed(start_host(&ex.paths, Some("committed")));
    let record = ex.paths.state.join("core.sqlite3");
    let db = rusqlite::Connection::open(&record).unwrap();
    db.execute("UPDATE operations SET accepted='{ not json'", [])
        .unwrap();
    drop(db);

    let core = reopen(&ex.paths);
    assert_eq!(core.unreadable_records().len(), 1);
    assert_eq!(core.unreadable_records()[0].0, record);
    assert!(
        !matches!(core.lookup(CALLER, ID), Err(LookupError::NotFound { .. })),
        "a corrupt accepted record must be reported as unreadable, never absent"
    );
    let Err(SubmitError::RecordUnreadable { reason }) = core.submit(CALLER, ex.paths.request())
    else {
        panic!("an unreadable record must not authorize a new run");
    };
    assert!(reason.contains("accepted"), "{reason}");
    assert_eq!(ex.paths.dispatches(), 0);
}

#[test]
fn the_state_dir_is_exclusive_and_bound_to_one_project() {
    let ex = experiment();
    let core = reopen(&ex.paths);
    assert!(
        Core::open(ex.paths.config()).is_err(),
        "second Core on one state dir"
    );
    drop(core);

    let other = ex.paths.root.join("other");
    fs::create_dir_all(&other).unwrap();
    let config = CoreConfig::new(&other, &ex.paths.state).executor(Executor::new("sh", "/bin/sh"));
    let error = Core::open(config).err().expect("project mismatch rejected");
    assert!(error.to_string().contains("belongs to project"), "{error}");
}

#[test]
fn forget_frees_only_terminal_records_and_then_the_identity_runs_again() {
    let ex = experiment();
    let mut config = ex.paths.config();
    config.limits.max_records = 1;
    let limited = Core::open(config.clone()).unwrap();
    limited.submit(CALLER, ex.paths.request()).unwrap();
    ex.paths.wait_started();
    let mut other = ex.paths.request();
    other.request_id = "other".into();
    assert_eq!(
        limited.submit(CALLER, other).unwrap_err(),
        SubmitError::Capacity {
            resource: "accepted request records",
            limit: 1
        }
    );
    assert!(matches!(
        limited.forget(CALLER, ID),
        Err(ForgetError::NotTerminal(_))
    ));

    ex.paths.open_gate();
    assert!(limited.wait(CALLER, ID, PATIENCE).unwrap().is_terminal());
    drop(limited);
    // Durable completed records still consume capacity in the next instance.
    let limited = Core::open(config).unwrap();
    let mut other = ex.paths.request();
    other.request_id = "other".into();
    assert!(matches!(
        limited.submit(CALLER, other),
        Err(SubmitError::Capacity { .. })
    ));
    limited.forget(CALLER, ID).unwrap();
    assert!(matches!(
        limited.lookup(CALLER, ID),
        Err(LookupError::NotFound { .. })
    ));
    drop(limited);
    let core = reopen(&ex.paths);
    assert!(matches!(
        core.lookup(CALLER, ID),
        Err(LookupError::NotFound { .. })
    ));
    assert_eq!(
        core.submit(CALLER, ex.paths.request()).unwrap().disposition,
        Disposition::Accepted
    );
    ex.paths.wait_started();
    ex.paths.open_gate();
    assert!(core.wait(CALLER, ID, PATIENCE).unwrap().is_terminal());
    assert_eq!(ex.paths.dispatches(), 2);
}

#[test]
fn a_saved_cancel_survives_a_crash_but_is_not_reported_as_a_stop() {
    let ex = experiment();
    // The host reads the started FIFO itself before cancelling.
    reap_killed(start_host_with(&ex.paths, Some("cancel-saved"), true));

    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    let cancel = view.cancel.as_ref().expect("the cancel request was saved");
    assert!(
        cancel.term_sent_at.is_none(),
        "no signal was sent before the crash"
    );
    // The task may still be running; nothing claims it stopped.
    assert!(
        matches!(view.status, RunStatus::Detached { present: true, .. }),
        "{:?}",
        view.status
    );
    ex.paths.open_gate();
    let view = core.wait(CALLER, ID, PATIENCE).unwrap();
    // Its end is observed, but not as the effect of a stop signal.
    assert!(
        matches!(view.status, RunStatus::Gone { .. }),
        "{:?}",
        view.status
    );
    let cancel = view.cancel.as_ref().unwrap();
    assert!(cancel.term_sent_at.is_none() && cancel.kill_sent_at.is_none());
    assert_eq!(ex.paths.dispatches(), 1);
}

fn wait_lock_absent(core: &Core) -> rho_core::RunView {
    let deadline = std::time::Instant::now() + PATIENCE;
    loop {
        let view = core.lookup(CALLER, ID).unwrap();
        if view.operation.run_lock.value() == Some(&false) {
            return view;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture never released its lock: {view:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn output_observations_and_truncation_survive_a_crash_before_exit() {
    let ex = experiment();
    fs::write(
        ex.paths.root.join("task.code"),
        format!("printf '12345678901234567890'\n{GATED}"),
    )
    .unwrap();
    let host = host_command(&ex.paths, None, false)
        .env("RHO_TEST_HOST_SCENARIO", "output")
        .spawn()
        .unwrap();
    ex.paths.wait_started();
    kill(&host, &ex.paths);
    reap_killed(host);
    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    assert_eq!(view.stdout.observed_bytes, 20);
    assert_eq!(view.stdout.retained_bytes, 10);
    assert!(view.stdout.truncated());
    assert!(!view.stdout.eof);
    assert!(!view.is_terminal());
    assert_eq!(
        core.read_output(CALLER, ID, Stream::Stdout, 0, 100)
            .unwrap()
            .bytes,
        b"1234567890"
    );
    duplicates_find_the_original(&core, &ex.paths);
    assert!(matches!(
        core.forget(CALLER, ID),
        Err(ForgetError::NotTerminal(_))
    ));
    ex.paths.open_gate();
    let view = core.wait(CALLER, ID, PATIENCE).unwrap();
    assert!(
        matches!(view.status, RunStatus::Gone { .. }),
        "{:?}",
        view.status
    );
    // Later output was not retained; the end does not claim it was.
    assert_eq!(view.stdout.retained_bytes, 10);
    assert!(view.stdout.retention_error.is_some() && !view.stdout.eof);
}

#[test]
fn an_interrupted_output_transaction_never_publishes_uncommitted_blob_bytes() {
    let ex = experiment();
    fs::write(
        ex.paths.root.join("task.code"),
        format!("printf 'partial'\n{GATED}"),
    )
    .unwrap();
    let host = start_host(&ex.paths, Some("output-staged"));
    // The native fixture is gated regardless of whether its pipe reader died.
    ex.paths.wait_started();
    reap_killed(host);
    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    assert_eq!(view.stdout.retained_bytes, 0);
    assert!(
        core.read_output(CALLER, ID, Stream::Stdout, 0, 100)
            .unwrap()
            .bytes
            .is_empty()
    );
    duplicates_find_the_original(&core, &ex.paths);
    ex.paths.open_gate();
    let view = core.wait(CALLER, ID, PATIENCE).unwrap();
    assert!(
        matches!(view.status, RunStatus::Gone { .. }),
        "{:?}",
        view.status
    );
    assert_eq!(view.stdout.retained_bytes, 0);
}

#[test]
fn saved_exit_is_not_downgraded_when_release_was_not_saved() {
    let ex = experiment();
    let host = start_host(&ex.paths, Some("exit-saved"));
    ex.paths.wait_started();
    ex.paths.open_gate();
    reap_killed(host);
    let core = reopen(&ex.paths);
    let view = core.lookup(CALLER, ID).unwrap();
    assert!(matches!(
        view.operation.execution.value(),
        Some(rho_core::ExecutionFact::ExitObserved {
            exit: rho_core::Exit::Code(0),
            ..
        })
    ));
    // The unsaved release is observed again from the empty process group.
    assert!(matches!(
        view.operation.group_released,
        Knowledge::Known {
            value: true,
            source: FactSource::ProcessTable,
            ..
        }
    ));
    let config = ex.paths.config();
    drop(core);
    let core = Core::open(config).unwrap();
    assert_eq!(
        core.lookup(CALLER, ID).unwrap().operation.execution,
        view.operation.execution
    );
    duplicates_find_the_original(&core, &ex.paths);
    core.forget(CALLER, ID).unwrap();
}

#[test]
fn a_failed_term_is_not_reported_as_sent_or_as_a_confirmed_stop() {
    let ex = experiment();
    let mut host = host_command(&ex.paths, None, false)
        .env("RHO_TEST_HOST_SCENARIO", "signal")
        .env("RHO_CORE_FAIL_AT", "signal-term")
        .spawn()
        .unwrap();
    let status = within("signal experiment", move || host.wait().unwrap());
    assert!(status.success(), "{status}");
    let record: rho_core::OperationRecord =
        serde_json::from_slice(&fs::read(ex.paths.root.join("signal-result.json")).unwrap())
            .unwrap();
    let cancel = record.cancellation.unwrap();
    assert!(cancel.term_sent_at.is_none());
    assert!(cancel.signal_error.as_deref().unwrap().contains("failed"));
    assert!(cancel.kill_sent_at.is_some());
    assert!(matches!(
        record.execution.value(),
        Some(rho_core::ExecutionFact::ExitObserved {
            exit: rho_core::Exit::Signal(libc::SIGKILL),
            ..
        })
    ));
    assert_eq!(ex.paths.dispatches(), 1);
}

fn delete_crash(point: &str) -> Experiment {
    let ex = experiment();
    fs::write(
        ex.paths.root.join("task.code"),
        "echo $$ >> dispatches.log\nprintf 'finished'\n",
    )
    .unwrap();
    let host = host_command(&ex.paths, Some(point), false)
        .env("RHO_TEST_HOST_SCENARIO", "delete")
        .spawn()
        .unwrap();
    reap_killed(host);
    ex
}

#[test]
fn a_crash_inside_deletion_keeps_the_acceptance_and_its_blobs() {
    let ex = delete_crash("delete-staged");
    let core = reopen(&ex.paths);
    assert!(core.lookup(CALLER, ID).unwrap().is_terminal());
    assert_eq!(
        core.read_output(CALLER, ID, Stream::Stdout, 0, 100)
            .unwrap()
            .bytes,
        b"finished"
    );
    duplicates_find_the_original(&core, &ex.paths);
    assert_eq!(ex.paths.dispatches(), 1);
}

#[test]
fn a_crash_after_committed_deletion_releases_the_key_explicitly() {
    let ex = delete_crash("deleted");
    let core = reopen(&ex.paths);
    assert!(matches!(
        core.lookup(CALLER, ID),
        Err(LookupError::NotFound { .. })
    ));
    assert_eq!(
        core.submit(CALLER, ex.paths.request()).unwrap().disposition,
        Disposition::Accepted
    );
    assert!(core.wait(CALLER, ID, PATIENCE).unwrap().is_terminal());
    assert_eq!(ex.paths.dispatches(), 2);
}
