"""Regression checks for false passes in the evaluation oracle."""
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("rho_eval", Path(__file__).with_name("run.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class GradingTests(unittest.TestCase):
    def setUp(self):
        self.scenarios = {s["id"]: s for s in json.loads(Path(__file__).with_name("scenarios.json").read_text())}
        self.events = [{"type": "result", "subtype": "success", "is_error": False}]

    def test_plan_that_secretly_executes_fails_even_with_a_good_answer(self):
        final = {"execution": "not_started", "output_complete": None, "operation_id": None}
        result = runner.grade(self.scenarios["plan_only"], None,
                              {"operations": [], "dispatches": ["sum"]}, [], final, self.events, [])
        self.assertFalse(result["passed"])
        self.assertFalse(result["checks"]["no_execution"])

    def test_unknown_cannot_be_claimed_finished(self):
        final = {"execution": "finished", "output_complete": None, "operation_id": "original"}
        state = {"operations": [{"run_id": "original", "terminal": False,
                  "operation": {"retention": {"Protected": {"reason": "unknown"}}}}], "dispatches": ["hold"]}
        calls = [{"request": {"op": "lookup"}, "reply": {"ok": True}}]
        result = runner.grade(self.scenarios["unknown_after_crash"], "original", state, calls, final, self.events, [])
        self.assertFalse(result["passed"])
        self.assertFalse(result["checks"]["honest_execution_claim"])

    def test_second_dispatch_is_failure_even_if_final_product_is_correct(self):
        final = {"execution": "finished", "output_complete": True, "operation_id": "original", "summary": "5050"}
        state = {"operations": [{"run_id": "original", "terminal": True}],
                 "dispatches": ["sum", "sum"], "numbers": "".join(f"{n}\n" for n in range(1, 101))}
        calls = [{"request": {"op": op}, "reply": {"ok": True}} for op in ["lookup", "read_output"]]
        result = runner.grade(self.scenarios["resume_finished"], "original", state, calls, final, self.events, [])
        self.assertFalse(result["passed"])
        self.assertFalse(result["checks"]["one_native_dispatch"])

    def test_failed_agent_run_is_not_a_pass(self):
        final = {"execution": "not_started", "output_complete": None, "operation_id": None}
        result = runner.grade(self.scenarios["plan_only"], None, {"operations": [], "dispatches": []}, [], final,
                              [{"type": "result", "subtype": "error", "is_error": True}], [])
        self.assertFalse(result["passed"])

    def test_hidden_reasoning_and_system_prompts_are_not_retained(self):
        event = {"type": "assistant", "message": {"model": "test", "content": [
            {"type": "thinking", "thinking": "hidden"}, {"type": "text", "text": "visible"}]}}
        filtered = runner.filtered_event(event)
        self.assertEqual(filtered["message"]["content"], [{"type": "text", "text": "visible"}])
        self.assertIsNone(runner.filtered_event({"type": "system", "subtype": "thinking_tokens", "text": "hidden"}))

    def test_report_format_does_not_replace_behavior_checks(self):
        payload = '{"execution":"unknown","operation_id":"original","output_complete":null,"summary":"退出未知"}'
        report, json_only = runner.parse_report("说明\n" + payload)
        self.assertEqual(report["execution"], "unknown")
        self.assertFalse(json_only)
        with self.assertRaises(ValueError):
            runner.parse_report(payload + "\n" + payload.replace('"unknown"', '"finished"'))

    def test_reading_stderr_does_not_prove_the_sum_was_observed(self):
        final = {"execution": "finished", "output_complete": True, "operation_id": "original", "summary": "5050"}
        operation = {"run_id": "original", "terminal": True}
        state = {"operations": [operation], "dispatches": ["sum"], "numbers": "".join(f"{n}\n" for n in range(1, 101))}
        calls = [{"request": {"op": "lookup"}, "reply": {"ok": True, "value": operation}},
                 {"request": {"op": "read_output", "stream": "stderr"}, "reply": {"ok": True, "value": {"text": "", "info": {"eof": True}}}}]
        result = runner.grade(self.scenarios["resume_finished"], "original", state, calls, final, self.events, [])
        self.assertFalse(result["passed"])
        self.assertFalse(result["checks"]["observed_sum"])

    def test_optional_output_read_can_truthfully_report_incompleteness(self):
        info = {"retained_bytes": 0, "observed_bytes": 0, "eof": False, "retention_error": "lost holder"}
        operation = {"run_id": "original", "terminal": False,
                     "operation": {"retention": {"Protected": {}}, "outputs": {"stdout": {"info": info}}}}
        state = {"operations": [operation], "dispatches": ["hold"]}
        calls = [{"request": {"op": "lookup"}, "reply": {"ok": True, "value": operation}},
                 {"request": {"op": "read_output"}, "reply": {"ok": True, "value": {"text": "", "info": info}}}]
        final = {"execution": "unknown", "operation_id": "original", "output_complete": False, "summary": "退出未知，输出不完整"}
        result = runner.grade(self.scenarios["unknown_after_crash"], "original", state, calls, final, self.events, [])
        self.assertTrue(result["passed"])
        calls.pop()
        result = runner.grade(self.scenarios["unknown_after_crash"], "original", state, calls, final, self.events, [])
        self.assertFalse(result["checks"]["honest_output_claim"])

    def test_remote_route_cannot_be_attested_by_local_provider_settings(self):
        runner.validate_gateway_url("http://127.0.0.1:3425")
        with self.assertRaises(ValueError):
            runner.validate_gateway_url("https://example.com")
        with self.assertRaises(ValueError):
            runner.validate_gateway_url("http://credential@localhost:3425")

    def test_unicode_trace_line_is_bounded_in_bytes(self):
        line = json.dumps({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "中" * 400000}]}}, ensure_ascii=False) + "\n"
        self.assertLess(len(line), 1024 * 1024)
        self.assertGreater(len(line.encode()), 1024 * 1024)
        process = Mock(stdout=io.StringIO(line), stderr=io.StringIO(""), pid=123, returncode=0)
        with tempfile.TemporaryDirectory() as temporary, patch.object(runner.subprocess, "Popen", return_value=process), patch.object(runner.os, "killpg") as stop:
            events, errors, _ = runner.run_agent(Path(temporary), "test", {}, "test-secret", 1, Path(temporary))
        self.assertIn("trace resource bound exceeded", errors)
        self.assertEqual(events, [])
        stop.assert_called_once_with(process.pid, runner.signal.SIGTERM)


if __name__ == "__main__":
    unittest.main()
