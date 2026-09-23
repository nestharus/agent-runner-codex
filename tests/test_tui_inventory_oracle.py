#!/usr/bin/env python3
"""Pure synthetic negative controls for the native TUI inventory oracle.

These call records model the fixed-output fake spooler. No native host, Bun,
private boundary, or provider process is started by this module.
"""
import copy
import hashlib
import json
import unittest

import verify_codex_tui_inventory as inventory


SESSION = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
OTHER_SESSION = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
COMMAND = "printf inventory-tool-call"
OUTPUT = b"fixture-output\n"
PROMPT = "Synthetic inventory system instructions."


def valid_evidence():
    snapshot = {"version": 1, "handle": "ab_test", "created_at_unix_ms": 1,
                "bytes": len(OUTPUT),
                "sha256": hashlib.sha256(OUTPUT).hexdigest(), "encoding": "hex"}
    calls = [{"owner": SESSION, "args": args} for args in [
        ["run", "--cancel-on-owner-exit", "--owner-pid", "1234",
         "--completion-scope", "root", "--delivery", "sync", "--", "bash", "-lc", COMMAND],
        ["status", "--tail-bytes", "0", "--observe-only", "ab_test"],
        ["snapshot", "ab_test"],
        ["accept-output", "ab_test", "--snapshot", json.dumps(snapshot)],
        ["status", "--tail-bytes", "0", "ab_test"],
    ]]
    output = ("DONE rc=0 handle=ab_test\n"
              "local receipt: durable bounded snapshot; remote ACK: unconfirmed; physical drain: unconfirmed\n"
              "progression: requested (not remote settlement evidence)\n"
              f"snapshot: {json.dumps(snapshot)}\n"
              "output representation: utf8; acquired bounded bytes only, not an atomic historical log; later append data unknown\n"
              "--- output ---\nfixture-output\n")
    inputs = [{"type": "function_call_output", "call_id": "inventory-bash-call", "output": output}]
    return calls, inputs


def valid_request_pair():
    calls, outputs = valid_evidence()
    first = {"model": "gpt-6-luna", "reasoning": {"effort": "low"}, "input": [
        {"type": "additional_tools", "tools": [
            {"type": "namespace", "name": "mcp__agent_bash", "tools": [
                {"type": "function", "name": "bash"}]},
        ]},
        {"role": "developer", "content": [{"type": "input_text", "text": PROMPT}]},
        {"role": "developer", "content": [{"type": "input_text", "text": "TUI-DEVELOPER-SENTINEL"}]},
    ]}
    second = copy.deepcopy(first)
    second["input"].extend([
        {"type": "function_call", "call_id": "inventory-bash-call"}, *outputs,
    ])
    return calls, [first, second]


