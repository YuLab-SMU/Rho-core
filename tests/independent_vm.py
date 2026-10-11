#!/usr/bin/env python3
"""Bounded crash checks in an explicitly selected disposable QEMU guest.

Provision/build the guest first; see independent_vm.md. The Mac-side `run`
starts only its own QEMU process, panics only a marked guest, kills that exact
QEMU child from outside the guest, and reopens the same disk for verification.
No Mac reboot, shared VM shutdown, mounted host project, or global process kill.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import sys
import time
import uuid

import linux_platform as lp


CALLER = "independent-vm"
lp.CALLER = CALLER
OUTPUT = {label: {stream: f"{label} retained {stream}\n".encode()
                  for stream in ("stdout", "stderr")} for label in ("finished", "live")}
WORKER = r"""import json, os, sys, time
from pathlib import Path
label = _fixture_label
marker = {'label': label, 'pid': os.getpid(), 'uid': os.geteuid(),
          'pgid': os.getpgrp(), 'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip()}
fd = os.open('launch-attempts.jsonl', os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
try:
    os.write(fd, (json.dumps(marker) + '\n').encode())
    os.fsync(fd)
finally:
    os.close(fd)
fd = os.open('.', os.O_RDONLY | os.O_DIRECTORY)
try:
    os.fsync(fd)
finally:
    os.close(fd)
print(label + ' retained stdout', flush=True)
print(label + ' retained stderr', file=sys.stderr, flush=True)
if label == 'live':
    while True:
        time.sleep(0.1)
"""


def sha256(path):
    with Path(path).open("rb") as stream:
        digest = hashlib.sha256()
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


class Guest(lp.Driver):
    def submit_label(self, host, label):
        code = f"_fixture_label = {json.dumps(label)}\n" + WORKER
        return self.success(host, ["submit", "--id", label, "--executor", "python",
                                   "--code", code])

    def lookup_label(self, host, label):
        return self.success(host, ["lookup", "--id", label, "--record"])

    def output(self, host, label, view):
        result = {}
        for stream in ("stdout", "stderr"):
            value = self.success(host, ["read", "--id", label, "--stream", stream,
                                       "--offset", "0", "--max-bytes", "4096"])
            lp.require(value["text"].encode() == OUTPUT[label][stream] and not value["lossy"]
                       and value["offset"] == 0 and value["next_offset"] == len(OUTPUT[label][stream])
                       and not value["more_retained"],
                       f"{label}/{stream} retained bytes differ: {value}")
            lp.require(value["info"] == view[stream], f"{label}/{stream} read and lookup metadata disagree")
            result[stream] = value
        return result

    def launches(self, base):
        path = base / "project" / "launch-attempts.jsonl"
        if not path.exists():
            return []
        with path.open("rb") as stream:
            data = stream.read(4097)
        lp.require(len(data) <= 4096, "fixture launch evidence exceeded 4 KiB")
        return [json.loads(line) for line in data.splitlines()]


def guest_prepare(args):
    # Reuse the maintained Linux fixture's subprocess identity, private paths,
    # CLI evidence, kernel-source observation and exact-child cleanup.
    args.command = "prepare-reboot"
    driver = Guest(args)
    try:
        base = driver.fixture("crash")
        host = driver.serve(base)
        snapshot = driver.snapshot("raw")
        lp.require(snapshot["boot_id_scope"] == "Kernel", "guest boot source is not procfs Kernel")
        prepared = {"binary_sha256": driver.evidence["binary_sha256"],
                    "uid": driver.uid, "snapshot": snapshot, "fixtures": {}}
        for label in ("finished", "live"):
            accepted = driver.submit_label(host, label)
            lp.require(accepted["disposition"] == "accepted", f"fixture was not freshly accepted: {accepted}")
            def retained():
                view = driver.lookup_label(host, label)
                if view["unsaved"] is not None:
                    return None
                if any(view[stream]["retained_bytes"] != len(OUTPUT[label][stream])
                       or view[stream]["observed_bytes"] != len(OUTPUT[label][stream])
                       or view[stream]["retention_error"] is not None
                       or view[stream]["truncated"]
                       or (label == "finished" and not view[stream]["eof"])
                       or (label == "live" and view[stream]["eof"])
                       for stream in ("stdout", "stderr")):
                    return None
                if label == "finished" and not view["terminal"]:
                    return None
                return view
            before = lp.eventually(f"{label} durable public output", retained)
            lp.require(accepted["run"]["run_id"] == before["run_id"],
                       "accepted CLI receipt and fault-boundary lookup identify different runs")
            key = before["record"]["key"]
            lp.require(key["caller_id"] == CALLER and key["request_id"] == label,
                       "fault-boundary lookup has a different caller/request identity")
            lp.require(lp.context(before)["boot_id_scope"] == "Kernel", "Core did not observe a Kernel boot source")
            if label == "live":
                lp.require(before["held"] and "Running" in before["status"] and not before["terminal"],
                           f"live fixture was not running at crash boundary: {before}")
                lp.require("SpawnObserved" in lp.known(before["record"], "execution"),
                           "live fixture has no durable spawn observation")
            else:
                lp.require(before["terminal"] and before["retention"] == "Removable",
                           "completed fixture is not durably finished/released")
                lp.require(before["status"].get("Finished", {}).get("exit") == {"Code": 0},
                           "completed fixture has no successful observed native exit")
                lp.require(lp.known(before["record"], "execution").get("ExitObserved", {}).get("exit") == {"Code": 0},
                           "completed fixture has no durable successful ExitObserved fact")
            prepared["fixtures"][label] = {"accepted": accepted, "before": before,
                                          "output": driver.output(host, label, before)}
        launches = driver.launches(base)
        lp.require(sorted(item["label"] for item in launches) == ["finished", "live"],
                   f"unexpected native launch attempts before crash: {launches}")
        prepared["launch_attempts"] = launches
        prepared["ready_at"] = time.time()
        lp.durable_json(driver.directory / "prepared.json", prepared)
        driver.event("prepared_for_external_crash", prepared)
        # Leave these exact fixture children alive. Only the owned VM is then
        # destroyed; no graceful Core shutdown precedes the crash.
        driver.hosts.clear()
        driver.jobs.clear()
        print(json.dumps(prepared))
    except BaseException as error:
        driver.event("failure", {"type": type(error).__name__, "message": str(error)})
        driver.cleanup()
        raise


def guest_verify(args):
    args.command = "verify-reboot"
    driver = Guest(args)
    try:
        prepared = json.loads((driver.directory / "prepared.json").read_text())
        lp.require(prepared["uid"] == driver.uid, "ordinary guest UID changed")
        lp.require(prepared["binary_sha256"] == driver.evidence["binary_sha256"], "Core binary changed")
        snapshot = driver.snapshot("raw")
        lp.require(snapshot["boot_id_scope"] == "Kernel" and snapshot["boot_id"] != prepared["snapshot"]["boot_id"],
                   "independent guest kernel restart was not established")
        lp.require(snapshot["btime"] != prepared["snapshot"]["btime"], "guest kernel boot time did not change")
        base = driver.fixture("crash", existing=True)
        host = driver.serve(base)
        recovered = {"before_kernel": prepared["snapshot"], "after_kernel": snapshot, "fixtures": {}}
        for label in ("finished", "live"):
            prior = prepared["fixtures"][label]
            view = driver.lookup_label(host, label)
            lp.require(view["run_id"] == prior["accepted"]["run"]["run_id"], "durable accepted run_id changed")
            for field in ("operation_id", "key", "request", "script", "accepted_at", "dispatch"):
                lp.require(view["record"][field] == prior["before"]["record"][field],
                           f"immutable accepted identity/material changed: {field}")
            lp.require(view["unsaved"] is None and view["terminal"] and not view["held"]
                       and view["retention"] == "Removable", "recovered run was not terminal/released")
            for stream in ("stdout", "stderr"):
                expected = dict(prior["before"][stream])
                if label == "live":
                    expected["retention_error"] = "the previous holder stopped; later output cannot be retained"
                    lp.require(view["record"]["outputs"][stream]["source"] == "StoreRecovery",
                               "interrupted output has no explicit recovery source")
                else:
                    lp.require(view["record"]["outputs"][stream] == prior["before"]["record"]["outputs"][stream],
                               "finished output facts changed during recovery")
                lp.require(view[stream] == expected,
                           f"saved {label}/{stream} output facts changed across crash: {view[stream]}")
            output = driver.output(host, label, view)
            if label == "live":
                evidence = view["status"].get("Gone", {}).get("evidence", {}).get("BootChanged", {})
                lp.require(evidence == {"dispatched": lp.context(prior["before"])["boot_id"],
                                        "current": snapshot["boot_id"]}, "live run lacks exact BootChanged evidence")
                lp.require(view["record"]["execution"]["Known"]["source"] == "BootSession"
                           and view["record"]["group_released"]["Known"]["source"] == "BootSession",
                           "recovered absence/release has the wrong fact source")
            else:
                lp.require(view["record"]["execution"] == prior["before"]["record"]["execution"],
                           "observed completion was replaced during recovery")
            duplicate = driver.submit_label(host, label)
            lp.require(duplicate["disposition"] == "existing" and duplicate["run"]["run_id"] == view["run_id"],
                       "same request did not resolve to original identity after crash")
            recovered["fixtures"][label] = {"view": view, "output": output, "duplicate": duplicate}
        # Observe the entry marker after both replies and again after bounded
        # host cleanup, rather than sampling only at a duplicate reply boundary.
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline:
            launches = driver.launches(base)
            lp.require(launches == prepared["launch_attempts"], f"recovery replayed native code: {launches}")
            time.sleep(0.05)
        driver.cleanup()
        launches = driver.launches(base)
        lp.require(launches == prepared["launch_attempts"], f"cleanup found an extra native launch: {launches}")
        recovered["launch_attempts"] = launches
        driver.event("crash_recovery_passed", recovered)
        print(json.dumps(recovered))
    except BaseException as error:
        driver.event("failure", {"type": type(error).__name__, "message": str(error)})
        driver.cleanup()
        raise


class VM:
    def __init__(self, args):
        self.args = args
        lp.require(re.fullmatch(r"[A-Za-z0-9._-]{1,64}", args.vm_id), "VM marker must be a bounded safe identity")
        self.trial = uuid.uuid4().hex
        self.root = Path(args.root).resolve(strict=True)
        lp.require(sys.platform == "darwin" and platform.machine() == "arm64", "run uses macOS arm64 HVF")
        lp.require(self.root.parent == Path("/private/tmp") and self.root.name.startswith("rho-"),
                   "VM root must be a dedicated rho-* direct child of /private/tmp")
        for name in ("disk", "seed", "ssh_key"):
            path = Path(getattr(args, name)).resolve(strict=True)
            lp.require(path.is_relative_to(self.root), f"{name} escapes the disposable VM root")
            setattr(args, name, str(path))
        lp.require(1024 <= args.port <= 65535 and 1 <= args.cpus <= 4 and 1024 <= args.memory <= 4096,
                   "bounded VM requires 1–4 CPUs, 1–4 GiB and an unprivileged local port")
        self.evidence = Path(args.evidence).resolve()
        self.evidence.mkdir(parents=True, exist_ok=False)
        self.process = None
        self.current_fixture = None
        self.logs = []
        self.events = {"vm_id": args.vm_id, "started_at": time.time(), "scope":
                       "independent Linux guest panic and guest virtual power cut; Mac host remains powered",
                       "events": []}
        self.event("configuration", {"qemu": args.qemu, "disk": args.disk, "firmware": args.firmware,
                                    "vcpus": args.cpus, "memory_mib": args.memory, "port": args.port,
                                    "binary": args.binary, "guest_vm_marker": args.vm_id,
                                    "trial_id": self.trial})

    def event(self, name, value):
        self.events["events"].append({"name": name, "at": time.time(), "value": value})
        (self.evidence / "run.json").write_text(json.dumps(self.events, indent=2) + "\n")

    def ssh(self, command, timeout=30, check=True, allow_timeout=False):
        argv = ["/usr/bin/ssh", "-i", self.args.ssh_key, "-p", str(self.args.port),
                "-o", "BatchMode=yes", "-o", "ConnectTimeout=3", "-o", "ServerAliveInterval=2",
                "-o", "ServerAliveCountMax=2", "-o", "StrictHostKeyChecking=accept-new",
                "-o", f"UserKnownHostsFile={self.root / 'known_hosts'}", "rho@127.0.0.1", shlex.join(command)]
        try:
            completed = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            def decoded(value):
                return value.decode(errors="replace") if isinstance(value, bytes) else value
            result = {"command": command, "timeout_seconds": timeout, "returncode": None,
                      "stdout": decoded(error.stdout), "stderr": decoded(error.stderr)}
            self.event("guest_command_timeout", result)
            if allow_timeout:
                return result
            raise
        result = {"command": command, "returncode": completed.returncode,
                  "stdout": completed.stdout, "stderr": completed.stderr}
        if check:
            self.event("guest_command", result)
            lp.require(completed.returncode == 0, f"guest command failed: {result}")
        return result

    def boot(self, name):
        lp.require(self.process is None or self.process.poll() is not None, "owned VM still running")
        self.serial = self.evidence / f"{name}.serial.log"
        stderr = (self.evidence / f"{name}.qemu.stderr").open("wb")
        self.logs.append(stderr)
        command = [self.args.qemu, "-machine", "virt", "-accel", "hvf", "-cpu", "host",
                   "-smp", str(self.args.cpus), "-m", str(self.args.memory), "-bios", self.args.firmware,
                   "-drive", f"file={self.args.disk},if=virtio,format=qcow2,cache=none",
                   "-drive", f"file={self.args.seed},if=virtio,format=raw,readonly=on",
                   "-netdev", f"user,id=n0,hostfwd=tcp:127.0.0.1:{self.args.port}-:22",
                   "-device", "virtio-net-pci,netdev=n0", "-display", "none", "-monitor", "none",
                   "-serial", f"file:{self.serial}"]
        self.process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                        stderr=stderr, start_new_session=True)
        self.event("qemu_started", {"pid": self.process.pid, "command": command})
        deadline = time.monotonic() + 120
        last = None
        while time.monotonic() < deadline:
            lp.require(self.process.poll() is None, f"QEMU exited: inspect {stderr.name}")
            last = self.ssh(["cat", "/etc/rho-disposable-marker"], check=False)
            if last["returncode"] == 0:
                lp.require(last["stdout"].strip() == self.args.vm_id, "SSH peer is not this disposable guest")
                self.event("marked_guest_ready", last)
                self.ssh(["uname", "-srmo"])
                return
            time.sleep(1)
        self.event("ssh_readiness_failure", last)
        raise AssertionError("SSH readiness exceeded 120 seconds")

    def kill(self, reason):
        lp.require(self.process is not None and self.process.poll() is None, "owned QEMU is not live")
        pid = self.process.pid
        self.process.kill()
        result = self.process.wait(timeout=10)
        lp.require(result == -9, f"QEMU did not exit from the selected SIGKILL: {result}")
        self.event("external_virtual_power_removed", {"pid": pid, "signal": "SIGKILL", "returncode": result,
                                                       "reason": reason, "guest_shutdown_called": False})

    def guest_phase(self, phase, root):
        result = self.ssh(["sudo", "-n", "/usr/bin/python3", "-B", self.args.driver,
                           phase, "--binary", self.args.binary, "--root", root, "--uid", "1000"], timeout=90)
        value = json.loads(result["stdout"])
        self.event(phase, value)
        return value

    def retain_guest_receipt(self, fault, root, phase):
        path = f"{root}/reboot/{phase}-reboot.json"
        result = self.ssh(["sudo", "-n", "/usr/bin/python3", "-c",
            "from pathlib import Path; import sys; "
            "s=Path(sys.argv[1]).open('rb'); data=s.read(2097153); "
            "assert len(data)<=2097152, 'guest receipt exceeded 2 MiB'; "
            "sys.stdout.buffer.write(data)", path])
        value = json.loads(result["stdout"])
        destination = self.evidence / f"{fault}-{phase}-reboot.json"
        destination.write_text(json.dumps(value, indent=2) + "\n")
        self.event("guest_receipt_retained", {"path": str(destination),
                                              "sha256": sha256(destination)})

    def retain_guest_failure(self):
        if self.current_fixture is None:
            return
        fault, root, phase = self.current_fixture
        try:
            self.retain_guest_receipt(fault, root, phase)
            result = self.ssh(["sudo", "-n", "/usr/bin/python3", "-c",
                "from pathlib import Path; import sys,json; "
                "files=list((Path(sys.argv[1])/'reboot/crash').glob('host-*.stderr')); "
                "assert len(files)<=8; out={}; "
                "exec(\"for p in files:\\n s=p.open('rb'); data=s.read(65537); "
                "assert len(data)<=65536; out[p.name]=data.decode(errors='replace')\"); "
                "print(json.dumps(out))", root])
            value = json.loads(result["stdout"])
            (self.evidence / f"{fault}-{phase}-host-errors.json").write_text(json.dumps(value, indent=2) + "\n")
        except BaseException as error:
            self.event("failure_receipt_unavailable", {"type": type(error).__name__, "message": str(error)})

    def run(self):
        for fault in ("panic", "powercut"):
            # Keep the complete UDS path below Linux's 108-byte sockaddr limit.
            root = f"/var/tmp/rho-vm-{self.trial}-{fault}"
            lp.require(len(os.fsencode(f"{root}/reboot/crash/state/host.sock")) < 108,
                       "selected fixture Unix socket path exceeds the guest limit")
            self.current_fixture = (fault, root, "prepare")
            self.boot(f"{fault}-before")
            self.guest_phase("guest-prepare", root)
            self.retain_guest_receipt(fault, root, "prepare")
            if fault == "panic":
                # No sync, shutdown, reboot, or filesystem unmount precedes c.
                result = self.ssh(["sudo", "-n", "/usr/bin/python3", "-c",
                    "from pathlib import Path; Path('/proc/sys/kernel/panic').write_text('0'); "
                    "Path('/proc/sysrq-trigger').write_text('c')"], timeout=15, check=False, allow_timeout=True)
                self.event("guest_panic_requested", result)
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    text = self.serial.read_text(errors="replace")
                    if "Kernel panic - not syncing" in text and "sysrq triggered crash" in text:
                        self.event("guest_kernel_panic_observed", {"serial": str(self.serial)})
                        break
                    time.sleep(0.1)
                else:
                    raise AssertionError("kernel panic was not observed on guest serial console")
                self.kill("guest kernel had already panicked; reset the disposable virtual hardware")
            else:
                self.kill("external abrupt guest virtual power cut while Core and native fixture remained active")
            self.boot(f"{fault}-after")
            self.current_fixture = (fault, root, "verify")
            self.guest_phase("guest-verify", root)
            self.retain_guest_receipt(fault, root, "verify")
            self.kill("fixture verification completed; discard live guest memory")
        self.event("passed", {"guest_kernel_panic": True, "external_guest_power_cut": True})

    def close(self):
        if self.process is not None and self.process.poll() is None:
            self.kill("bounded fixture cleanup")
        for stream in self.logs:
            stream.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="phase", required=True)
    for name in ("guest-prepare", "guest-verify"):
        part = sub.add_parser(name)
        part.add_argument("--binary", required=True)
        part.add_argument("--root", required=True)
        part.add_argument("--uid", type=int, default=1000)
        part.set_defaults(other_uid=65534)
    run = sub.add_parser("run")
    for name in ("root", "disk", "seed", "ssh-key", "firmware", "binary", "driver", "evidence", "vm-id"):
        run.add_argument("--" + name, required=True)
    run.add_argument("--qemu", default=shutil.which("qemu-system-aarch64"))
    run.add_argument("--port", type=int, default=22611)
    run.add_argument("--cpus", type=int, default=4)
    run.add_argument("--memory", type=int, default=4096)
    args = parser.parse_args()
    if args.phase == "guest-prepare":
        guest_prepare(args)
    elif args.phase == "guest-verify":
        guest_verify(args)
    else:
        vm = VM(args)
        try:
            vm.run()
        except BaseException as error:
            vm.event("failure", {"type": type(error).__name__, "message": str(error)})
            vm.retain_guest_failure()
            raise
        finally:
            vm.close()


if __name__ == "__main__":
    main()
