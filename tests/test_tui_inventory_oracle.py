#!/usr/bin/env python3
"""Pure synthetic negative controls for the native TUI inventory oracle.

These call records model the fixed-output fake spooler. No native host, Bun,
private boundary, or provider process is started by this module.
"""
import hashlib
import json
import unittest

import verify_codex_tui_inventory as inventory


SESSION = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
OTHER_SESSION = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
COMMAND = "printf inventory-tool-call"
OUTPUT = b"fixture-output\n"


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


class InventoryOracleTest(unittest.TestCase):
    def test_valid_fixed_output_evidence(self):
        calls, inputs = valid_evidence()
        inventory.assert_retained_result(calls, inputs)

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