class InventoryOracleTest(unittest.TestCase):
    def assert_valid_pair(self, calls, requests):
        return inventory.assert_inventory_request_pair(requests, calls, "gpt-6-luna", "low", PROMPT)

    def test_prompt_report_preserves_exact_item_and_reports_other_developer_text(self):
        prompt = "Synthetic inventory system instructions."
        body = {"instructions": {"unrelated": "native field"}, "input": [
            {"role": "developer", "tools": []},
            {"role": "developer", "content": [{"type": "input_text", "text": prompt}]},
            {"role": "developer", "content": [{"type": "input_text", "text": "TUI-DEVELOPER-SENTINEL and native policy"}]},
        ]}
        report, texts = inventory.prompt_evidence(body, prompt)
        self.assertEqual(report, {
            "configured_prompt_match": "exact_developer_item",
            "additional_developer_text_observed": True,
        })
        self.assertEqual(texts, [prompt, "TUI-DEVELOPER-SENTINEL and native policy"])
        only_prompt, _ = inventory.prompt_evidence({"input": [body["input"][1]]}, prompt)
        self.assertFalse(only_prompt["additional_developer_text_observed"])

    def test_prompt_report_keeps_top_level_fallback_and_rejects_partial_match(self):
        prompt = "Synthetic inventory system instructions."
        body = {"instructions": prompt + "\n", "input": [
            {"role": "developer", "content": [{"type": "input_text", "text": "native policy"}]},
        ]}
        report, _ = inventory.prompt_evidence(body, prompt)
        self.assertEqual(report, {
            "configured_prompt_match": "top_level_instructions_after_trim",
            "additional_developer_text_observed": True,
        })
        body = {"input": [{"role": "developer", "content": [
            {"type": "input_text", "text": prompt + " with extra text"}]}]}
        with self.assertRaisesRegex(AssertionError, "System prompt differs"):
            inventory.prompt_evidence(body, prompt)

    def test_valid_fixed_output_evidence(self):
        calls, inputs = valid_evidence()
        inventory.assert_retained_result(calls, inputs)

    def test_valid_request_pair(self):
        calls, requests = valid_request_pair()
        requests[1]["prompt_cache_key"] = "incidental second-request field"
        names, report = self.assert_valid_pair(calls, requests)
        self.assertEqual(names, ["mcp__agent_bash.bash"])
        self.assertEqual(report["configured_prompt_match"], "exact_developer_item")

    def test_extra_tool_result_is_rejected(self):
        for index, call_id in ((0, "earlier-call"), (1, "unrelated-call"),
                               (1, "inventory-bash-call")):
            with self.subTest(request=index, call_id=call_id):
                calls, requests = valid_request_pair()
                requests[index]["input"].append({"type": "function_call_output",
                                                 "call_id": call_id, "output": "unrelated"})
                with self.assertRaises(AssertionError):
                    self.assert_valid_pair(calls, requests)

    def test_second_request_policy_drift_is_rejected(self):
        for change in ("model", "effort", "extra_tool", "missing_bash", "prompt", "account"):
            with self.subTest(change=change):
                calls, requests = valid_request_pair()
                second = requests[1]
                if change == "model":
                    second["model"] = "gpt-6-sol"
                elif change == "effort":
                    second["reasoning"]["effort"] = "high"
                elif change == "extra_tool":
                    second["input"][0]["tools"].append({"type": "function", "name": "exec_command"})
                elif change == "missing_bash":
                    second["input"][0]["tools"].clear()
                elif change == "prompt":
                    second["input"][1]["content"][0]["text"] = "different instructions"
                else:
                    second["input"][2]["content"][0]["text"] = "different account text"
                with self.assertRaises(AssertionError):
                    self.assert_valid_pair(calls, requests)

    def test_snapshot_before_run_is_rejected(self):
        calls, inputs = valid_evidence()
        calls[0], calls[2] = calls[2], calls[0]
        with self.assertRaises(AssertionError):
            inventory.assert_retained_result(calls, inputs)

    def test_unrelated_run_command_is_rejected(self):
        calls, inputs = valid_evidence()
        calls[0]["args"][-1] = "printf unrelated-command"
        with self.assertRaises(AssertionError):
            inventory.assert_retained_result(calls, inputs)

    def test_other_handle_observation_is_rejected(self):
        calls, inputs = valid_evidence()
        calls[1]["args"][-1] = "ab_other"
        with self.assertRaises(AssertionError):
            inventory.assert_retained_result(calls, inputs)

    def test_later_owner_drift_is_rejected(self):
        for index in (1, 2, 3, 4):
            with self.subTest(index=index):
                calls, inputs = valid_evidence()
                calls[index]["owner"] = OTHER_SESSION
                # The native path binds the first call to the reported session.
                self.assertEqual(calls[0]["owner"], SESSION)
                with self.assertRaises(AssertionError):
                    inventory.assert_retained_result(calls, inputs)

    def test_native_input_text_wrapper_is_exact(self):
        calls, inputs = valid_evidence()
        body = inputs[0]["output"]
        inputs[0]["output"] = [
            {"type": "input_text", "text": "Wall time: 0.3372 seconds\nOutput:"},
            {"type": "input_text", "text": body},
        ]
        inventory.assert_retained_result(calls, inputs)
        for changed in (
            [{"type": "input_text", "text": body}],
            [{"type": "input_text", "text": "Error:\nOutput:"}, inputs[0]["output"][1]],
            [*inputs[0]["output"], {"type": "input_text", "text": body}],
            [inputs[0]["output"][0], {"type": "input_text", "text": body + "extra"}],
        ):
            with self.subTest(changed=changed):
                with self.assertRaises((AssertionError, ValueError)):
                    inventory.assert_retained_result(calls, [{"type": "function_call_output",
                                                             "call_id": "inventory-bash-call",
                                                             "output": changed}])


if __name__ == "__main__":
    unittest.main()
