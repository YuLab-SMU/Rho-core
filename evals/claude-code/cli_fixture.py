"""Bounded evaluation jobs driven solely through the real public Core CLI."""
import hashlib
import json
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import subprocess
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
BIN = ROOT / "target/debug/rho-core"
CALLER = "eval-agent"
JOBS = {
    "sum": "printf 'sum\\n' >> dispatches.txt; seq 1 100 > numbers.txt; awk '{s+=$1} END {print s}' numbers.txt",
    "loud": "printf 'loud\\n' >> dispatches.txt; i=0; while [ $i -lt 200 ]; do printf x; i=$((i+1)); done",
    "hold": "exec </dev/null; printf 'hold\\n' >> dispatches.txt; printf '%s\\n' $$ > held.pid; while [ ! -f release ]; do sleep 0.05; done; printf done",
}


class CLIError(RuntimeError):
    def __init__(self, code, error):
        self.exit_code, self.error = code, error
        super().__init__(f"CLI exit {code}: {error}")


class Fixture:
    def __init__(self, root):
        self.root = Path(root)
        self.analysis = self.root / "project/analysis"
        self.analysis.mkdir(parents=True)
        self.proc = None
        self.detached_pid = None
        self.fault = None
        self.commands = []
        self.fault_evidence = []
        self.lock = threading.RLock()
        self.start()

    def start(self):
        env = os.environ.copy()
        env.pop("RHO_CORE_CRASH_AT", None)
        env.pop("RHO_CORE_FAIL_AT", None)
        self.proc = subprocess.Popen([str(BIN), "serve", "--project", str(self.root / "project"),
            "--state-dir", str(self.root / "state"), "--executor", "sh=/bin/sh",
            "--limit", "max_stream_bytes=64", "--limit", "stop_grace_ms=100"],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, env=env)
        try:
            if not select.select([self.proc.stdout], [], [], 10)[0]:
                raise RuntimeError("CLI host readiness timeout")
            line = self.proc.stdout.readline()
            if not line:
                error = self.proc.stderr.read(4096)
                raise RuntimeError("CLI host failed before readiness: " + error.strip())
            ready = json.loads(line)
            expected_socket = self.root / "state/host.sock"
            if Path(ready["ready"]["socket"]) != expected_socket:
                raise RuntimeError("CLI host reported an unexpected fixture socket")
            self.socket = expected_socket
        except BaseException:
            self._stop(crash=True)
            raise

    def _stop(self, crash=False):
        if self.proc is not None:
            if self.proc.poll() is None:
                self.proc.send_signal(signal.SIGKILL if crash else signal.SIGTERM)
            try:
                self.proc.wait(timeout=8)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
            self.proc.stdout.close()
            self.proc.stderr.close()
            self.proc = None

    def _cli(self, args, socket_path=None):
        command = [str(BIN), *args, "--socket", str(socket_path or self.socket), "--caller", CALLER]
        completed = subprocess.run(command, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=15)
        receipt = {"argv": command, "exit_code": completed.returncode,
                   "stdout": completed.stdout, "stderr": completed.stderr,
                   "stdout_sha256": hashlib.sha256(completed.stdout.encode()).hexdigest()}
        self.commands.append(receipt)
        if args == ["capability", "--contract"] and completed.returncode == 0:
            reply = {"ok": True, "value": completed.stdout}
        else:
            try:
                reply = json.loads(completed.stdout)
            except ValueError as error:
                raise RuntimeError(f"CLI returned no JSON (exit {completed.returncode})") from error
        receipt["reply"] = json.loads(json.dumps(reply))
        return completed.returncode, reply

    def _record_view(self, value):
        if not isinstance(value, dict) or not value.get("request_id"):
            return value
        code, reply = self._cli(["lookup", "--id", value["request_id"], "--record"])
        if code:
            raise CLIError(code, reply.get("error"))
        observed = reply["value"]
        return {**observed, "operation": observed["record"]}

    def _lost_reply(self, args):
        """Forward a complete real request, retain its actual reply, then drop it."""
        path = self.root / "lost.sock"
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(path))
        listener.listen(1)
        listener.settimeout(10)
        forwarded = {}
        errors = []
        def proxy():
            try:
                front, _ = listener.accept()
                with front, socket.socket(socket.AF_UNIX) as back:
                    front.settimeout(10)
                    back.settimeout(10)
                    back.connect(str(self.socket))
                    with back.makefile("rb") as reader, front.makefile("rb") as source:
                        greeting = reader.readline(4097)
                        front.sendall(greeting)
                        request = source.readline(32769)
                        if not request.endswith(b"\n") or len(request) > 32768:
                            raise RuntimeError("fault proxy requires a complete bounded request")
                        back.sendall(request)
                        forwarded["host_reply"] = json.loads(reader.readline(1024 * 1024))
                        forwarded["request"] = json.loads(request)
                        # Closing the front without sending the actual reply makes exit 4.
            except Exception as error:
                errors.append(str(error))
            finally:
                listener.close()
        worker = threading.Thread(target=proxy)
        worker.start()
        try:
            result = self._cli(args, path)
            worker.join(timeout=12)
            if worker.is_alive() or errors or not forwarded.get("host_reply", {}).get("ok"):
                raise RuntimeError(f"lost-reply fixture failed: {errors}")
            self.fault_evidence.append({"mode": "drop_after_real_acceptance", **forwarded})
            return result
        finally:
            listener.close()
            worker.join(timeout=1)
            path.unlink(missing_ok=True)

    def invoke(self, request):
        with self.lock:
            op = request.get("op")
            if op == "capability":
                code, reply = self._cli(["capability", "--contract"] if request.get("contract") else ["capability"])
                if not code and not request.get("contract"):
                    reply["value"]["jobs"] = list(JOBS)
            elif op == "submit":
                job = request.get("job")
                if job not in JOBS:
                    raise ValueError("only predeclared evaluation jobs are allowed")
                args = ["submit", "--id", request["request_id"], "--executor", "sh",
                        "--workdir", "analysis", "--code", JOBS[job]]
                fault, self.fault = self.fault, None
                if fault == "not_sent":
                    # Stop only this empty fixture host, observe absence, then restore it.
                    self._stop()
                    code, reply = self._cli(args)
                    self.start()
                    after = self.snapshot()
                    self.fault_evidence.append({"mode": "absent_fixture_host", "reply": reply,
                                                "exit_code": code, "state_after_error": after})
                elif fault == "reply_unknown":
                    code, reply = self._lost_reply(args)
                else:
                    code, reply = self._cli(args)
                if not code:
                    reply["value"]["run"] = self._record_view(reply["value"]["run"])
            elif op == "list_operations":
                code, reply = self._cli(["list"])
                if not code:
                    reply["value"] = [self._record_view(v) for v in reply["value"]["runs"]]
            elif op in ["lookup", "wait", "cancel", "read_output"]:
                args = [{"read_output": "read"}.get(op, op), "--id", request["request_id"]]
                if op == "lookup":
                    args += ["--record"]
                if op == "wait":
                    ms = request.get("timeout_ms", 1000)
                    args += ["--timeout", str(min(5000, max(0, int(ms))) / 1000)]
                if op == "read_output":
                    args += ["--stream", request.get("stream", "stdout"), "--offset", str(request.get("offset", 0)),
                             "--max-bytes", "4096"]
                code, reply = self._cli(args)
                if not code and op != "read_output":
                    reply["value"] = self._record_view(reply["value"])
            else:
                raise ValueError("unsupported public CLI evaluation operation")
            return {**reply, "cli_exit": code}

    def call(self, request):
        reply = self.invoke(request)
        if not reply["ok"]:
            raise CLIError(reply["cli_exit"], reply["error"])
        return reply["value"]

    def seed(self, seed):
        if seed in ["not_sent", "reply_unknown"]:
            self.fault = seed
            return None
        if not seed:
            return None
        job = "hold" if seed == "crash" else seed
        rid = {"sum": "sum-100", "hold": "hold-1", "loud": "loud-1"}[job]
        view = self.call({"op": "submit", "request_id": rid, "job": job})["run"]
        if job == "hold":
            deadline = time.monotonic() + 5
            pidfile = self.analysis / "held.pid"
            while not pidfile.exists():
                if time.monotonic() >= deadline:
                    raise RuntimeError("CLI hold job did not reach its gate")
                time.sleep(.01)
            pid = int(pidfile.read_text())
        else:
            view = self.call({"op": "wait", "request_id": rid, "timeout_ms": 5000})
            if not view["terminal"]:
                raise RuntimeError("CLI seed failed to complete")
        if seed == "crash":
            self.detached_pid = pid
            self._stop(crash=True)
            self.start()
        elif seed == "sum":
            self._stop()
            self.start()
        return view["run_id"]

    def snapshot(self):
        return {"operations": self.call({"op": "list_operations"}),
                "dispatches": (self.analysis / "dispatches.txt").read_text().splitlines()
                    if (self.analysis / "dispatches.txt").exists() else [],
                "numbers": (self.analysis / "numbers.txt").read_text()
                    if (self.analysis / "numbers.txt").exists() else None,
                "cli_commands": list(self.commands), "delivery_faults": list(self.fault_evidence)}

    def install_entrypoint(self, workspace):
        workspace = Path(workspace)
        shutil.copyfile(Path(__file__).with_name("cli_entry.py"), workspace / "rho-core")
        (workspace / "rho-core").chmod(0o700)
        (workspace / "jobs").mkdir()
        for job, code in JOBS.items():
            (workspace / "jobs" / (job + ".sh")).write_text(code + "\n")

    def close(self):
        self._stop()
        if self.detached_pid:
            try:
                os.killpg(self.detached_pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
