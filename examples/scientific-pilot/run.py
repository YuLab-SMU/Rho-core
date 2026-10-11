#!/usr/bin/env python3
"""Run a direct native baseline and a public rho-core CLI consumer pilot."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import signal
import subprocess
import sys
import time

from verify import require, verify

HERE = Path(__file__).resolve().parent
CALLER = "iris-pilot"


def save(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def eventually(description, check, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.02)
    raise AssertionError("timed out: " + description)


def progress(path):
    try:
        value = json.loads(path.read_text())
        return value if value["completed_assignments"] > 0 else None
    except FileNotFoundError:
        return None


def launch_attempts(project, output):
    path = project / (output + ".launch-attempts.jsonl")
    return [json.loads(line) for line in path.read_text().splitlines()]


class Host:
    def __init__(self, binary, project, state, evidence, timeout):
        self.binary, self.evidence, self.timeout = binary, evidence, timeout
        self.socket = state / "host.sock"
        require(len(os.fsencode(self.socket)) < 100, "fixture socket path too long; choose shorter --root")
        env = os.environ.copy()
        env.pop("RHO_CORE_CRASH_AT", None)
        env.pop("RHO_CORE_FAIL_AT", None)
        command = [str(binary), "serve", "--project", str(project), "--state-dir", str(state),
                   "--executor", "python=" + sys.executable, "--executor-arg", "python=-u"]
        self.stderr = (evidence / "host.stderr.txt").open("w")
        self.proc = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=self.stderr, text=True, env=env)
        try:
            if not select.select([self.proc.stdout], [], [], 10)[0]:
                raise AssertionError("host readiness timed out")
            line = self.proc.stdout.readline()
            (evidence / "host-readiness.txt").write_text(line)
            require(bool(line), "host exited before readiness; inspect host.stderr.txt")
            ready = json.loads(line)
            save(evidence / "host-ready.json", ready)
            require(ready["ready"]["socket"] == str(self.socket), "unexpected fixture socket")
            self.calls = []
        except BaseException:
            self.close(check_shutdown=False)
            raise

    def command(self, arguments):
        return [str(self.binary), *arguments, "--socket", str(self.socket), "--caller", CALLER]

    def call(self, arguments):
        # Global flags precede the native argument separator in submit commands.
        if "--" in arguments:
            split = arguments.index("--")
            command = self.command(arguments[:split]) + arguments[split:]
        else:
            command = self.command(arguments)
        output = subprocess.run(command, capture_output=True, text=True, timeout=self.timeout + 10)
        reply = json.loads(output.stdout)
        self.calls.append({"arguments": arguments, "exit_code": output.returncode, "reply": reply,
                           "stderr": output.stderr})
        save(self.evidence / "cli-calls.json", self.calls)
        require(output.returncode == 0 and reply["ok"], "CLI request failed: " + repr(reply))
        return reply["value"]

    def close(self, check_shutdown=True):
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                stdout, _ = self.proc.communicate(timeout=10)
                (self.evidence / "host-shutdown.txt").write_text(stdout)
                if check_shutdown:
                    shutdown = json.loads(stdout)["shutdown"]
                    require(self.proc.returncode == 0 and not shutdown["not_confirmed"]
                            and shutdown["requests_still_open"] == 0 and shutdown["signal_error"] is None,
                            "fixture host shutdown did not confirm clean completion")
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.communicate(timeout=5)
                raise AssertionError("fixture host did not stop normally")
        self.stderr.close()


def submit_arguments(request_id, output):
    return ["submit", "--id", request_id, "--executor", "python", "--code-file", str(HERE / "analyze.py"),
            "--", "--input", "iris.csv", "--output", output]


def read_all(host, request_id, stream):
    offset, texts, reads = 0, [], []
    while True:
        value = host.call(["read", "--id", request_id, "--stream", stream,
                           "--offset", str(offset), "--max-bytes", "4096"])
        reads.append(value)
        require(not value["lossy"], "unexpected lossy scientific output")
        texts.append(value["text"])
        offset = value["next_offset"]
        if not value["more_retained"]:
            break
    (host.evidence / (request_id + "." + stream + ".txt")).write_text("".join(texts))
    save(host.evidence / (request_id + "." + stream + ".reads.json"), reads)
    return "".join(texts)


def complete(host, request_id):
    view = host.call(["wait", "--id", request_id, "--timeout", str(host.timeout)])
    require(view["terminal"] and not view["held"], "native analysis did not reach terminal within timeout")
    require(view["status"]["Finished"]["exit"] == {"Code": 0}, "native analysis failed")
    require(not view["unsaved"], "facts remain unsaved")
    for stream in ("stdout", "stderr"):
        info = view[stream]
        require(info["eof"] and info["retention_error"] is None, "incomplete " + stream)
        require(info["retained_bytes"] == info["observed_bytes"], "truncated " + stream)
    return view


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--root", required=True, type=Path, help="fresh disposable directory; retained as evidence")
    parser.add_argument("--timeout", default=120, type=int, help="max seconds per native analysis (1 to 600)")
    args = parser.parse_args()
    require(1 <= args.timeout <= 600, "timeout must be 1 to 600")
    binary = args.binary.resolve(strict=True)
    root = args.root.resolve()
    root.mkdir(parents=True, exist_ok=False)
    project, evidence = root / "project", root / "evidence"
    project.mkdir()
    evidence.mkdir()
    shutil.copyfile(HERE / "iris.csv", project / "iris.csv")
    metadata = {"binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "python": sys.executable, "python_version": sys.version,
                "input_sha256": hashlib.sha256((project / "iris.csv").read_bytes()).hexdigest(),
                "worker_sha256": hashlib.sha256((HERE / "analyze.py").read_bytes()).hexdigest(),
                "started_at_ns": time.time_ns(), "timeout_seconds": args.timeout}
    save(evidence / "environment.json", metadata)
    host = None
    try:
        # Core-free baseline proves these are real native computations and artifacts.
        try:
            baseline = subprocess.run([sys.executable, str(HERE / "analyze.py"), "--input", "iris.csv", "--output", "native"],
                                      cwd=project, capture_output=True, text=True, timeout=args.timeout)
        except subprocess.TimeoutExpired as error:
            for stream in ("stdout", "stderr"):
                value = getattr(error, stream) or ""
                if isinstance(value, bytes):
                    value = value.decode("utf-8", errors="replace")
                (evidence / ("native." + stream + ".txt")).write_text(value)
            raise
        (evidence / "native.stdout.txt").write_text(baseline.stdout)
        (evidence / "native.stderr.txt").write_text(baseline.stderr)
        require(baseline.returncode == 0, "direct native baseline failed")
        baseline_verification = verify(project / "iris.csv", project / "native/result.json")
        require(len(launch_attempts(project, "native")) == 1, "unexpected direct native launch count")
        save(evidence / "native-verification.json", baseline_verification)
        host = Host(binary, project, root / "state", evidence, args.timeout)
        request_id = "iris-exact-001"
        arguments = submit_arguments(request_id, "managed")
        accepted = host.call(arguments)
        require(accepted["disposition"] == "accepted", "expected first acceptance")
        run_id = accepted["run"]["run_id"]
        eventually("real enumeration progress", lambda: progress(project / "managed/progress.json"))
        waiter = subprocess.Popen(host.command(["wait", "--id", request_id, "--timeout", str(args.timeout)]),
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            time.sleep(0.1)
            require(waiter.poll() is None, "analysis completed before waiter-interruption check")
            waiter.send_signal(signal.SIGTERM)
            interrupted_stdout, interrupted_stderr = waiter.communicate(timeout=5)
            save(evidence / "interrupted-wait.json", {"exit_code": waiter.returncode,
                 "stdout": interrupted_stdout, "stderr": interrupted_stderr, "request_id": request_id, "run_id": run_id})
        finally:
            if waiter.poll() is None:
                waiter.kill()
                waiter.communicate(timeout=5)
        found = host.call(["lookup", "--id", request_id, "--record"])
        require(found["run_id"] == run_id and found["held"], "client interruption did not preserve the original running operation")
        launch_before = (project / "managed/launch.json").read_bytes()
        duplicate = host.call(arguments)
        require(duplicate["disposition"] == "existing" and duplicate["run"]["run_id"] == run_id,
                "same-ID retry did not return original run")
        completed = complete(host, request_id)
        require(completed["run_id"] == run_id, "completion changed identity")
        require((project / "managed/launch.json").read_bytes() == launch_before, "native launch was replayed")
        require(launch_attempts(project, "managed") == [json.loads(launch_before)], "native worker entered more than once")
        managed_verification = verify(project / "iris.csv", project / "managed/result.json")
        require(managed_verification == baseline_verification, "managed/direct native disagreement")
        stdout = read_all(host, request_id, "stdout")
        require('"result": "result.json"' in stdout, "missing retained final output")
        require(read_all(host, request_id, "stderr") == "", "unexpected native error output")
        save(evidence / "managed-verification.json", managed_verification)

        cancelled_id = "iris-exact-cancel-001"
        cancel_arguments = submit_arguments(cancelled_id, "cancelled")
        cancelled_acceptance = host.call(cancel_arguments)
        cancel_run_id = cancelled_acceptance["run"]["run_id"]
        partial_before = eventually("cancellation during real computation", lambda: progress(project / "cancelled/progress.json"))
        require(not partial_before["complete"], "analysis finished before cancellation check")
        requested = host.call(["cancel", "--id", cancelled_id])
        stopped = host.call(["wait", "--id", cancelled_id, "--timeout", "10"])
        require(stopped["run_id"] == cancel_run_id and stopped["terminal"] and not stopped["held"], "stop not confirmed")
        finished = stopped["status"]["Finished"]
        require(finished["exit"] == {"Signal": signal.SIGTERM} and finished["group_released"], "SIGTERM or release unconfirmed")
        require(stopped["cancel"]["term_sent_at"] is not None, "no actual cancellation signal")
        require(not (project / "cancelled/result.json").exists(), "cancelled computation published a complete result")
        partial_after = progress(project / "cancelled/progress.json")
        require(not partial_after["complete"] and partial_after["completed_assignments"] < partial_after["total_assignments"],
                "partial artifact incorrectly asserts complete inference")
        repeated = host.call(cancel_arguments)
        require(repeated["disposition"] == "existing" and repeated["run"]["run_id"] == cancel_run_id, "cancelled identity was replayed")
        cancel_launch = json.loads((project / "cancelled/launch.json").read_text())
        require(launch_attempts(project, "cancelled") == [cancel_launch], "cancelled worker entered more than once")
        read_all(host, cancelled_id, "stdout")
        read_all(host, cancelled_id, "stderr")
        save(evidence / "cancelled-analysis.json", {"requested": requested, "stopped": stopped, "partial": partial_after})
        report = {"passed": True, "native_baseline": baseline_verification,
                  "managed_request_id": request_id, "managed_run_id": run_id,
                  "interrupted_client_continuation": True, "same_identity_no_replay": True,
                  "cancelled_request_id": cancelled_id, "cancelled_run_id": cancel_run_id,
                  "cancellation_confirmed": True, "partial_assignments": partial_after["completed_assignments"],
                  "native_launch_attempts": 1, "managed_launch_attempts": 1, "cancelled_launch_attempts": 1,
                  "completed_at_ns": time.time_ns(), "evidence_root": str(root)}
    except BaseException as error:
        save(evidence / "failure.json", {"type": type(error).__name__, "message": str(error), "at_ns": time.time_ns()})
        raise
    finally:
        try:
            if host is not None:
                host.close()
        except BaseException as error:
            save(evidence / "shutdown-failure.json", {"type": type(error).__name__, "message": str(error), "at_ns": time.time_ns()})
            raise
    # A passing report also requires successful cleanup of this fixture Host.
    save(evidence / "report.json", report)
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()
