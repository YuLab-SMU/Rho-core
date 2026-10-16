//! The `rho-core` host and CLI used as an Agent uses them: every call is a
//! separate client process, and the host is a real process that a test can
//! terminate or kill. Runs must outlive the clients, duplicate deliveries must
//! not dispatch again, and a request that was not delivered must start nothing.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

#[path = "support/dispatch.rs"]
mod dispatch;

const BIN: &str = env!("CARGO_BIN_EXE_rho-core");
const CALLER: &str = "agent";
const PATIENCE: Duration = Duration::from_secs(20);

/// Records its dispatch, then runs until `release` exists. It writes to stdout
/// only after release, so a killed host's closed pipe does not end it early.
const HOLD: &str = "printf 'hold\\n' >> dispatches.txt
while [ ! -f release ]; do sleep 0.05; done
echo released";

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("project/analysis")).unwrap();
        Self { dir }
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    fn work(&self, name: &str) -> PathBuf {
        self.dir.path().join("project/analysis").join(name)
    }

    fn dispatches(&self) -> usize {
        dispatch::read(&self.work("dispatches.txt")).map_or(0, |lines| lines.len())
    }

    fn release(&self) {
        fs::write(self.work("release"), "").unwrap();
    }

    fn serve_command(&self, extra: &[&str]) -> Command {
        let mut command = Command::new(BIN);
        command
            .arg("serve")
            .arg("--project")
            .arg(self.dir.path().join("project"))
            .arg("--state-dir")
            .arg(self.state())
            .args(["--executor", "sh=/bin/sh"])
            .args(extra);
        command
    }

    fn serve(&self, extra: &[&str]) -> Host {
        let mut child = self
            .serve_command(extra)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let ready = next_line(&mut stdout);
        let socket = ready["ready"]["socket"]
            .as_str()
            .unwrap_or_else(|| panic!("no readiness line: {ready}"))
            .into();
        Host {
            child,
            stdout,
            socket,
        }
    }
}

fn next_line(stdout: &mut BufReader<ChildStdout>) -> Value {
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap_or(Value::Null)
}

struct Host {
    child: Child,
    stdout: BufReader<ChildStdout>,
    socket: PathBuf,
}

impl Host {
    fn cli(&self, args: &[&str]) -> (i32, Value) {
        cli(&self.socket, CALLER, args, None)
    }

    fn signal(&self, signal: i32) {
        // SAFETY: kill has no memory-safety preconditions.
        assert_eq!(unsafe { libc::kill(self.child.id() as i32, signal) }, 0);
    }

    fn kill(&mut self) {
        self.signal(libc::SIGKILL);
        self.child.wait().unwrap();
    }

