#!/usr/bin/env python3
"""Real Linux boundary checks; run as root against an already built binary.

Commands use a disposable, persistent /var/tmp root. prepare-reboot leaves only
its gated fixture runs alive; it never restarts a machine. After an independently
authorized kernel restart, verify-reboot reopens those exact stores.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import pwd
import selectors
import shutil
import signal
import subprocess
import sys
import time
import uuid


PATIENCE = 15.0
CALLER = "linux-platform"
REQUEST = "gated-run"

SNAPSHOT = r"""
import ctypes, json, os, time
from pathlib import Path
path = '/proc/sys/kernel/random/boot_id'
buf = (ctypes.c_long * 32)()
libc = ctypes.CDLL(None, use_errno=True)
with open(path) as stream:
    if libc.fstatfs(stream.fileno(), ctypes.byref(buf)) != 0:
        raise OSError(ctypes.get_errno(), 'fstatfs boot_id')
    boot_id = stream.read().strip()
btime = next(int(line.split()[1]) for line in Path('/proc/stat').read_text().splitlines() if line.startswith('btime '))
print(json.dumps({'boot_id': boot_id,
    'boot_id_scope': 'Kernel' if buf[0] == 0x9fa0 else 'LocalView',
    'filesystem_type': hex(buf[0]), 'pid_namespace': os.readlink('/proc/self/ns/pid'),
    'btime': btime, 'uptime': float(Path('/proc/uptime').read_text().split()[0]),
    'observed_at': time.time()}))
"""

# A private mount namespace changes only the observed boot file. The replacement
# is a fixture file on a non-proc filesystem, so both views remain LocalView.
ALTER_VIEW = r"""import os, subprocess, sys
subprocess.run(['mount', '--make-rprivate', '/'], check=True, timeout=5)
subprocess.run(['mount', '--bind', sys.argv[1], '/proc/sys/kernel/random/boot_id'], check=True, timeout=5)
os.execvp(sys.argv[2], sys.argv[2:])
"""

# The dispatch evidence is durable before the gate is entered. No stdout/stderr
# is written, so killing a host cannot terminate this run through a broken pipe.
GATED = r"""import json, os, time
from pathlib import Path
stat = Path('/proc/self/stat').read_text().rsplit(')', 1)[1].split()
marker = {'pid': os.getpid(), 'pgid': os.getpgrp(), 'uid': os.geteuid(),
          'start_ticks': int(stat[19]), 'pid_namespace': os.readlink('/proc/self/ns/pid')}
