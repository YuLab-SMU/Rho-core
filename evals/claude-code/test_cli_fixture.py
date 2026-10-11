"""Actual public CLI transport regressions; no model or credentials."""
import tempfile
import unittest
from unittest.mock import Mock, patch
import subprocess
from pathlib import Path
from cli_entry import request_from_args
from cli_fixture import Fixture


class CLITransport(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="rho-cli-test-", dir="/private/tmp" if Path("/private/tmp").exists() else None)
        self.fixture = Fixture(Path(self.tmp.name) / "f")
        self.addCleanup(self.tmp.cleanup)
        self.addCleanup(self.fixture.close)

    def test_capability_is_actual_json(self):
        value = self.fixture.call({"op": "capability"})
        self.assertEqual(value["project_root"], str(self.fixture.root / "project"))
        self.assertEqual(value["jobs"], ["sum", "loud", "hold"])

    def test_capability_contract_preserves_native_text(self):
        request = request_from_args(["capability", "--contract"])
        self.assertTrue(request["contract"])
        value = self.fixture.call(request)
        self.assertIsInstance(value, str)
        self.assertIn("rho-core", value)
        command = self.fixture.commands[-1]
        self.assertEqual(command["argv"][1:3], ["capability", "--contract"])
        self.assertEqual(command["reply"]["value"], value)

    def test_exit3_starts_nothing_then_same_identity_completes(self):
        f = self.fixture
        f.seed("not_sent")
        req = {"op": "submit", "request_id": "sum-100", "job": "sum"}
        error = f.invoke(req)
        self.assertEqual(error["cli_exit"], 3)
        self.assertEqual(error["error"]["kind"], "host_unavailable")
        before = f.snapshot()
        self.assertEqual(before["operations"], [])
        self.assertEqual(before["dispatches"], [])
        accepted = f.call(req)
        f.call({"op": "wait", "request_id": "sum-100", "timeout_ms": 5000})
        existing = f.call(req)
        self.assertEqual(existing["disposition"], "existing")
        self.assertEqual(accepted["run"]["run_id"], existing["run"]["run_id"])
        self.assertEqual(f.snapshot()["dispatches"], ["sum"])

    def test_exit4_real_acceptance_is_found_without_replay(self):
        f = self.fixture
        f.seed("reply_unknown")
        req = {"op": "submit", "request_id": "sum-100", "job": "sum"}
        error = f.invoke(req)
        self.assertEqual(error["cli_exit"], 4)
        self.assertEqual(error["error"]["kind"], "reply_unknown")
        actual = f.fault_evidence[0]["host_reply"]["value"]["run"]["run_id"]
        original = f.call({"op": "lookup", "request_id": "sum-100"})
        self.assertEqual(original["run_id"], actual)
        self.assertEqual(f.call(req)["disposition"], "existing")
        f.call({"op": "wait", "request_id": "sum-100", "timeout_ms": 5000})
        self.assertEqual(f.snapshot()["dispatches"], ["sum"])
        self.assertEqual(f.call({"op": "read_output", "request_id": "sum-100"})["text"], "5050\n")

    def test_raw_cli_evidence_is_not_changed_by_normalization(self):
        f = self.fixture
        f.call({"op": "submit", "request_id": "sum-100", "job": "sum"})
        actual = next(c for c in f.commands if c["argv"][1] == "submit")
        self.assertNotIn("operation", actual["reply"]["value"]["run"])
        self.assertIn("stdout_sha256", actual)

    def test_restart_retains_unknown_and_cancel_confirms_release(self):
        f = self.fixture
        rid = f.seed("crash")
        original = f.call({"op": "lookup", "request_id": "hold-1"})
        self.assertEqual(original["run_id"], rid)
        self.assertFalse(original["terminal"])
        self.assertIn("Protected", original["operation"]["retention"])
        self.assertEqual(f.snapshot()["dispatches"], ["hold"])

    def test_entrypoint_rejects_arbitrary_code_and_executor(self):
        for args in [["submit", "--id", "x", "--executor", "python", "--workdir", "analysis", "--code-file", "jobs/sum.sh"],
                     ["submit", "--id", "x", "--executor", "sh", "--workdir", "analysis", "--code", "echo unsafe"],
                     ["wait", "--id", "x", "--timeout", "6"]]:
            with self.assertRaises(SystemExit) as caught:
                request_from_args(args)
            self.assertEqual(caught.exception.code, 2)


class CLITransportFailures(unittest.TestCase):
    def test_invalid_host_readiness_closes_own_process(self):
        for readiness in ["not-json\n", '{"ready":{"socket":"/wrong/host.sock"}}\n']:
            proc = Mock()
            proc.stdout.readline.return_value = readiness
            proc.poll.return_value = None
            with tempfile.TemporaryDirectory() as tmp, patch("cli_fixture.subprocess.Popen", return_value=proc), \
                    patch("cli_fixture.select.select", return_value=([proc.stdout], [], [])):
                with self.assertRaises((ValueError, RuntimeError)):
                    Fixture(Path(tmp) / "f")
                proc.send_signal.assert_called_once()
                proc.wait.assert_called_once()
                proc.stdout.close.assert_called_once()
                proc.stderr.close.assert_called_once()

    def test_non_json_cli_failure_preserves_raw_receipt(self):
        fixture = Fixture.__new__(Fixture)
        fixture.socket = Path("/unused.sock")
        fixture.commands = []
        result = subprocess.CompletedProcess([], 2, "invalid native reply\n", "native error\n")
        with patch("cli_fixture.subprocess.run", return_value=result):
            with self.assertRaises(RuntimeError):
                fixture._cli(["list"])
        self.assertEqual(len(fixture.commands), 1)
        receipt = fixture.commands[0]
        self.assertEqual(receipt["exit_code"], 2)
        self.assertEqual(receipt["stdout"], result.stdout)
        self.assertEqual(receipt["stderr"], result.stderr)
        self.assertNotIn("reply", receipt)


if __name__ == "__main__":
    unittest.main()