    /// Sends SIGTERM and returns the exit success and the shutdown report.
    fn terminate(&mut self) -> (bool, Value) {
        self.signal(libc::SIGTERM);
        let report = next_line(&mut self.stdout);
        (self.child.wait().unwrap().success(), report)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            self.signal(libc::SIGTERM);
            let deadline = Instant::now() + PATIENCE;
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn client(socket: &Path, caller: &str, args: &[&str]) -> Command {
    let mut command = Command::new(BIN);
    command
        .args(args)
        .env("RHO_CORE_SOCKET", socket)
        .env("RHO_CORE_CALLER", caller)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn cli(socket: &Path, caller: &str, args: &[&str], stdin: Option<&str>) -> (i32, Value) {
    let mut command = client(socket, caller, args);
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().unwrap();
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    reply(child.wait_with_output().unwrap())
}

fn reply(output: Output) -> (i32, Value) {
    let code = output.status.code().expect("client ended by a signal");
    let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        Value::String(format!(
            "stdout: {} stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    });
    (code, value)
}

fn submit_args<'a>(id: &'a str, code: &'a str) -> [&'a str; 9] {
    [
        "submit",
        "--id",
        id,
        "--executor",
        "sh",
        "--workdir",
        "analysis",
        "--code",
        code,
    ]
}

fn submit(host: &Host, id: &str, code: &str) -> (i32, Value) {
    host.cli(&submit_args(id, code))
}

fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn kind(reply: &Value) -> &str {
    reply["error"]["kind"].as_str().unwrap_or("")
}

#[test]
fn an_agent_drives_one_run_through_separate_cli_calls() {
    let fixture = Fixture::new();
    let host = fixture.serve(&[]);
    let script = "printf '%s\\n' \"$@\" > args.txt
printf 'run\\n' >> dispatches.txt
echo out; echo err >&2
while [ ! -f release ]; do sleep 0.05; done
echo done";
    let args = [
        "submit",
        "--id",
        "job-1",
        "--executor",
        "sh",
        "--workdir",
        "analysis",
        "--code-file",
        "-",
        "--",
        "alpha",
        "beta gamma",
    ];
    let (code, accepted) = cli(&host.socket, CALLER, &args, Some(script));
    assert_eq!(code, 0, "{accepted}");
    assert_eq!(accepted["value"]["disposition"], "accepted");
    let run_id = accepted["value"]["run"]["run_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // That client has exited; the host still holds the run for later calls.
    let (code, view) = host.cli(&["lookup", "--id", "job-1"]);
    assert_eq!(code, 0, "{view}");
    assert_eq!(view["value"]["run_id"], run_id.as_str());
    assert_eq!(view["value"]["held"], true);
    assert!(view["value"]["status"]["Running"].is_object(), "{view}");
    let (_, listed) = host.cli(&["list"]);
    assert_eq!(listed["value"]["runs"][0]["request_id"], "job-1");
    // Another caller's scope does not contain it.
    let (code, other) = cli(
        &host.socket,
        "someone-else",
        &["lookup", "--id", "job-1"],
        None,
    );
    assert_eq!((code, kind(&other)), (1, "not_found"), "{other}");

    fixture.release();
    let (code, done) = host.cli(&["wait", "--id", "job-1", "--timeout", "20"]);
    assert_eq!(code, 0, "{done}");
    assert_eq!(done["value"]["terminal"], true, "{done}");
    assert_eq!(done["value"]["status"]["Finished"]["exit"]["Code"], 0);
    let (_, stdout) = host.cli(&["read", "--id", "job-1"]);
    assert_eq!(stdout["value"]["text"], "out\ndone\n", "{stdout}");
    assert_eq!(stdout["value"]["more_retained"], false);
    let (_, stderr) = host.cli(&["read", "--id", "job-1", "--stream", "stderr"]);
    assert_eq!(stderr["value"]["text"], "err\n");
    assert_eq!(
        fs::read_to_string(fixture.work("args.txt")).unwrap(),
        "alpha\nbeta gamma\n"
    );

    // Delivering the same submission again returns the original run.
    let (code, again) = cli(&host.socket, CALLER, &args, Some(script));
    assert_eq!(code, 0, "{again}");
    assert_eq!(again["value"]["disposition"], "existing");
    assert_eq!(again["value"]["run"]["run_id"], run_id.as_str());
    assert_eq!(fixture.dispatches(), 1);

    let (code, forgotten) = host.cli(&["forget", "--id", "job-1"]);
    assert_eq!(code, 0, "{forgotten}");
    let (code, gone) = host.cli(&["lookup", "--id", "job-1"]);
    assert_eq!((code, kind(&gone)), (1, "not_found"));
    let (code, _) = host.cli(&["submit", "--executor", "sh", "--code", "true"]);
    assert_eq!(code, 2, "a usage error sends nothing");
}

#[test]
fn without_a_host_nothing_is_sent_and_nothing_runs() {
    let fixture = Fixture::new();
    let state = fixture.state();
    fs::create_dir_all(&state).unwrap();
    let socket = state.join("host.sock");
    let args = submit_args("job", "printf 'x\\n' >> dispatches.txt");
    let (code, reply) = cli(&socket, CALLER, &args, None);
    assert_eq!((code, kind(&reply)), (3, "host_unavailable"), "{reply}");

    // A socket left behind by a host that is gone refuses connections.
    drop(UnixListener::bind(&socket).unwrap());
    let (code, reply) = cli(&socket, CALLER, &args, None);
    assert_eq!((code, kind(&reply)), (3, "host_unavailable"), "{reply}");
    assert_eq!(fixture.dispatches(), 0);

    // A new host replaces that socket and has no record of the request.
    let host = fixture.serve(&[]);
    let (code, reply) = host.cli(&["lookup", "--id", "job"]);
    assert_eq!((code, kind(&reply)), (1, "not_found"), "{reply}");
    assert_eq!(fixture.dispatches(), 0);
}

#[test]
fn concurrent_duplicate_submissions_from_separate_processes_dispatch_once() {
    let fixture = Fixture::new();
    let host = fixture.serve(&[]);
    let code = "printf 'x\\n' >> dispatches.txt; sleep 0.2";
    let clients: Vec<Child> = (0..8)
        .map(|_| {
            client(&host.socket, CALLER, &submit_args("same", code))
                .spawn()
                .unwrap()
        })
        .collect();
    let replies: Vec<Value> = clients
        .into_iter()
        .map(|child| {
            let (code, reply) = reply(child.wait_with_output().unwrap());
            assert_eq!(code, 0, "{reply}");
            reply
        })
        .collect();
    let accepted = replies
        .iter()
        .filter(|r| r["value"]["disposition"] == "accepted")
        .count();
    assert_eq!(accepted, 1, "{replies:?}");
    let run_id = &replies[0]["value"]["run"]["run_id"];
    assert!(
        replies
            .iter()
            .all(|r| &r["value"]["run"]["run_id"] == run_id)
    );

    let (_, done) = host.cli(&["wait", "--id", "same", "--timeout", "20"]);
    assert_eq!(done["value"]["terminal"], true, "{done}");
    assert_eq!(fixture.dispatches(), 1);

    // The same id with different code is a conflict and starts nothing.
    let (code, conflict) = submit(&host, "same", "printf 'y\\n' >> dispatches.txt");
    assert_eq!((code, kind(&conflict)), (1, "conflict"), "{conflict}");
    assert_eq!(&conflict["error"]["existing"]["run_id"], run_id);
    assert_eq!(fixture.dispatches(), 1);
}

#[test]
fn cancel_through_the_host_ends_the_run_with_sigterm() {
    let fixture = Fixture::new();
    // A long grace keeps SIGKILL out of the picture: a run that cannot
    // receive SIGTERM (e.g. an inherited blocked mask) would end by SIGKILL.
    let host = fixture.serve(&["--limit", "stop_grace_ms=10000"]);
    let (code, accepted) = submit(&host, "job", HOLD);
    assert_eq!(code, 0, "{accepted}");
    eventually("dispatch", || fixture.dispatches() == 1);

    let (code, cancelled) = host.cli(&["cancel", "--id", "job"]);
    assert_eq!(code, 0, "{cancelled}");
    assert_eq!(cancelled["value"]["cancel"]["reason"], "Caller");
    let (_, done) = host.cli(&["wait", "--id", "job", "--timeout", "20"]);
    let finished = &done["value"]["status"]["Finished"];
    assert_eq!(finished["exit"]["Signal"], libc::SIGTERM, "{done}");
    assert_eq!(finished["group_released"], true);
    assert!(done["value"]["cancel"]["kill_sent_at"].is_null(), "{done}");
}

#[test]
fn a_killed_host_restarts_and_finds_its_runs_without_dispatching_again() {
    let fixture = Fixture::new();
    let mut host = fixture.serve(&[]);
    let (code, accepted) = submit(&host, "job", HOLD);
    assert_eq!(code, 0, "{accepted}");
    let run_id = accepted["value"]["run"]["run_id"].clone();
    eventually("dispatch", || fixture.dispatches() == 1);

    host.kill();
    assert!(host.socket.exists(), "a killed host leaves its socket");
    let (code, reply) = host.cli(&["lookup", "--id", "job"]);
    assert_eq!((code, kind(&reply)), (3, "host_unavailable"), "{reply}");

    let host = fixture.serve(&[]);
    let (code, view) = host.cli(&["lookup", "--id", "job"]);
    assert_eq!(code, 0, "{view}");
    assert_eq!(view["value"]["run_id"], run_id);
    assert_eq!(view["value"]["held"], false);
    assert_eq!(
        view["value"]["status"]["Detached"]["present"], true,
        "{view}"
    );
    let (code, again) = submit(&host, "job", HOLD);
    assert_eq!(code, 0, "{again}");
    assert_eq!(again["value"]["disposition"], "existing");
    assert_eq!(again["value"]["run"]["run_id"], run_id);
    let (code, protected) = host.cli(&["forget", "--id", "job"]);
    assert_eq!((code, kind(&protected)), (1, "protected"), "{protected}");

    fixture.release();
    let (_, done) = host.cli(&["wait", "--id", "job", "--timeout", "20"]);
    let gone = &done["value"]["status"]["Gone"];
    assert_eq!(gone["evidence"], "MainPidAbsent", "{done}");
    assert_eq!(gone["group_released"], true);
    let (code, forgotten) = host.cli(&["forget", "--id", "job"]);
    assert_eq!(code, 0, "{forgotten}");
    assert_eq!(fixture.dispatches(), 1);
}

#[test]
fn a_terminated_host_stops_its_runs_and_removes_its_socket() {
    let fixture = Fixture::new();
    let mut host = fixture.serve(&[]);
    let (_, accepted) = submit(&host, "job", HOLD);
    let run_id = accepted["value"]["run"]["run_id"].clone();
    eventually("dispatch", || fixture.dispatches() == 1);

    let (success, report) = host.terminate();
    assert!(success, "{report}");
    assert_eq!(report["shutdown"]["stopped"][0], run_id, "{report}");
    assert!(!host.socket.exists());
    let (code, reply) = host.cli(&["list"]);
    assert_eq!((code, kind(&reply)), (3, "host_unavailable"), "{reply}");

    let host = fixture.serve(&[]);
    let (_, view) = host.cli(&["lookup", "--id", "job"]);
    assert_eq!(view["value"]["terminal"], true, "{view}");
    assert_eq!(
        view["value"]["status"]["Finished"]["exit"]["Signal"],
        libc::SIGTERM
    );
    assert_eq!(view["value"]["cancel"]["reason"], "Shutdown");
    assert_eq!(fixture.dispatches(), 1);
}

#[test]
fn a_second_host_cannot_take_over_a_state_dir_in_use() {
    let fixture = Fixture::new();
    let host = fixture.serve(&[]);
    let second = fixture.serve_command(&[]).output().unwrap();
    assert!(!second.status.success());
    assert!(
        second.stdout.is_empty(),
        "the second host never became ready"
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("in use by another Core"), "{stderr}");

    assert!(host.socket.exists(), "the first host's socket is untouched");
    let (code, reply) = host.cli(&["capability"]);
    assert_eq!(code, 0, "{reply}");
}

#[test]
fn oversized_requests_are_refused_before_anything_starts() {
    let fixture = Fixture::new();
    let host = fixture.serve(&[
        "--limit",
        "max_code_bytes=1024",
        "--limit",
        "max_arg_bytes=1024",
        "--limit=max_args=8",
    ]);
    let marker = "printf 'x\\n' >> dispatches.txt\n";

    // Within the host's request bound, Core refuses the code size.
    let (code, reply) = submit(&host, "big", &format!("{marker}{}", "#".repeat(2000)));
    assert_eq!((code, kind(&reply)), (1, "invalid_request"), "{reply}");
    let (code, reply) = host.cli(&["lookup", "--id", "big"]);
    assert_eq!((code, kind(&reply)), (1, "not_found"));

    // Beyond it the client sends nothing.
    let (_, capability) = host.cli(&["capability"]);
    let max = capability["value"]["host"]["max_request_bytes"]
        .as_u64()
        .unwrap() as usize;
    let file = fixture.dir.path().join("huge.sh");
    fs::write(&file, format!("{marker}{}", "#".repeat(max))).unwrap();
    let (code, reply) = host.cli(&[
        "submit",
        "--id",
        "huge",
        "--executor",
        "sh",
        "--workdir",
        "analysis",
        "--code-file",
        file.to_str().unwrap(),
    ]);
    assert_eq!((code, kind(&reply)), (1, "request_too_large"), "{reply}");

    // A client that ignores the bound gets an answer and the host goes on.
    let stream = UnixStream::connect(&host.socket).unwrap();
    let mut reader = BufReader::new(&stream);
    let mut greeting = String::new();
    reader.read_line(&mut greeting).unwrap();
    assert!(greeting.contains("\"ready\""), "{greeting}");
    (&stream).write_all(&vec![b'x'; max]).unwrap();
    let mut answer = String::new();
    reader.read_line(&mut answer).unwrap();
    let answer: Value = serde_json::from_str(&answer).unwrap();
    assert_eq!(kind(&answer), "request_too_large", "{answer}");

    let (code, reply) = host.cli(&["list"]);
    assert_eq!(code, 0, "{reply}");
    assert_eq!(reply["value"]["runs"], Value::Array(Vec::new()));
    assert_eq!(fixture.dispatches(), 0);
}

#[test]
fn a_full_host_answers_busy_and_a_lost_reply_is_reported_unknown() {
    let fixture = Fixture::new();
    let mut host = fixture.serve(&["--max-connections", "1"]);
    let (code, accepted) = submit(&host, "job", HOLD);
    assert_eq!(code, 0, "{accepted}");
    let pid = accepted["value"]["run"]["status"]["Running"]["pid"]
        .as_i64()
        .unwrap_or_else(|| panic!("not running: {accepted}")) as i32;
    let waiter = || {
        client(
            &host.socket,
            CALLER,
            &["wait", "--id", "job", "--timeout", "60"],
        )
        .spawn()
        .unwrap()
    };

    // The waiter occupies the only connection; a probe that collides with it
    // instead turns the waiter away, so start the waiter again.
    let mut waiting = waiter();
    eventually("a busy answer", || {
        if matches!(waiting.try_wait(), Ok(Some(_))) {
            waiting = waiter();
        }
        let (code, reply) = host.cli(&["lookup", "--id", "job"]);
        assert!(code == 0 || kind(&reply) == "busy", "{reply}");
        code == 3
    });

    // The waiter's request is out; the host dies before replying.
    thread::sleep(Duration::from_millis(200));
    host.kill();
    let (code, reply) = reply(waiting.wait_with_output().unwrap());
    assert_eq!((code, kind(&reply)), (4, "reply_unknown"), "{reply}");
    assert_eq!(fixture.dispatches(), 1);
    // No host holds the run any more; end it before its directory goes away.
    fixture.release();
    // SAFETY: signal 0 only checks for existence.
    eventually("the run to end", || unsafe { libc::kill(pid, 0) } != 0);
}
