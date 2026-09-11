"""Offline adapter contract tests. Run: python3 integrations/codex/test_mcp.py."""
import hashlib
import json
import os
from pathlib import Path
import select
import socket
import shutil
import subprocess
import tempfile
import time
import threading
import unittest

HERE = Path(__file__).resolve().parent
PINNED_SHA256 = "23dbb0dfd555e3ac720659e5b22a1fe0119c5ebe13102b09231ab47e4fd42c2d"
FAKE = '''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ["FAKE_LOG"], "a") as f:
    f.write(json.dumps({"args": args, "cwd": os.getcwd(), "owner": os.environ.get("AGENT_BASH_OWNER_SESSION_ID"), "custom": os.environ.get("TEST_INHERITED_VALUE")}) + "\\n")
if args[0] == "run": print(json.dumps({"handle": "ab_test", "dispatch_state": "running"}))
elif args[0] == "status": print(("RUNNING" if os.environ.get("FAKE_RUNNING") else "DONE rc=0") + " handle=ab_test\\nfixture-output")
elif args[0] == "mode": print("sync")
elif args[0] == "cancel": print("cancel-accepted")
elif args[0] == "list": print("[]")
elif args[0] != "consume": sys.exit(1)
'''


class AdapterTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="codex-bash-test-")
        self.root = Path(self.tmp.name)
        self.log = self.root / "calls.jsonl"
        fake = self.root / "agent-bash"
        fake.write_text(FAKE)
        fake.chmod(0o755)
        self.env = dict(os.environ)
        for name in ("OULIPOLY_LIVE_SESSION_BIND_SOCKET", "OULIPOLY_LIVE_SESSION_BIND_TOKEN", "OULIPOLY_PARENT_INVOCATION", "AGENT_RUNNER_CODEX_SESSION_FILE", "AGENT_RUNNER_CODEX_SESSION_ID", "AGENT_RUNNER_CODEX_SESSION_BINDING", "AGENT_RUNNER_CODEX_INTERACTIVE", "AGENT_RUNNER_CODEX_REGISTRATION_CWD"):
            self.env.pop(name, None)
        self.env.update(AGENT_BASH_BIN=str(fake), FAKE_LOG=str(self.log), AGENT_RUNNER_CODEX_SESSION_ID="codex-native-test", AGENT_BASH_TOOL_POLL_MS="25", TEST_INHERITED_VALUE="inherited")
        self.process = None

    def tearDown(self):
        if self.process:
            if self.process.poll() is None:
                self.process.stdin.close()
                try: self.process.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait()
            self.process.stdout.close()
            self.process.stderr.close()
        self.tmp.cleanup()

    def start(self):
        self.process = subprocess.Popen([shutil.which("bun"), "--no-install", str(HERE / "agent-bash-mcp.ts")], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, bufsize=1, env=self.env)

    def send(self, method, params=None, request_id=1):
        request = {"jsonrpc": "2.0", "method": method}
        if params is not None: request["params"] = params
        if request_id is not None: request["id"] = request_id
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()

    def receive(self, timeout=4):
        ready, _, _ = select.select([self.process.stdout], [], [], timeout)
        self.assertTrue(ready, "MCP response timed out")
        line = self.process.stdout.readline()
        self.assertTrue(line, "MCP exited before responding")
        return json.loads(line)

    def calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def wait_for_run(self):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if any(call["args"][0] == "run" for call in self.calls()): return
            time.sleep(0.02)
        self.fail("Bash dispatch did not occur")

    def test_exact_pinned_source_and_sole_tool_schema(self):
        self.assertEqual(hashlib.sha256((HERE / "../opencode/tools/bash.ts").read_bytes()).hexdigest(), PINNED_SHA256)
        self.start()
        self.send("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}})
        self.assertEqual(self.receive()["result"]["capabilities"], {"tools": {}})
        self.send("tools/list", request_id=2)
        tools = self.receive()["result"]["tools"]
        self.assertEqual([tool["name"] for tool in tools], ["bash"])
        self.assertEqual(set(tools[0]["inputSchema"]["properties"]), {"command", "handle", "delivery", "workdir"})
        self.assertIn("supervised", tools[0]["inputSchema"]["properties"]["workdir"]["description"])

    def test_sync_command_preserves_workdir_environment_and_owner(self):
        self.start()
        self.send("tools/call", {"name": "bash", "arguments": {"command": "printf probe", "workdir": str(self.root)}})
        response = self.receive()["result"]
        self.assertNotIn("isError", response)
        self.assertIn("fixture-output", response["content"][0]["text"])
        run = self.calls()[0]
        self.assertEqual((run["cwd"], run["owner"], run["custom"]), (str(self.root), "codex-native-test", "inherited"))
        self.assertIn("--cancel-on-owner-exit", run["args"])
        self.assertIn("root", run["args"])
        self.assertTrue(any(call["args"][0] == "consume" for call in self.calls()))

    def test_headless_children_remain_async_and_pin_runner(self):
        self.start()
        self.send("tools/call", {"name": "bash", "arguments": {"command": "agents -m codex-gpt-low task", "delivery": "sync"}})
        self.assertIn("End this headless turn", self.receive()["result"]["content"][0]["text"])
        run = self.calls()[0]["args"]
        self.assertIn("async", run)
        self.assertIn("tree", run)
        self.assertNotIn("--cancel-on-owner-exit", run)
        self.assertIn("/.local/bin/agents", run[-1])

    def test_cancel_notification_cancels_supervised_command(self):
        self.env["FAKE_RUNNING"] = "1"
        self.start()
        self.send("tools/call", {"name": "bash", "arguments": {"command": "workload"}})
        self.wait_for_run()
        self.send("notifications/cancelled", {"requestId": 1}, request_id=None)
        self.assertIn("Cancellation requested", self.receive()["result"]["content"][0]["text"])
        self.assertTrue(any(call["args"][0] == "cancel" for call in self.calls()))

    def test_stdin_close_cancels_active_sync_command(self):
        self.env["FAKE_RUNNING"] = "1"
        self.start()
        self.send("tools/call", {"name": "bash", "arguments": {"command": "workload"}})
        self.wait_for_run()
        self.process.stdin.close()
        self.assertEqual(self.process.wait(timeout=4), 0)
        self.assertTrue(any(call["args"][0] == "cancel" for call in self.calls()))

    def test_missing_session_refuses_execution(self):
        self.env.pop("AGENT_RUNNER_CODEX_SESSION_ID")
        self.start()
        self.send("tools/call", {"name": "bash", "arguments": {"command": "workload"}})
        self.assertTrue(self.receive()["result"]["isError"])
        self.assertEqual(self.calls(), [])

    def test_session_file_waits_for_native_identity(self):
        self.env.pop("AGENT_RUNNER_CODEX_SESSION_ID")
        path = self.root / "session"
        path.write_text("")
        self.env["AGENT_RUNNER_CODEX_SESSION_FILE"] = str(path)
        self.start()
        self.send("tools/call", {"name": "bash", "arguments": {"command": "workload"}})
        time.sleep(0.15)
        self.assertEqual(self.calls(), [])
        path.write_text("native-bound-session\n")
        self.assertNotIn("isError", self.receive()["result"])
        self.assertEqual(self.calls()[0]["owner"], "native-bound-session")

    def test_invalid_arguments_do_not_dispatch(self):
        self.start()
        self.send("tools/call", {"name": "bash", "arguments": {"command": 123}})
        self.assertTrue(self.receive()["result"]["isError"])
        self.assertEqual(self.calls(), [])

    def interactive_metadata(self):
        self.env.pop("AGENT_RUNNER_CODEX_SESSION_ID", None)
        self.env["AGENT_RUNNER_CODEX_SESSION_BINDING"] = "tool_metadata"
        self.env["AGENT_RUNNER_CODEX_INTERACTIVE"] = "1"
        self.env["CODEX_THREAD_ID"] = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
        return {"threadId": "11111111-2222-3333-4444-555555555555"}

    def test_tui_metadata_binds_exact_native_session_before_dispatch(self):
        metadata = self.interactive_metadata()
        listener = socket.socket(socket.AF_UNIX)
        path = self.root / "bind.sock"
        listener.bind(str(path))
        listener.listen(1)
        listener.settimeout(3)
        self.env.update(OULIPOLY_LIVE_SESSION_BIND_SOCKET=str(path), OULIPOLY_LIVE_SESSION_BIND_TOKEN="fixture-token", OULIPOLY_PARENT_INVOCATION=json.dumps({"id": "fixture-invocation"}))
        reports = []
        def acknowledge():
            connection, _ = listener.accept()
            with connection:
                report = json.loads(connection.makefile("rb").readline())
                reports.append(report)
                self.assertEqual(self.calls(), [], "Dispatch must wait for binding acknowledgement")
                connection.sendall((json.dumps({"ok": True, "session_id": report["provider_session_id"]}) + "\n").encode())
        worker = threading.Thread(target=acknowledge)
        worker.start()
        try:
            self.start()
            self.send("tools/call", {"name": "bash", "arguments": {"command": "workload"}, "_meta": metadata})
            self.assertNotIn("isError", self.receive()["result"])
            worker.join(timeout=4)
            self.assertEqual(reports, [{"schema_version": 1, "token": "fixture-token", "invocation_uuid": "fixture-invocation", "provider_session_id": metadata["threadId"]}])
            self.assertEqual(self.calls()[0]["owner"], metadata["threadId"])
        finally:
            listener.close()
            worker.join(timeout=4)

    def test_tui_child_sync_selection_and_async_owner_lease_match_opencode(self):
        metadata = self.interactive_metadata()
        self.start()
        self.send("tools/call", {"name": "bash", "arguments": {"command": "agents -m gpt-luna-low task", "delivery": "sync"}, "_meta": metadata})
        result = self.receive()["result"]["content"][0]["text"]
        self.assertIn("fixture-output", result)
        self.assertNotIn("End this headless turn", result)
        run = self.calls()[0]["args"]
        self.assertIn("sync", run)
        self.assertIn("--cancel-on-owner-exit", run)
        self.send("tools/call", {"name": "bash", "arguments": {"command": "agents -m gpt-luna-low task"}, "_meta": metadata}, request_id=2)
        result = self.receive()["result"]["content"][0]["text"]
        self.assertNotIn("End this headless turn", result)
        run = [call for call in self.calls() if call["args"][0] == "run"][-1]["args"]
        self.assertIn("async", run)
        self.assertIn("--cancel-on-owner-exit", run)

    def test_tui_missing_or_changed_metadata_never_uses_parent_identity(self):
        metadata = self.interactive_metadata()
        self.start()
        for bad in [None, {}, {"threadId": "not-a-uuid"}]:
            self.send("tools/call", {"name": "bash", "arguments": {"command": "workload"}, "_meta": bad})
            self.assertTrue(self.receive()["result"]["isError"])
        self.assertEqual(self.calls(), [])
        self.send("tools/call", {"name": "bash", "arguments": {"command": "workload"}, "_meta": metadata})
        self.assertNotIn("isError", self.receive()["result"])
        count = len(self.calls())
        self.send("tools/call", {"name": "bash", "arguments": {"command": "workload"}, "_meta": {"threadId": self.env["CODEX_THREAD_ID"]}})
        self.assertTrue(self.receive()["result"]["isError"])
        self.assertEqual(len(self.calls()), count)

    def test_tui_resume_metadata_must_match_owned_session(self):
        metadata = self.interactive_metadata()
        self.env["AGENT_RUNNER_CODEX_SESSION_ID"] = self.env["CODEX_THREAD_ID"]
        self.start()
        self.send("tools/call", {"name": "bash", "arguments": {"command": "workload"}, "_meta": metadata})
        self.assertTrue(self.receive()["result"]["isError"])
        self.assertEqual(self.calls(), [])


if __name__ == "__main__":
    unittest.main()
