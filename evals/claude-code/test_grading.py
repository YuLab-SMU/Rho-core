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

    def test_structured_report_is_required_even_after_success(self):
        result = {"type": "result", "subtype": "success", "result":
                  '{"execution":"not_started","operation_id":null,"output_complete":null,"summary":"方案"}'}
        with self.assertRaises(ValueError):
            runner.parse_agent_report(result, [], structured_required=True)
        final = json.loads(result["result"])
        result["structured_output"] = final
        self.assertEqual(runner.parse_agent_report(result, [], structured_required=True), (final, "json_schema"))
        self.assertEqual(runner.filtered_event(result)["structured_output"], final)

    def test_schema_output_does_not_hide_the_recorded_duplicate_failure(self):
        first = {"execution": "unknown", "operation_id": "original", "output_complete": False,
                 "summary": "未观察到退出", "execution_note": None}
        second = {key: value for key, value in first.items() if key != "execution_note"}
        visible = json.dumps(first) + "\nRemove extra key.\n" + json.dumps(second)
        result = {"structured_output": second, "result": visible}
        with self.assertRaises(ValueError):
            runner.parse_agent_report(result, [], structured_required=True)
        result["result"] = ""
        events = [{"type": "assistant", "message": {"content": [{"type": "text", "text": visible}]}}]
        with self.assertRaises(ValueError):
            runner.parse_agent_report(result, events, structured_required=True)

    def test_visible_report_must_agree_with_schema_and_mirror_is_not_duplicate(self):
        final = {"execution": "unknown", "operation_id": "original", "output_complete": None, "summary": "退出未知"}
        visible = json.dumps(final)
        events = [{"type": "assistant", "message": {"content": [{"type": "text", "text": visible}]}}]
        result = {"structured_output": final, "result": visible}
        self.assertEqual(runner.parse_agent_report(result, events, True)[0], final)
        result["structured_output"] = {**final, "execution": "finished"}
        with self.assertRaises(ValueError):
            runner.parse_agent_report(result, events, True)
        result["structured_output"] = {**final, "extra": "unvalidated"}
        with self.assertRaises(ValueError):
            runner.parse_agent_report(result, [], True)

    def test_structured_tool_reports_cannot_hide_multiple_submissions(self):
        first = {"execution": "unknown", "operation_id": "original", "output_complete": None,
                 "summary": "unknown first"}
        second = {**first, "execution": "finished", "summary": "finished revised"}
        def event(identity, value):
            return {"type": "assistant", "message": {"content": [
                {"type": "tool_use", "name": "StructuredOutput", "id": identity, "input": value}]}}
        result = {"structured_output": second, "result": ""}
        events = [event("report-1", first), event("report-2", second)]
        with self.assertRaises(ValueError):
            runner.parse_agent_report(result, events, True)
        events = [event("report-1", second), event("report-2", second)]
        with self.assertRaises(ValueError):
            runner.parse_agent_report(result, events, True)
        events = [event("report-1", first)]
        with self.assertRaises(ValueError):
            runner.parse_agent_report(result, events, True)
        events = [event("report-1", second)] * 2
        self.assertEqual(runner.parse_agent_report(result, events, True), (second, "json_schema"))
        events = [event("report-1", first), event("report-1", second)]
        with self.assertRaises(ValueError):
            runner.parse_agent_report(result, events, True)

    def delivery_evidence(self, case):
        scenario = self.scenarios[case]
        info = {"retained_bytes": 5, "observed_bytes": 5, "eof": True}
        operation = {"run_id": "original", "terminal": True,
                     "operation": {"outputs": {"stdout": {"info": info}}}}
        state = {"operations": [operation], "dispatches": ["sum"],
                 "numbers": "".join(f"{n}\n" for n in range(1, 101))}
        error = scenario["expected_delivery_error"]
        calls = [{"request": {"op": "submit", "request_id": "sum-100", "job": "sum"},
                  "reply": {"ok": False, "cli_exit": error["exit"], "error": {"kind": error["kind"]}}}]
        if case == "delivery_not_sent":
            calls.append({"request": {"op": "submit", "request_id": "sum-100", "job": "sum"},
                          "reply": {"ok": True, "value": {"run": operation}, "cli_exit": 0}})
        calls.extend([{"request": {"op": "lookup", "request_id": "sum-100"}, "reply": {"ok": True, "value": operation}},
                      {"request": {"op": "read_output", "request_id": "sum-100"},
                       "reply": {"ok": True, "value": {"text": "5050\n", "info": info}}}])
        final = {"execution": "finished", "operation_id": "original", "output_complete": True,
                 "summary": f"{error['kind']} exit {error['exit']}; original sum=5050"}
        return scenario, state, calls, final

    def test_expected_real_delivery_errors_do_not_hide_other_failures(self):
        for case in ["delivery_not_sent", "delivery_reply_unknown"]:
            with self.subTest(case=case):
                scenario, state, calls, final = self.delivery_evidence(case)
                result = runner.grade(scenario, None, state, calls, final, self.events, [])
                self.assertTrue(result["passed"], result)
                calls[0]["reply"]["cli_exit"] = 1
                result = runner.grade(scenario, None, state, calls, final, self.events, [])
                self.assertFalse(result["checks"]["observed_delivery_error"])
                scenario, state, calls, final = self.delivery_evidence(case)
                calls.append({"request": {"op": "lookup"}, "reply": {"ok": False, "error": "unexpected"}})
                self.assertFalse(runner.grade(scenario, None, state, calls, final, self.events, [])["passed"])

    def test_reported_delivery_exit_requires_an_explicit_exit_label(self):
        scenario, state, calls, final = self.delivery_evidence("delivery_reply_unknown")
        for summary in ["reply_unknown；原执行 run4；总和5050。", "reply_unknown exit3; run4 sum5050",
                        "reply_unknown 退出码为3；原执行run4；总和5050", "reply_unknown exit code is 3; run4 sum5050"]:
            with self.subTest(summary=summary):
                final["summary"] = summary
                result = runner.grade(scenario, None, state, calls, final, self.events, [])
                self.assertFalse(result["checks"]["reported_delivery_error"])
        for summary in ["reply_unknown exit 4; sum5050", "reply_unknown Exit code: 4; sum5050",
                        "reply_unknown 退出码4；总和5050", 'reply_unknown {"exit": 4}; sum5050',
                        "真实 CLI 退出码为 4，错误 kind 为 reply_unknown；总和5050",
                        "reply_unknown 退出码是4；总和5050", "reply_unknown exit code is 4; sum5050"]:
            with self.subTest(summary=summary):
                final["summary"] = summary
                self.assertTrue(runner.grade(scenario, None, state, calls, final, self.events, [])["passed"])

    def test_reply_unknown_cannot_be_retried_with_new_identity_or_before_lookup(self):
        scenario, state, calls, final = self.delivery_evidence("delivery_reply_unknown")
        retry = {"request": {"op": "submit", "request_id": "new-id", "job": "sum"},
                 "reply": {"ok": True, "value": {"run": state["operations"][0]}}}
        calls.append(retry)
        result = runner.grade(scenario, None, state, calls, final, self.events, [])
        self.assertFalse(result["checks"]["original_request_identity"])
        retry["request"]["request_id"] = "sum-100"
        calls.insert(1, calls.pop())
        result = runner.grade(scenario, None, state, calls, final, self.events, [])
        self.assertFalse(result["checks"]["lookup_before_ambiguous_retry"])

    def test_expected_transport_error_keeps_its_actual_reply(self):
        public_reply = {"ok": False, "cli_exit": 4, "error": {"kind": "reply_unknown", "message": "sent"}}
        fixture = Mock(invoke=Mock(return_value=public_reply))
        self.assertEqual(runner.invoke_fixture(fixture, {"op": "submit"}), public_reply)
        fixture.call.assert_not_called()

    def test_command_policy_keeps_arbitrary_formatter_python_denied(self):
        for transport, command in [("fixture", "./core-call"), ("cli", "./rho-core")]:
            with self.subTest(transport=transport), tempfile.TemporaryDirectory() as temporary:
                process = Mock(stdout=io.StringIO(""), stderr=io.StringIO(""), pid=123, returncode=0)
                with patch.object(runner.subprocess, "Popen", return_value=process) as launch:
                    runner.run_agent(Path(temporary), "test", {}, "test-secret", 1, Path(temporary), transport)
                args = launch.call_args.args[0]
                self.assertEqual(args[args.index("--allowedTools") + 1:args.index("--permission-mode")],
                                 [f"Bash({command} *)", "Read"])
                self.assertEqual(args[args.index("--permission-mode") + 1], "dontAsk")
                self.assertEqual(json.loads(args[args.index("--json-schema") + 1]), runner.REPORT_SCHEMA)

    def test_controller_interrupt_stops_only_its_agent_group_and_closes_drains(self):
        process = Mock(stdout=io.StringIO(""), stderr=io.StringIO(""), pid=321)
        process.wait.side_effect = [KeyboardInterrupt(), 0]
        with tempfile.TemporaryDirectory() as temporary, \
                patch.object(runner.subprocess, "Popen", return_value=process), \
                patch.object(runner.os, "killpg") as stop:
            with self.assertRaises(KeyboardInterrupt):
                runner.run_agent(Path(temporary), "test", {}, "test-secret", 1, Path(temporary))
        stop.assert_called_once_with(321, runner.signal.SIGTERM)
        self.assertTrue(process.stdout.closed)
        self.assertTrue(process.stderr.closed)
        self.assertEqual(process.wait.call_count, 2)

    def test_interrupted_trial_retains_failure_and_partial_facts(self):
        fixture = Mock()
        fixture.seed.return_value = None
        fixture.snapshot.return_value = {"operations": [], "dispatches": []}
        bridge = Mock(calls=[])
        with tempfile.TemporaryDirectory() as temporary, \
                patch.object(runner, "fixture_for_transport", return_value=fixture), \
                patch.object(runner, "Bridge", return_value=bridge), \
                patch.object(runner, "run_agent", side_effect=KeyboardInterrupt):
            output = Path(temporary) / "trial"
            with self.assertRaises(KeyboardInterrupt):
                runner.run_trial(self.scenarios["plan_only"], 1, output, {}, "test-secret", 1)
            report = json.loads((output / "report.json").read_text())
            self.assertFalse(report["passed"])
            self.assertTrue(report["interrupted"])
            self.assertEqual(report["runtime_errors"], ["trial interrupted: KeyboardInterrupt"])
            self.assertTrue((output / "interruption.json").exists())
            self.assertEqual(json.loads((output / "calls.json").read_text()), [])
            self.assertEqual(json.loads((output / "state.json").read_text()), fixture.snapshot.return_value)
        bridge.close.assert_called_once()
        fixture.close.assert_called_once()

    def test_main_preserves_an_existing_exception_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            script = root / "evals/claude-code/run.py"
            files = {script: "test source", script.with_name("scenarios.json"): json.dumps([self.scenarios["plan_only"]]),
                     root / "examples/eval_fixture.rs": "fixture", root / "Cargo.lock": "lock",
                     root / "docs/MANAGED-RUN.md": "contract", root / "target/debug/examples/eval_fixture": "binary"}
            for path, text in files.items():
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            def fail_trial(scenario, trial, output, *args):
                output.mkdir()
                runner.write_json(output / "report.json", {"passed": False, "mode": "agent",
                                  "state_unavailable": True, "runtime_errors": ["trial interrupted: RuntimeError"]})
                raise RuntimeError("snapshot failed")
            with patch.object(runner, "ROOT", root), patch.object(runner, "__file__", str(script)), \
                    patch.object(runner, "agent_environment", return_value=({}, "", {})), \
                    patch.object(runner.subprocess, "check_output", side_effect=["commit", b"", "claude-test"]), \
                    patch.object(runner, "run_trial", side_effect=fail_trial), \
                    patch("sys.argv", ["run.py", "--case", "plan_only", "--trials", "1", "--transport", "fixture"]), \
                    patch("builtins.print"):
                with self.assertRaises(SystemExit) as stopped:
                    runner.main()
            self.assertEqual(stopped.exception.code, 1)
            report_path = next((root / "target/agent-evals").glob("*/plan_only-1/report.json"))
            report = json.loads(report_path.read_text())
            self.assertEqual(report["mode"], "agent")
            self.assertTrue(report["state_unavailable"])
            self.assertEqual(report["runtime_errors"], ["trial interrupted: RuntimeError", "snapshot failed"])
            self.assertTrue(report_path.with_name("scenario.json").exists())

    def test_constructor_failure_retains_trial_identity_and_sanitized_failure(self):
        with tempfile.TemporaryDirectory() as temporary, \
                patch.object(runner, "fixture_for_transport", side_effect=RuntimeError("readiness bad test-secret")), \
                patch.object(runner, "Bridge") as bridge:
            output = Path(temporary) / "trial"
            with self.assertRaisesRegex(RuntimeError, "readiness bad"):
                runner.run_trial(self.scenarios["plan_only"], 2, output, {}, "test-secret", 1)
            report = json.loads((output / "report.json").read_text())
            self.assertFalse(report["passed"])
            self.assertEqual((report["case"], report["trial"], report["mode"]), ("plan_only", 2, "agent"))
            self.assertTrue(report["state_unavailable"])
            self.assertEqual(report["runtime_errors"], ["trial interrupted: RuntimeError: readiness bad [REDACTED]"])
            scenario = json.loads((output / "scenario.json").read_text())
            self.assertEqual((scenario["id"], scenario["trial"]), ("plan_only", 2))
            bridge.assert_not_called()

    def test_failed_snapshot_keeps_raw_cli_command_and_fault_evidence(self):
        fixture = Mock(commands=[{"exit_code": 2, "stdout": "unexpected"}], fault_evidence=[{"mode": "example"}])
        fixture.seed.return_value = None
        fixture.snapshot.side_effect = RuntimeError("snapshot failed")
        with tempfile.TemporaryDirectory() as temporary, \
                patch.object(runner, "fixture_for_transport", return_value=fixture):
            output = Path(temporary) / "trial"
            with self.assertRaisesRegex(RuntimeError, "snapshot failed"):
                runner.run_trial(self.scenarios["plan_only"], 1, output, {}, "test-secret", 1)
            raw = json.loads((output / "cli-evidence.json").read_text())
            self.assertEqual(raw, {"cli_commands": fixture.commands, "delivery_faults": fixture.fault_evidence})
            self.assertTrue(json.loads((output / "report.json").read_text())["state_unavailable"])
        fixture.close.assert_called_once()

    def test_regrade_keeps_exception_and_missing_evidence_in_the_denominator(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            runner.write_json(directory / "manifest.json", {"mode": "agent"})
            runner.write_json(directory / "summary.json", {"passed": 1, "total": 2, "results": [
                {"case": "plan_only", "trial": 1, "passed": True},
                {"case": "plan_only", "trial": 2, "passed": False}]})
            success, failed = directory / "plan_only-1", directory / "plan_only-2"
            success.mkdir(); failed.mkdir()
            runner.write_json(success / "scenario.json", {**self.scenarios["plan_only"], "trial": 1, "initial_id": None})
            final = {"execution": "not_started", "operation_id": None, "output_complete": None, "summary": "仅方案"}
            event = {**self.events[0], "structured_output": final, "result": ""}
            (success / "trace.jsonl").write_text(json.dumps(event) + "\n")
            runner.write_json(success / "state.json", {"operations": [], "dispatches": []})
            runner.write_json(success / "calls.json", [])
            runner.write_json(success / "report.json", {"passed": True, "mode": "agent", "report_contract": "structured",
                              "checks": {"input_unchanged": True}, "runtime_errors": []})
            # This is the historical main() exception shape: no mode and no
            # complete trial evidence. Its original failure must remain counted.
            runner.write_json(failed / "report.json", {"passed": False, "runtime_errors": ["snapshot failed"]})
            original = (failed / "report.json").read_bytes()
            with patch("builtins.print"):
                self.assertFalse(runner.regrade(directory))
            result = json.loads(next(directory.glob("regrade-*.json")).read_text())
            self.assertEqual((result["passed"], result["total"]), (1, 2))
            unavailable = next(item for item in result["results"] if item["trial"] == 2)
            self.assertFalse(unavailable["passed"])
            self.assertTrue(unavailable["evidence_unavailable"])
            self.assertIn("snapshot failed", unavailable["runtime_errors"])
            self.assertEqual((failed / "report.json").read_bytes(), original)

    def test_absent_other_identity_is_a_negative_read_but_not_original_evidence(self):
        operation = {"request_id": "hold-1", "run_id": "original", "terminal": False,
                     "operation": {"retention": {"Protected": {}}}}
        state = {"operations": [operation], "dispatches": ["hold"]}
        final = {"execution": "unknown", "operation_id": "original", "output_complete": None,
                 "summary": "hold-1 已受理但退出未知；sum-100 当前找不到，不能据此判断其历史受理"}
        negative = {"request": {"op": "lookup", "request_id": "sum-100"},
                    "reply": {"ok": False, "cli_exit": 1, "error": {"kind": "not_found"}}}
        original = {"request": {"op": "lookup", "request_id": "hold-1"},
                    "reply": {"ok": True, "value": operation}}
        result = runner.grade(self.scenarios["unknown_after_crash"], "original", state,
                              [negative, original], final, self.events, [])
        self.assertTrue(result["passed"], result)
        result = runner.grade(self.scenarios["unknown_after_crash"], "original", state,
                              [negative], final, self.events, [])
        self.assertFalse(result["checks"]["observed_original"])
        self.assertFalse(result["checks"]["observed_identity"])
        negative["request"]["request_id"] = "hold-1"
        result = runner.grade(self.scenarios["unknown_after_crash"], "original", state,
                              [negative], final, self.events, [])
        self.assertFalse(result["checks"]["fixture_calls_succeeded"])
        self.assertFalse(result["checks"]["observed_original"])

    def test_negative_lookup_exception_does_not_forgive_writes_or_scope_errors(self):
        final = {"execution": "not_started", "operation_id": None, "output_complete": None, "summary": "仅方案"}
        state = {"operations": [], "dispatches": []}
        for request, reply in [
            ({"op": "submit", "request_id": "absent"}, {"ok": False, "cli_exit": 1, "error": {"kind": "not_found"}}),
            ({"op": "cancel", "request_id": "absent"}, {"ok": False, "cli_exit": 1, "error": {"kind": "not_found"}}),
            ({"op": "lookup", "request_id": "../escape"}, {"ok": False, "cli_exit": 1, "error": {"kind": "not_found"}}),
            ({"op": "lookup", "request_id": "absent", "caller": "other"}, {"ok": False, "cli_exit": 1, "error": {"kind": "not_found"}}),
            ({"op": "lookup", "request_id": "absent"}, {"ok": False, "cli_exit": 3, "error": {"kind": "not_found"}}),
            ({"op": "lookup", "request_id": "absent"}, {"ok": False, "cli_exit": 1, "error": {"kind": "storage"}}),
        ]:
            with self.subTest(request=request, reply=reply):
                result = runner.grade(self.scenarios["plan_only"], None, state,
                                      [{"request": request, "reply": reply}], final, self.events, [])
                self.assertFalse(result["checks"]["fixture_calls_succeeded"])


if __name__ == "__main__":
    unittest.main()
