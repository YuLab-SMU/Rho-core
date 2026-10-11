#!/usr/bin/env python3
"""Check macOS peer UID refusal with one disposable host and existing users.

Run ``uid`` as root, using an already built binary and a fresh direct child of
/private/tmp. ``--preflight`` is read-only and reports whether that is possible.
Only the fixture socket and its state directory temporarily become accessible
to the second UID. No users, OS configuration or existing hosts are changed.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import pwd
import selectors
import shutil
import stat
import subprocess
import sys
import time


PATIENCE = 15.0
CALLER = "macos-platform"
FORBIDDEN_CODE = "printf 'forbidden\\n' >> dispatches.txt"
RAW_PROBE = r"""import json, os, socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(3)
s.connect(sys.argv[1])
f = s.makefile('rb')
greeting = json.loads(f.readline(4096))
try:
    s.sendall((json.dumps({'op': 'submit', 'caller': 'macos-platform',
        'request_id': 'raw-forbidden', 'executor': 'sh', 'workdir': '.',
        'code': sys.argv[2], 'args': []}) + '\n').encode())
    reply = f.readline(4096).decode()
except (BrokenPipeError, ConnectionResetError):
    reply = ''
print(json.dumps({'uid': os.geteuid(), 'gid': os.getegid(),
                 'greeting': greeting, 'reply': reply}))
