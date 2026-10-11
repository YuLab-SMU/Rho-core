#!/usr/bin/env python3
"""Focused failure-retention and fixture-cleanup controls for the consumer."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import run


class ConsumerFailures(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ic-", dir="/private/tmp")
        self.root = Path(self.temporary.name)
        self.binary = self.root / "unused-binary"
        self.binary.write_bytes(b"test binary identity only")

    def tearDown(self):
        self.temporary.cleanup()

    def invocation(self, target):
        return ["run.py", "--binary", str(self.binary), "--root", str(target)]

    def test_baseline_timeout_keeps_partial_streams_and_failure(self):
        target = self.root / "timeout"
        timeout = subprocess.TimeoutExpired(["python", "analyze.py"], 120,
                                           output=b"partial-progress\n", stderr=b"partial-error\n")
        with patch.object(sys, "argv", self.invocation(target)), \
             patch.object(run.subprocess, "run", side_effect=timeout), patch.object(run, "Host") as host:
            with self.assertRaises(subprocess.TimeoutExpired):
                run.main()
        host.assert_not_called()
        evidence = target / "evidence"
        self.assertEqual(json.loads((evidence / "failure.json").read_text())["type"], "TimeoutExpired")
        self.assertEqual((evidence / "native.stdout.txt").read_text(), "partial-progress\n")
        self.assertEqual((evidence / "native.stderr.txt").read_text(), "partial-error\n")
        self.assertFalse((evidence / "report.json").exists())

    def test_baseline_oracle_failure_is_retained_before_host_start(self):
        target = self.root / "oracle"
        completed = subprocess.CompletedProcess(["python", "analyze.py"], 0, stdout="result\n", stderr="")
        with patch.object(sys, "argv", self.invocation(target)), \
             patch.object(run.subprocess, "run", return_value=completed), \
             patch.object(run, "verify", side_effect=AssertionError("oracle mismatch")), \
             patch.object(run, "Host") as host:
            with self.assertRaisesRegex(AssertionError, "oracle mismatch"):
                run.main()
        host.assert_not_called()
        self.assertEqual(json.loads((target / "evidence/failure.json").read_text())["message"], "oracle mismatch")
        self.assertFalse((target / "evidence/report.json").exists())

    def test_invalid_host_readiness_stops_its_process(self):
        for name, greeting in [("malformed", "not-json"), ("wrong-socket", '{"ready":{"socket":"wrong"}}')]:
            with self.subTest(name=name):
                fixture = self.root / name
                fixture.mkdir()
                project, evidence = fixture / "project", fixture / "evidence"
                project.mkdir()
                evidence.mkdir()
                binary = fixture / "fake-host"
                binary.write_text("#!" + sys.executable + "\nimport time\nprint(" + repr(greeting) + ", flush=True)\ntime.sleep(60)\n")
                binary.chmod(0o700)
                original_popen = subprocess.Popen
                started = []

                def start(*args, **kwargs):
                    process = original_popen(*args, **kwargs)
                    started.append(process)
                    return process

                with patch.object(run.subprocess, "Popen", side_effect=start) as popen:
                    with self.assertRaises((AssertionError, json.JSONDecodeError)):
                        run.Host(binary, project, fixture / "state", evidence, 120)
                self.assertEqual(popen.call_count, 1)
                self.assertIsNotNone(started[0].poll())
                self.assertEqual((evidence / "host-readiness.txt").read_text(), greeting + "\n")
                self.assertTrue((evidence / "host-shutdown.txt").exists())


if __name__ == "__main__":
    unittest.main()