fd = os.open('dispatches.jsonl', os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
try:
    os.write(fd, (json.dumps(marker) + '\n').encode())
    os.fsync(fd)
finally:
    os.close(fd)
dirfd = os.open('.', os.O_RDONLY | os.O_DIRECTORY)
try:
    os.fsync(dirfd)
finally:
    os.close(dirfd)
while not Path('release').exists():
    time.sleep(0.05)
"""


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def durable_json(path, value):
    path = Path(path)
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("w", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)
    fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def process_identity(pid):
    try:
        text = Path(f"/proc/{pid}/stat").read_text()
        fields = text.rsplit(")", 1)[1].split()
        uid = Path(f"/proc/{pid}").stat().st_uid
        return {"pid": pid, "start_ticks": int(fields[19]), "uid": uid,
                "pgid": int(fields[2]), "state": fields[0]}
    except (FileNotFoundError, ProcessLookupError):
        return None


def same_process(identity):
    current = process_identity(identity["pid"])
    return (current is not None and current["state"] != "Z"
            and all(current[key] == identity[key] for key in ("start_ticks", "uid")))


def descendants(pid):
    result = []
    pending = [pid]
    while pending:
        parent = pending.pop()
        try:
            children = Path(f"/proc/{parent}/task/{parent}/children").read_text()
        except FileNotFoundError:
            continue
        for child in map(int, children.split()):
            result.append(child)
            pending.append(child)
    return result


def eventually(description, check, timeout=PATIENCE):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.025)
    raise AssertionError(f"timed out waiting for {description}")


def known(record, key):
    return record[key]["Known"]["value"]


def context(view):
    return known(view["record"], "dispatch")["Attempted"]["context"]


class Driver:
    def __init__(self, args):
        self.binary = Path(args.binary).resolve(strict=True)
        self.root = Path(args.root).resolve()
        require(self.root.is_relative_to(Path("/var/tmp")) and self.root != Path("/var/tmp"),
                "--root must be a disposable subdirectory of /var/tmp")
        require(os.geteuid() == 0 and sys.platform == "linux", "run this driver as Linux root")
        require(args.uid != 0 and args.uid != args.other_uid, "host and second UID must differ and host must be ordinary")
        self.uid = args.uid
        self.other_uid = args.other_uid
        self.gid = self.group(self.uid)
        self.hosts = []
        self.jobs = []
        self.root.mkdir(parents=True, exist_ok=True)
        os.chmod(self.root, 0o755)
        self.directory = self.root / ("reboot" if "reboot" in args.command else args.command)
        if args.command == "verify-reboot":
            require((self.directory / "prepared.json").is_file(), "no prepared reboot fixture in this root")
        else:
            self.directory.mkdir(mode=0o755, exist_ok=False)
        self.evidence_path = self.directory / f"{args.command}.json"
        self.evidence = {"command": args.command, "started_at": time.time(),
                         "binary": str(self.binary), "binary_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest(),
                         "uid": self.uid, "other_uid": self.other_uid, "events": []}
        self.save()

    @staticmethod
    def group(uid):
        try:
            return pwd.getpwuid(uid).pw_gid
        except KeyError:
            return uid

    def as_uid(self, uid):
        return ["setpriv", f"--reuid={uid}", f"--regid={self.group(uid)}", "--clear-groups", "--"]

    def event(self, name, value):
        self.evidence["events"].append({"name": name, "at": time.time(), "value": value})
        self.save()

    def save(self):
        durable_json(self.evidence_path, self.evidence)

    def run(self, command, timeout=PATIENCE):
        started_at = time.time()
        started = time.monotonic()
        try:
            completed = subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True,
                                       text=True, timeout=timeout, cwd="/")
        except subprocess.TimeoutExpired as error:
            self.event("bounded_command_timeout", {"command": command, "timeout_seconds": timeout,
                "started_at": started_at, "elapsed_seconds": time.monotonic() - started,
                "stdout": error.stdout.decode(errors="replace") if isinstance(error.stdout, bytes) else error.stdout,
                "stderr": error.stderr.decode(errors="replace") if isinstance(error.stderr, bytes) else error.stderr})
            raise
        return {"returncode": completed.returncode, "stdout": completed.stdout, "stderr": completed.stderr,
                "started_at": started_at, "finished_at": time.time(), "elapsed_seconds": time.monotonic() - started}

    def snapshot(self, mode="normal", host=None):
        prefix = []
        if host is not None:
            prefix = ["nsenter", "--target", str(host.identity["pid"]), "--mount", "--pid", "--"]
        elif mode == "raw":
            prefix = ["unshare", "--mount", "--mount-proc", "--"]
        result = self.run(prefix + ["/usr/bin/python3", "-c", SNAPSHOT])
        require(result["returncode"] == 0, f"snapshot failed: {result}")
        value = json.loads(result["stdout"])
        self.event("snapshot", {"mode": mode, "value": value})
        return value

    def fixture(self, name, existing=False):
        base = self.directory / name
        if not existing:
            base.mkdir(mode=0o755)
            for subdirectory in (base / "project", base / "state"):
                subdirectory.mkdir(mode=0o700)
                os.chown(subdirectory, self.uid, self.gid)
        return base

    def serve(self, base, mode="normal"):
        host = Host(self, base, mode)
        self.hosts.append(host)
        return host

    def cli(self, host, arguments, uid=None):
        uid = self.uid if uid is None else uid
        command = self.as_uid(uid) + [str(self.binary)] + arguments + ["--socket", str(host.socket), "--caller", CALLER]
        result = self.run(command)
        try:
            result["reply"] = json.loads(result["stdout"])
        except json.JSONDecodeError:
            result["reply"] = None
        self.event("cli", {"uid": uid, "arguments": arguments, "result": result})
        return result["returncode"], result["reply"]

    def success(self, host, arguments):
        code, reply = self.cli(host, arguments)
        require(code == 0 and reply and reply["ok"], f"request failed: {code}, {reply}")
        return reply["value"]

    def submit(self, host):
        return self.success(host, ["submit", "--id", REQUEST, "--executor", "python", "--code", GATED])

    def lookup(self, host):
        return self.success(host, ["lookup", "--id", REQUEST, "--record"])

    def dispatches(self, base):
        marker = base / "project" / "dispatches.jsonl"
        return [json.loads(line) for line in marker.read_text().splitlines()] if marker.exists() else []

    def capture_job(self, host):
        markers = eventually("durable dispatch marker", lambda: self.dispatches(host.base))
        require(len(markers) == 1, f"dispatch count: {markers}")
        marker = markers[0]
        candidates = [process_identity(pid) for pid in descendants(host.process.pid)]
        job = next((candidate for candidate in candidates if candidate and
                    candidate["start_ticks"] == marker["start_ticks"] and candidate["uid"] == marker["uid"]), None)
        require(job is not None, "cannot associate fixture marker with a live host descendant")
        require(job["pgid"] == job["pid"], f"fixture run does not own its process group: {job}")
        self.jobs.append(job)
        self.event("fixture_process", {"marker": marker, "outer_process": job})
        return job

    def protected(self, host, view=None):
        view = self.lookup(host) if view is None else view
        require(not view["terminal"] and "Detached" in view["status"], f"unexpected completion: {view}")
        require("Protected" in view["retention"], f"unexpected removable record: {view}")
        require(view["record"]["group_released"].get("Known", {}).get("value") is not True,
                f"unexpected release: {view}")
        code, reply = self.cli(host, ["forget", "--id", REQUEST])
        require(code == 1 and reply["error"]["kind"] == "protected", f"forget was not protected: {reply}")
        return view

    def duplicate(self, host, run_id):
        reply = self.submit(host)
        require(reply["disposition"] == "existing" and reply["run"]["run_id"] == run_id,
                f"duplicate did not retain the original run: {reply}")
        require(len(self.dispatches(host.base)) == 1, "duplicate submission replayed the fixture")

    def cleanup(self):
        errors = []
        for host in reversed(self.hosts):
            try:
                host.close()
            except Exception as error:
                errors.append(f"host {host.base}: {type(error).__name__}: {error}")
        for job in self.jobs:
            try:
                if same_process(job):
                    require(os.getpgid(job["pid"]) == job["pgid"], "fixture process group changed before cleanup")
                    os.killpg(job["pgid"], signal.SIGKILL)
            except ProcessLookupError:
                pass
            except Exception as error:
                errors.append(f"fixture {job['pid']}: {type(error).__name__}: {error}")
        for job in self.jobs:
            try:
                eventually("fixture process cleanup", lambda job=job: not same_process(job))
            except Exception as error:
                errors.append(f"fixture {job['pid']}: {type(error).__name__}: {error}")
        self.event("cleanup", {"captured_fixture_processes": self.jobs, "errors": errors})
        require(not errors, f"fixture cleanup failed: {errors}")


class Host:
    def __init__(self, driver, base, mode):
        self.driver, self.base, self.mode = driver, base, mode
        prefix = []
        if mode in ("raw", "nested"):
            prefix = ["unshare", "--mount"]
            if mode == "nested":
                prefix += ["--pid", "--fork"]
            prefix += ["--mount-proc", "--"]
        elif mode == "local-altered":
            boot_file = base / "altered-boot-id"
            with boot_file.open("w") as stream:
                stream.write(str(uuid.uuid4()) + "\n")
                stream.flush()
                os.fsync(stream.fileno())
            prefix = ["unshare", "--mount", "--", "/usr/bin/python3", "-c", ALTER_VIEW, str(boot_file)]
        command = prefix + driver.as_uid(driver.uid) + [str(driver.binary), "serve", "--project", str(base / "project"),
                  "--state-dir", str(base / "state"), "--executor", "python=/usr/bin/python3"]
        self.stderr = (base / f"host-{mode}-{time.time_ns()}.stderr").open("wb")
        self.process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                        stderr=self.stderr, start_new_session=True, cwd="/")
        self.wrapper = process_identity(self.process.pid)
        self.identity = self.wrapper
        self.ready = False
        try:
            selector = selectors.DefaultSelector()
            selector.register(self.process.stdout, selectors.EVENT_READ)
            data = b""
            deadline = time.monotonic() + PATIENCE
            while b"\n" not in data:
                require(time.monotonic() < deadline, "host readiness timed out")
                for key, _ in selector.select(min(0.1, max(0, deadline - time.monotonic()))):
                    chunk = os.read(key.fd, 4096)
                    require(bool(chunk), f"host ended before readiness; inspect {self.stderr.name}")
                    data += chunk
            selector.close()
            ready = json.loads(data.split(b"\n", 1)[0])
            self.socket = Path(ready["ready"]["socket"])
            if mode == "nested":
                matches = [process_identity(pid) for pid in descendants(self.process.pid)]
                self.identity = next((item for item in matches if item and item["uid"] == driver.uid), None)
            else:
                # setpriv changes the UID after Popen returns; record the host's
                # identity once its own readiness confirms that exec completed.
                self.identity = process_identity(self.process.pid)
            require(self.identity is not None and self.identity["uid"] == driver.uid,
                    "host did not start under the selected ordinary UID")
            self.snapshot = driver.snapshot(mode, self)
            driver.event("host_ready", {"mode": mode, "ready": ready, "outer_process": self.identity,
                                       "wrapper": self.wrapper, "stderr_path": self.stderr.name})
            self.ready = True
        except BaseException:
            self.close()
            raise

    def close(self, crash=False):
        if not self.ready and self.wrapper is not None:
            current = process_identity(self.process.pid)
            if current is not None and current["start_ticks"] == self.wrapper["start_ticks"]:
                self.identity = current
        was_live = self.identity is not None and same_process(self.identity)
        if self.identity is not None and same_process(self.identity):
            os.kill(self.identity["pid"], signal.SIGKILL if crash else signal.SIGTERM)
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            if self.identity is not None and same_process(self.identity):
                os.kill(self.identity["pid"], signal.SIGKILL)
            if self.wrapper is not None and same_process(self.wrapper):
                os.kill(self.wrapper["pid"], signal.SIGKILL)
            self.process.wait(timeout=5)
        require(self.identity is None or not same_process(self.identity), "fixture host survived bounded stop")
        if was_live:
            self.driver.event("host_closed", {"mode": self.mode, "process": self.identity,
                                              "crash": crash, "returncode": self.process.returncode})
        self.stderr.close()
        self.process.stdout.close()


def uid_check(driver):
    base = driver.fixture("boundary")
    host = driver.serve(base)
    require(driver.success(host, ["capability"])["project_root"] == str(base / "project"), "same-UID request failed")
    require((base / "state").stat().st_mode & 0o777 == 0o700, "state directory is not private")
    require(host.socket.stat().st_mode & 0o777 == 0o600, "socket is not private")
    code, reply = driver.cli(host, ["capability"], driver.other_uid)
    require(code == 3 and reply["error"]["kind"] == "host_unavailable", f"default permissions allowed another UID: {reply}")
    # Relax only this disposable fixture's access to reach SO_PEERCRED, rather
    # than accidentally scoring the filesystem permission rejection twice.
    os.chmod(base / "state", 0o711)
    os.chmod(host.socket, 0o666)
    code, reply = driver.cli(host, ["submit", "--id", "forbidden", "--executor", "python", "--code", GATED], driver.other_uid)
    require(code == 3 and reply["error"]["kind"] == "refused", f"SO_PEERCRED did not refuse: {reply}")
    raw = r"""import json, socket, sys
s=socket.socket(socket.AF_UNIX); s.settimeout(3); s.connect(sys.argv[1])
f=s.makefile('rb'); greeting=json.loads(f.readline(4096))
try:
    s.sendall((json.dumps({'op':'submit','caller':'linux-platform','request_id':'raw-forbidden',
        'executor':'python','workdir':'.','code':sys.argv[2],'args':[]})+'\n').encode())
    reply=f.readline(4096).decode()
except (BrokenPipeError, ConnectionResetError):
    reply=''
print(json.dumps({'greeting':greeting,'reply':reply}))
"""
    result = driver.run(driver.as_uid(driver.other_uid) + ["/usr/bin/python3", "-c", raw, str(host.socket), GATED])
    require(result["returncode"] == 0, f"raw refusal probe failed: {result}")
    refusal = json.loads(result["stdout"])
    driver.event("raw_refusal", refusal)
    require(refusal["greeting"]["state"] == "refused" and refusal["reply"] == "", f"raw peer accepted: {refusal}")
    operations = driver.success(host, ["list"])
    require(operations == {"runs": []}, f"refused requests created operations: {operations}")
    require(driver.dispatches(base) == [], "refused requests dispatched a process")
    os.chmod(host.socket, 0o600)
    os.chmod(base / "state", 0o700)
    driver.event("uid_checks_passed", {"host_uid": driver.uid, "peer_uid": driver.other_uid})


def namespace_check(driver):
    local = driver.fixture("local-to-other-local")
    host = driver.serve(local)
    accepted = driver.submit(host)
    run_id = accepted["run"]["run_id"]
    job = driver.capture_job(host)
    original = context(driver.lookup(host))
    require(original["boot_id_scope"] == "LocalView", f"normal OrbStack view must be LocalView: {original}")
    host.close(crash=True)
    changed = driver.serve(local, "local-altered")
    require(changed.snapshot["boot_id_scope"] == "LocalView" and changed.snapshot["boot_id"] != original["boot_id"]
            and changed.snapshot["pid_namespace"] == original["pid_namespace"], "LocalView->LocalView fixture did not change only boot ID")
    driver.protected(changed)
    driver.duplicate(changed, run_id)
    require(same_process(job), "another local boot view incorrectly stopped the original process")

    local = driver.fixture("local-to-kernel")
    host = driver.serve(local)
    accepted = driver.submit(host)
    run_id = accepted["run"]["run_id"]
    job = driver.capture_job(host)
    original = context(driver.lookup(host))
    require(original["boot_id_scope"] == "LocalView", f"normal OrbStack view must be LocalView: {original}")
    host.close(crash=True)
    changed = driver.serve(local, "raw")
    require(changed.snapshot["boot_id_scope"] == "Kernel" and changed.snapshot["boot_id"] != original["boot_id"]
            and changed.snapshot["pid_namespace"] == original["pid_namespace"], "LocalView->Kernel fixture did not change only boot view")
    driver.protected(changed)
    driver.duplicate(changed, run_id)
    require(same_process(job), "altered boot view incorrectly stopped the original process")

    kernel = driver.fixture("kernel-to-other-pidns")
    host = driver.serve(kernel, "raw")
    accepted = driver.submit(host)
    run_id = accepted["run"]["run_id"]
    job = driver.capture_job(host)
    original = context(driver.lookup(host))
    require(original["boot_id_scope"] == "Kernel", f"raw proc view must be Kernel: {original}")
    host.close(crash=True)
    changed = driver.serve(kernel, "nested")
    require(changed.snapshot["boot_id_scope"] == "Kernel" and changed.snapshot["boot_id"] == original["boot_id"]
            and changed.snapshot["pid_namespace"] != original["pid_namespace"], "Kernel->Kernel fixture did not change only PID namespace")
    driver.protected(changed)
    driver.duplicate(changed, run_id)
    require(same_process(job), "other PID namespace incorrectly stopped the original process")
    driver.event("namespace_checks_passed", {"local_to_other_local": True, "local_to_kernel": True,
                                             "same_kernel_other_pid_namespace": True})


def prepare_reboot(driver):
    prepared = {"prepared_at": time.time(), "binary_sha256": driver.evidence["binary_sha256"],
                "uid": driver.uid, "fixtures": {}}
    for name, mode, scope in (("normal", "normal", "LocalView"), ("raw", "raw", "Kernel")):
        base = driver.fixture(name)
        host = driver.serve(base, mode)
        accepted = driver.submit(host)
        run_id = accepted["run"]["run_id"]
        job = driver.capture_job(host)
        before = driver.lookup(host)
        require(before["held"] and "Running" in before["status"] and not before["terminal"],
                f"fixture has no live held run before host crash: {before}")
        require("SpawnObserved" in known(before["record"], "execution") and before["unsaved"] is None,
                f"fixture spawn is not durably observed: {before}")
        require(context(before)["boot_id_scope"] == scope, f"unexpected dispatch scope: {before}")
        require(before["stdout"]["retained_bytes"] == 0 and before["stderr"]["retained_bytes"] == 0,
                f"gated fixture produced output: {before}")
        driver.duplicate(host, run_id)
        host.close(crash=True)
        require(same_process(job), "fixture ended when its host was killed")
        restarted = driver.serve(base, mode)
        driver.protected(restarted)
        driver.duplicate(restarted, run_id)
        restarted.close(crash=True)
        require(same_process(job), "fixture ended before the external reboot")
        prepared["fixtures"][name] = {"base": str(base), "mode": mode, "run_id": run_id,
                                      "context": context(before), "before": before,
                                      "snapshot": host.snapshot, "process": job}
    prepared["ready_at"] = time.time()
    durable_json(driver.directory / "prepared.json", prepared)
    driver.event("prepared_for_external_reboot", prepared)
    # The caller, not this script, authorizes and performs the machine restart.
    driver.jobs.clear()


def verify_reboot(driver):
    prepared = json.loads((driver.directory / "prepared.json").read_text())
    require(prepared["uid"] == driver.uid, "verification UID differs from preparation")
    require(prepared["binary_sha256"] == driver.evidence["binary_sha256"], "binary changed between preparation and verification")
    now = driver.snapshot("raw")
    before = prepared["fixtures"]["raw"]["snapshot"]
    require(now["boot_id_scope"] == "Kernel" and before["boot_id_scope"] == "Kernel", "kernel boot source unavailable")
    require(now["boot_id"] != before["boot_id"], "kernel boot_id did not change; reboot has not been established")
    require(now["btime"] != before["btime"], "kernel btime did not change")
    require(now["uptime"] <= max(0, time.time() - prepared["ready_at"]) + 30,
            "current uptime does not fit a boot since preparation")
    driver.event("kernel_reboot_established", {"before": before, "after": now})
    for name in ("normal", "raw"):
        fixture = prepared["fixtures"][name]
        base = Path(fixture["base"])
        require(base == driver.directory / name, "prepared fixture path escapes the selected root")
        host = driver.serve(base, fixture["mode"])
        view = driver.lookup(host)
        require(view["run_id"] == fixture["run_id"] and not view["held"], "did not recover the original run")
        driver.duplicate(host, fixture["run_id"])
        if name == "raw":
            gone = view["status"].get("Gone", {})
            evidence = gone.get("evidence", {}).get("BootChanged", {})
            require(evidence == {"dispatched": fixture["context"]["boot_id"], "current": now["boot_id"]},
                    f"raw record lacks exact BootChanged evidence: {view}")
            require(view["terminal"] and gone["group_released"] and view["retention"] == "Removable",
                    f"raw record is not removable: {view}")
            require(view["record"]["execution"]["Known"]["source"] == "BootSession"
                    and view["record"]["group_released"]["Known"]["source"] == "BootSession", "wrong reboot fact source")
            driver.success(host, ["forget", "--id", REQUEST])
            code, missing = driver.cli(host, ["lookup", "--id", REQUEST])
            require(code == 1 and missing["error"]["kind"] == "not_found", "raw record was not forgotten")
        else:
            old = fixture["context"]
            current = host.snapshot
            comparable = all(old[key] == current[key] for key in ("boot_id", "boot_id_scope", "pid_namespace"))
            if not comparable:
                driver.protected(host, view)
                driver.event("local_reboot_record_stays_protected", {"dispatched": old, "current": current})
            elif "Gone" in view["status"]:
                require(view["status"]["Gone"]["evidence"] == "MainPidAbsent", "LocalView incorrectly supplied BootChanged")
                require(view["status"]["Gone"]["group_released"] and view["retention"] == "Removable", "proven local absence was not removable")
                driver.success(host, ["forget", "--id", REQUEST])
                driver.event("unchanged_local_context_proves_pid_absence", view)
            else:
                driver.protected(host, view)
                driver.event("unchanged_local_context_without_absence_stays_protected", view)
        require(len(driver.dispatches(base)) == 1, "reboot recovery replayed the fixture")
    driver.event("reboot_checks_passed", {"dispatches_per_fixture": 1})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("uid", "namespace", "prepare-reboot", "verify-reboot"))
    parser.add_argument("--binary", required=True, help="already built Linux rho-core executable")
    parser.add_argument("--root", required=True, help="fresh persistent fixture directory below /var/tmp")
    parser.add_argument("--uid", type=int, default=501, help="ordinary host UID (default: 501)")
    parser.add_argument("--other-uid", type=int, default=65534, help="refused peer UID (default: nobody 65534)")
    args = parser.parse_args()
    for tool in ("setpriv", "unshare", "nsenter", "mount"):
        require(shutil.which(tool), f"required maintained Linux tool is unavailable: {tool}")
    driver = Driver(args)
    try:
        {"uid": uid_check, "namespace": namespace_check,
         "prepare-reboot": prepare_reboot, "verify-reboot": verify_reboot}[args.command](driver)
        driver.evidence["result"] = "passed"
    except BaseException as error:
        driver.evidence["result"] = "failed"
        driver.evidence["failure"] = f"{type(error).__name__}: {error}"
        raise
    finally:
        try:
            driver.cleanup()
        except BaseException as error:
            driver.evidence["result"] = "failed"
            driver.evidence["cleanup_failure"] = f"{type(error).__name__}: {error}"
            raise
        finally:
            driver.evidence["finished_at"] = time.time()
            driver.save()
    print(json.dumps({"result": "passed", "evidence": str(driver.evidence_path)}, ensure_ascii=False))


if __name__ == "__main__":
    main()