"""


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def durable_json(path, value):
    temporary = path.with_name(path.name + ".tmp")
    with temporary.open("w", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def preflight(args):
    require(sys.platform == "darwin", "this check requires macOS")
    require(sys.version_info >= (3, 9), "this driver requires Python 3.9 or newer")
    source = Path(args.binary).resolve(strict=True)
    require(source.is_file() and os.access(source, os.X_OK), "--binary must be an executable file")
    root = Path(args.root).absolute()
    require(root.parent == Path("/private/tmp") and root.name not in ("", ".", ".."),
            "--root must be a fresh direct child of /private/tmp")
    require(not root.exists() and not root.is_symlink(), "--root already exists; choose a fresh fixture root")
    require(len(os.fsencode(root / "state" / "host.sock")) < 104,
            "--root is too long for a macOS Unix socket")
    uid = int(args.uid if args.uid is not None else os.environ.get("SUDO_UID", os.getuid()))
    other_uid = args.other_uid if args.other_uid is not None else pwd.getpwnam("nobody").pw_uid
    require(0 < uid <= 0xFFFFFFFF and 0 <= other_uid <= 0xFFFFFFFF and uid != other_uid,
            "ordinary host UID and peer UID must be existing, different users")
    host_user, peer_user = pwd.getpwuid(uid), pwd.getpwuid(other_uid)
    python = Path(args.python).resolve(strict=True)
    require(python.is_file() and os.access(python, os.X_OK), "--python must be an executable file")
    return {"platform": sys.platform, "supervisor_uid": os.geteuid(),
            "root_available": os.geteuid() == 0, "root": str(root),
            "binary": str(source), "binary_sha256": hashlib.sha256(source.read_bytes()).hexdigest(),
            "python": str(python), "host_uid": uid, "host_gid": host_user.pw_gid,
            "peer_uid": other_uid, "peer_gid": peer_user.pw_gid}


class Driver:
    def __init__(self, info):
        self.info = info
        self.root = Path(info["root"])
        self.root.mkdir(mode=0o755, exist_ok=False)
        os.chmod(self.root, 0o755)
        self.evidence_path = self.root / "uid.json"
        self.evidence = {"command": "uid", "started_at": time.time(),
                         "preflight": info, "events": [], "result": "running"}
        self.host = None
        self.stderr = None
        self.save()

    def setup(self):
        self.binary = self.root / "rho-core"
        shutil.copyfile(self.info["binary"], self.binary)
        os.chmod(self.binary, 0o755)
        require(hashlib.sha256(self.binary.read_bytes()).hexdigest() == self.info["binary_sha256"],
                "fixture binary copy differs from the selected binary")
        self.project, self.state = self.root / "project", self.root / "state"
        for directory in (self.project, self.state):
            directory.mkdir(mode=0o700)
            os.chown(directory, self.info["host_uid"], self.info["host_gid"])

    def save(self):
        durable_json(self.evidence_path, self.evidence)

    def event(self, name, value):
        self.evidence["events"].append({"name": name, "at": time.time(), "value": value})
        self.save()

    def user_options(self, peer=False):
        prefix = "peer" if peer else "host"
        return {"user": self.info[f"{prefix}_uid"], "group": self.info[f"{prefix}_gid"],
                "extra_groups": [], "cwd": "/", "stdin": subprocess.DEVNULL}

    def run(self, command, peer=False):
        began = time.time()
        try:
            result = subprocess.run(command, capture_output=True, text=True,
                                    timeout=PATIENCE, **self.user_options(peer))
        except subprocess.TimeoutExpired as error:
            self.event("command_timeout", {"command": command, "peer": peer,
                "timeout_seconds": PATIENCE,
                "stdout": error.stdout.decode(errors="replace") if isinstance(error.stdout, bytes) else error.stdout,
                "stderr": error.stderr.decode(errors="replace") if isinstance(error.stderr, bytes) else error.stderr})
            raise
        value = {"command": command, "uid": self.info["peer_uid" if peer else "host_uid"],
                 "returncode": result.returncode, "stdout": result.stdout, "stderr": result.stderr,
                 "started_at": began, "finished_at": time.time()}
        self.event("command", value)
        return value

    def cli(self, arguments, peer=False):
        result = self.run([str(self.binary), *arguments, "--socket", str(self.socket),
                           "--caller", CALLER], peer)
        require(bool(result["stdout"]), f"CLI returned no JSON: {result}")
        return result["returncode"], json.loads(result["stdout"])

    def success(self, arguments):
        code, reply = self.cli(arguments)
        require(code == 0 and reply.get("ok") is True, f"same UID request failed: {code}, {reply}")
        return reply["value"]

    def serve(self):
        # Prove the actual credentials of both dropped processes before using
        # them as platform evidence. No launchctl bootstrap change implies UID.
        probe = "import json,os; print(json.dumps({'uid':os.geteuid(),'gid':os.getegid(),'groups':os.getgroups()}))"
        for peer in (False, True):
            result = self.run([self.info["python"], "-c", probe], peer)
            require(result["returncode"] == 0, f"UID probe could not run: {result}")
            identity = json.loads(result["stdout"])
            prefix = "peer" if peer else "host"
            require(identity["uid"] == self.info[f"{prefix}_uid"]
                    and identity["gid"] == self.info[f"{prefix}_gid"],
                    f"incorrect dropped credentials: {identity}")
        self.stderr = (self.root / "host.stderr").open("wb")
        self.host = subprocess.Popen([str(self.binary), "serve", "--project", str(self.project),
            "--state-dir", str(self.state), "--executor", "sh=/bin/sh"],
            stdout=subprocess.PIPE, stderr=self.stderr, start_new_session=True,
            **self.user_options())
        with selectors.DefaultSelector() as selector:
            selector.register(self.host.stdout, selectors.EVENT_READ)
            data = b""
            deadline = time.monotonic() + PATIENCE
            while b"\n" not in data:
                require(time.monotonic() < deadline, "fixture host readiness timed out")
                for key, _ in selector.select(0.1):
                    chunk = os.read(key.fd, 4096)
                    require(bool(chunk), f"host ended before readiness; inspect {self.stderr.name}")
                    data += chunk
                    require(len(data) <= 65536, "fixture readiness exceeds 64 KiB")
        ready = json.loads(data.split(b"\n", 1)[0])
        self.socket = Path(ready["ready"]["socket"])
        require(self.socket == self.state / "host.sock", f"fixture socket escaped state directory: {ready}")
        require(self.socket.stat().st_uid == self.info["host_uid"], "host socket has the wrong owner")
        self.event("host_ready", {"ready": ready, "pid": self.host.pid,
                                   "stderr_path": str(self.stderr.name)})

    def check(self):
        self.setup()
        self.serve()
        require(self.success(["capability"])["project_root"] == str(self.project),
                "same UID capability does not report the fixture project")
        require(stat.S_IMODE(self.state.stat().st_mode) == 0o700, "state directory is not private")
        require(stat.S_IMODE(self.socket.stat().st_mode) == 0o600, "socket is not private")
        code, reply = self.cli(["capability"], peer=True)
        require(code == 3 and reply["error"]["kind"] == "host_unavailable",
                f"default permissions allowed another UID: {code}, {reply}")
        # These two changes affect only the newly created fixture. Reaching the
        # socket isolates getpeereid refusal from the filesystem denial above.
        os.chmod(self.state, 0o711)
        os.chmod(self.socket, 0o666)
        try:
            code, reply = self.cli(["submit", "--id", "forbidden", "--executor", "sh",
                                    "--code", FORBIDDEN_CODE], peer=True)
            require(code == 3 and reply["error"]["kind"] == "refused",
                    f"macOS peer credential check did not refuse: {code}, {reply}")
            result = self.run([self.info["python"], "-c", RAW_PROBE,
                               str(self.socket), FORBIDDEN_CODE], peer=True)
            require(result["returncode"] == 0, f"raw refusal probe failed: {result}")
            raw = json.loads(result["stdout"])
            require(raw["uid"] == self.info["peer_uid"] and raw["greeting"]["state"] == "refused"
                    and raw["reply"] == "", f"raw peer was not refused before request handling: {raw}")
            require(self.success(["list"]) == {"runs": []}, "refused requests created operations")
            require(not (self.project / "dispatches.txt").exists(), "refused request dispatched native code")
        finally:
            os.chmod(self.socket, 0o600)
            os.chmod(self.state, 0o700)
        require(self.success(["capability"])["project_root"] == str(self.project),
                "same UID calls failed after peer refusal")
        self.event("uid_checks_passed", {"host_uid": self.info["host_uid"],
            "peer_uid": self.info["peer_uid"], "operations": 0, "dispatches": 0,
            "filesystem_refusal": True, "getpeereid_refusal": True, "same_uid_followup": True})

    def cleanup(self):
        if self.host is not None:
            if self.host.poll() is None:
                self.host.terminate()
                try:
                    self.host.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.host.kill()
                    self.host.wait(timeout=5)
            self.event("host_closed", {"pid": self.host.pid, "returncode": self.host.returncode})
            self.host.stdout.close()
        if self.stderr is not None:
            self.stderr.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("uid",))
    parser.add_argument("--binary", required=True, help="already built macOS rho-core executable")
    parser.add_argument("--root", required=True, help="fresh direct subdirectory of /private/tmp")
    parser.add_argument("--uid", type=int, help="ordinary host UID; defaults to SUDO_UID or current UID")
    parser.add_argument("--other-uid", type=int, help="peer UID; defaults to the existing nobody account")
    parser.add_argument("--python", default="/usr/bin/python3", help="Python available to both existing UIDs")
    parser.add_argument("--preflight", action="store_true", help="read-only prerequisite report; starts no fixture")
    args = parser.parse_args()
    try:
        info = preflight(args)
    except (AssertionError, OSError, KeyError, ValueError) as error:
        print(json.dumps({"result": "not_run", "reason": str(error)}, ensure_ascii=False))
        return 2
    if args.preflight:
        print(json.dumps({"result": "preflight", **info}, ensure_ascii=False))
        return 0
    if not info["root_available"]:
        print(json.dumps({"result": "not_run", "reason": "run this disposable fixture driver as root",
                          "preflight": info}, ensure_ascii=False))
        return 2
    driver = Driver(info)
    try:
        driver.check()
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
    return 0


if __name__ == "__main__":
    sys.exit(main())
