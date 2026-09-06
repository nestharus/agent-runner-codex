"""Offline request-control lifecycle/security tests; no installed runtime changes."""
import json
import os
from pathlib import Path
import select
import shutil
import socket
import sqlite3
import subprocess
import tempfile
import time
import unittest

HERE = Path(os.environ.get("MCP_TEST_SOURCE", Path(__file__).resolve().parent))
UUID = "11111111-2222-3333-4444-555555555555"
DIR = "AGENT_RUNNER_CODEX_REQUEST_CONTROL_DIR"
BINDING = "AGENT_RUNNER_CODEX_REQUEST_CONTROL_BINDING"
FAKE = r'''#!/usr/bin/env python3
import json, os, sys, time
from pathlib import Path
root = Path(os.environ['FIXTURE_ROOT'])
args = sys.argv[1:]
op = args[0]
handle = 'ab_' + args[-1] if op == 'run' else args[-1] if len(args) > 1 else ''
with open(root / 'calls', 'a') as f:
    f.write(json.dumps({'op':op, 'handle':handle, 'control_inherited':any(k.startswith('AGENT_RUNNER_CODEX_REQUEST_CONTROL') for k in os.environ)}) + '\n')
if op == 'run':
    while (root / ('hold-' + handle)).exists(): time.sleep(.01)
    print(json.dumps({'handle':handle, 'dispatch_state':'running'}))
elif op == 'status':
    print(('DONE rc=0' if (root / ('done-' + handle)).exists() else 'RUNNING') + ' handle=' + handle + '\nprivate-reply-sentinel')
elif op == 'mode': print('sync')
elif op == 'cancel': print('cancel-accepted')
elif op != 'consume': sys.exit(1)
'''


def incarnation(pid):
    ticks = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]
    boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    return f"linux:{boot}:{ticks}"


class Frames:
    def __init__(self, source):
        self.source = source
        self.buffer = b""

    def receive(self, timeout=5):
        deadline = time.monotonic() + timeout
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.source], [], [], remaining)[0]:
                raise AssertionError("frame deadline")
            chunk = os.read(self.source.fileno(), 16384)
            if not chunk:
                raise EOFError("endpoint closed")
            self.buffer += chunk
        line, self.buffer = self.buffer.split(b"\n", 1)
        return json.loads(line)

    def event(self, name, request_id=None):
        for _ in range(30):
            event = self.receive()
            if event.get("event") == name and (request_id is None or event.get("id") == request_id):
                return event
        raise AssertionError("event missing")


class ControlTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="mcp-rc-")
        self.root = Path(self.tmp.name)
        self.slot = self.root / "launch"
        self.children = []
        self.sockets = []
        fake = self.root / "agent-bash"
        fake.write_text(FAKE)
        fake.chmod(0o700)
        self.env = dict(os.environ)
        for key in list(self.env):
            if key.startswith("AGENT_RUNNER_CODEX_") or key.startswith("OULIPOLY_LIVE_SESSION_") or key == "OULIPOLY_PARENT_INVOCATION":
                self.env.pop(key)
        self.binding = dict(version=1, provider_pid=os.getpid(), provider_incarnation=incarnation(os.getpid()), request_id="outer-request-one", provider_instance_id="codex-fixture", parent=dict(id=UUID, source="fixture"), identity_database=str(self.root / "pid-identity.db"))
        self.env.update({DIR: str(self.slot), BINDING: json.dumps(self.binding), "AGENT_BASH_BIN": str(fake), "AGENT_BASH_TOOL_POLL_MS": "10", "FIXTURE_ROOT": str(self.root), "AGENT_RUNNER_CODEX_SESSION_ID": UUID, "OULIPOLY_PARENT_INVOCATION": json.dumps(self.binding["parent"])})
        with sqlite3.connect(self.binding["identity_database"]) as db:
            db.execute("CREATE TABLE pid_identity (os_pid INTEGER, os_boot_id TEXT, os_pid_starttime_ticks INTEGER, invocation_uuid TEXT, provider_name TEXT)")
            _, boot, ticks = self.binding["provider_incarnation"].split(":")
            db.execute("INSERT INTO pid_identity VALUES (?,?,?,?,?)", (os.getpid(), boot, int(ticks), UUID, "codex-fixture"))

    def tearDown(self):
        for s in self.sockets:
            s.close()
        for p in reversed(self.children):
            if p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=4)
                except subprocess.TimeoutExpired:
                    p.kill()
                    p.wait(timeout=2)
            for stream in [p.stdin, p.stdout, p.stderr]:
                if stream:
                    stream.close()
        self.tmp.cleanup()

    def spawn(self, args, env=None):
        p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env or self.env)
        self.children.append(p)
        return p

    def start(self, env=None):
        self.bridge = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "agent-bash-mcp.ts")], env)
        self.rpc = Frames(self.bridge.stdout)
        deadline = time.monotonic() + 5
        while not (self.slot / "control.sock").exists() and time.monotonic() < deadline:
            if self.bridge.poll() is not None:
                self.fail("bridge startup failed: " + self.bridge.stderr.read().decode())
            time.sleep(.01)
        self.assertTrue((self.slot / "control.sock").exists(), "bridge did not expose request control")
        self.descriptor = json.loads((self.slot / "control.json").read_text())

    def client(self, raw=False):
        s = socket.socket(socket.AF_UNIX)
        s.connect(str(self.slot / "control.sock"))
        self.sockets.append(s)
        frames = Frames(s)
        challenge = frames.receive()["challenge"]
        credential = dict(token=self.descriptor["token"], generation=self.descriptor["generation"], request_id=self.binding["request_id"], invocation_id=UUID, challenge=challenge, seq=0)
        self.control_send(s, credential, op="observe", raw=raw)
        attached = frames.event("attached")
        self.assertNotIn("identity_database", attached["binding"])
        return s, frames, credential, attached

    @staticmethod
    def control_send(s, credential, **fields):
        credential["seq"] += 1
        s.sendall((json.dumps(dict(credential, **fields)) + "\n").encode())

    def request(self, request_id, command="first"):
        self.bridge.stdin.write((json.dumps(dict(jsonrpc="2.0", id=request_id, method="tools/call", params=dict(name="bash", arguments=dict(command=command)))) + "\n").encode())
        self.bridge.stdin.flush()

    def cancel(self, client, request):
        s, frames, credential, _ = client
        self.control_send(s, credential, op="cancel", id=request["id"], request_generation=request["request_generation"])
        return frames.event("cancel")["accepted"]

    def calls(self):
        path = self.root / "calls"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def wait_call(self, op, handle):
        deadline = time.monotonic() + 4
        while time.monotonic() < deadline:
            if any(c["op"] == op and c["handle"] == handle for c in self.calls()):
                return
            time.sleep(.01)
        self.fail("downstream call missing")

    def test_concurrent_exact_ids_selected_abort_response_and_retirement(self):
        self.start()
        client = self.client(raw=True)
        observer = client[1]
        self.request("7", "first")
        one = observer.event("request", "7")
        self.assertEqual(one["payload"]["id"], "7")
        self.request(7, "second")
        two = observer.event("request", 7)
        self.assertIsInstance(two["id"], int)
        self.wait_call("status", "ab_first")
        self.wait_call("status", "ab_second")
        self.assertTrue(self.cancel(client, one))
        response = observer.event("response", "7")["payload"]
        self.assertIn("Cancellation requested", response["result"]["content"][0]["text"])
        self.assertEqual(self.rpc.receive(), response)
        observer.event("retired", "7")
        self.assertFalse(self.cancel(client, one))
        calls = self.calls()
        self.assertEqual([c["handle"] for c in calls if c["op"] == "cancel"], ["ab_first"])
        self.assertFalse(any(c["control_inherited"] for c in calls))
        snapshot = self.client()[3]["active"]
        self.assertEqual([(r["id"], r["aborted"]) for r in snapshot], [(7, False)])
        self.assertNotIn("payload", snapshot[0])
        raw_snapshot = self.client(raw=True)[3]["active"]
        self.assertEqual(raw_snapshot[0]["payload"]["params"]["arguments"], {"command":"second"})
        (self.root / "done-ab_second").touch()
        self.assertEqual(self.rpc.receive()["id"], 7)
        observer.event("retired", 7)
        self.assertEqual(self.client()[3]["active"], [])
        self.assertEqual(len([c for c in self.calls() if c["op"] == "cancel"]), 1)

    def test_before_session_and_during_registration_cancellation(self):
        self.env.pop("AGENT_RUNNER_CODEX_SESSION_ID")
        session = self.root / "session"
        self.env["AGENT_RUNNER_CODEX_SESSION_FILE"] = str(session)
        self.start()
        client = self.client()
        missing = dict(id="missing", request_generation="0" * 64)
        self.assertFalse(self.cancel(client, missing))
        self.request("before")
        request = client[1].event("request", "before")
        self.assertTrue(self.cancel(client, request))
        self.assertEqual(self.calls(), [])
        session.write_text(UUID)
        self.assertIn("before dispatch", self.rpc.receive()["result"]["content"][0]["text"])
        client[1].event("retired", "before")
        hold = self.root / "hold-ab_first"
        hold.touch()
        self.request("registering")
        request = client[1].event("request", "registering")
        self.wait_call("run", "ab_first")
        self.assertTrue(self.cancel(client, request))
        self.assertFalse(self.cancel(client, request))
        hold.unlink()
        self.assertIn("Cancellation requested", self.rpc.receive()["result"]["content"][0]["text"])
        client[1].event("retired", "registering")
        self.assertEqual([c["handle"] for c in self.calls() if c["op"] == "cancel"], ["ab_first"])

    def test_wrong_auth_identity_replay_request_and_reused_id(self):
        self.start()
        observer = self.client()
        self.request(1)
        request = observer[1].event("request", 1)
        for key, value in [("token", "0" * 64), ("generation", "0" * 64), ("request_id", "other"), ("invocation_id", "other"), ("challenge", "0" * 64), ("seq", 1)]:
            client = self.client()
            s, frames, credential, _ = client
            self.control_send(s, credential, op="cancel", id=1, request_generation=request["request_generation"], **{key:value})
            with self.assertRaises(EOFError):
                frames.receive()
        self.assertFalse(self.cancel(observer, dict(request, id="1")))
        self.assertFalse(self.cancel(observer, dict(request, request_generation="0" * 64)))
        self.assertTrue(self.cancel(observer, request))
        self.rpc.receive()
        observer[1].event("retired", 1)
        self.request(1, "second")
        current = observer[1].event("request", 1)
        self.assertNotEqual(current["request_generation"], request["request_generation"])
        self.assertFalse(self.cancel(observer, request))
        self.assertTrue(self.cancel(observer, current))
        self.rpc.receive()

    def test_two_live_invocations_cannot_use_each_others_capabilities(self):
        self.start()
        first_client = self.client()
        self.request("same-id")
        first = first_client[1].event("request", "same-id")
        other_slot = self.root / "other"
        other_env = dict(self.env)
        other_env[DIR] = str(other_slot)
        claim = dict(self.binding, request_id="outer-request-two", parent=dict(id="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", source="fixture"))
        other_env[BINDING] = json.dumps(claim)
        owner = self.root / "fixture-owner.py"
        owner.write_text('''import json, os, signal, sqlite3, subprocess, sys
from pathlib import Path
binding = json.loads(os.environ["AGENT_RUNNER_CODEX_REQUEST_CONTROL_BINDING"])
pid = os.getpid()
boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
ticks = Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()[19]
binding.update(provider_pid=pid, provider_incarnation=f'linux:{boot}:{ticks}')
with sqlite3.connect(binding['identity_database']) as db:
    db.execute('INSERT INTO pid_identity VALUES (?,?,?,?,?)', (pid,boot,int(ticks),binding['parent']['id'],binding['provider_instance_id']))
os.environ['AGENT_RUNNER_CODEX_REQUEST_CONTROL_BINDING'] = json.dumps(binding)
os.environ['OULIPOLY_PARENT_INVOCATION'] = json.dumps(binding['parent'])
child = subprocess.Popen(''' + repr([shutil.which("bun"), "--no-install", str(HERE / "agent-bash-mcp.ts")]) + ''')
signal.signal(signal.SIGTERM, lambda *_: child.terminate())
sys.exit(child.wait())
''')
        other = self.spawn(["python3", str(owner)], other_env)
        deadline = time.monotonic() + 5
        while not (other_slot / "control.sock").exists() and time.monotonic() < deadline:
            time.sleep(.01)
        descriptor = json.loads((other_slot / "control.json").read_text())
        self.assertNotEqual(descriptor["binding"]["provider_pid"], self.descriptor["binding"]["provider_pid"])
        self.assertNotEqual(descriptor["binding"]["parent"]["id"], UUID)
        s = socket.socket(socket.AF_UNIX)
        self.sockets.append(s)
        s.connect(str(other_slot / "control.sock"))
        frames = Frames(s)
        credential = dict(token=descriptor["token"], generation=descriptor["generation"], request_id=claim["request_id"], invocation_id=claim["parent"]["id"], challenge=frames.receive()["challenge"], seq=0)
        self.control_send(s, credential, op="observe", raw=False)
        frames.event("attached")
        other.stdin.write(b'{"jsonrpc":"2.0","id":"same-id","method":"tools/call","params":{"name":"bash","arguments":{"command":"second"}}}\n')
        other.stdin.flush()
        second = frames.event("request", "same-id")
        attacker = socket.socket(socket.AF_UNIX)
        self.sockets.append(attacker)
        attacker.connect(str(other_slot / "control.sock"))
        attacker_frames = Frames(attacker)
        replay = dict(first_client[2], challenge=attacker_frames.receive()["challenge"], seq=0)
        self.control_send(attacker, replay, op="cancel", id=second["id"], request_generation=second["request_generation"])
        with self.assertRaises(EOFError):
            attacker_frames.receive()
        self.assertTrue(self.cancel(first_client, first))
        self.rpc.receive()
        self.control_send(s, credential, op="cancel", id=second["id"], request_generation=second["request_generation"])
        self.assertTrue(frames.event("cancel")["accepted"], "other invocation must still be active")
        self.assertEqual(Frames(other.stdout).receive()["id"], "same-id")
        other.stdin.close()
        self.assertEqual(other.wait(timeout=4), 0)

    def test_private_default_metadata_no_payload_persistence_and_unused_no_residue(self):
        database_before = Path(self.binding["identity_database"]).read_bytes()
        self.start()
        client = self.client()
        self.assertEqual(self.slot.stat().st_mode & 0o777, 0o700)
        for name in ["control.json", "control.sock"]:
            self.assertEqual((self.slot / name).stat().st_mode & 0o777, 0o600)
        self.request("private", "secret-command-sentinel")
        request = client[1].event("request", "private")
        self.assertNotIn("payload", request)
        self.assertNotIn("secret-command-sentinel", (self.slot / "control.json").read_text())
        self.assertEqual(sorted(p.name for p in self.slot.iterdir()), ["control.json", "control.sock"])
        self.cancel(client, request)
        self.rpc.receive()
        response = client[1].event("response", "private")
        self.assertNotIn("payload", response)
        self.bridge.stdin.close()
        self.assertEqual(self.bridge.wait(timeout=4), 0)
        self.assertFalse(self.slot.exists())
        env = dict(self.env)
        env.pop(DIR)
        env.pop(BINDING)
        before = sorted(p.name for p in self.root.iterdir())
        p = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "agent-bash-mcp.ts")], env)
        p.stdin.write(b'{"jsonrpc":"2.0","id":1,"method":"ping"}\n')
        p.stdin.flush()
        self.assertEqual(Frames(p.stdout).receive()["result"], {})
        p.stdin.write(b'{"jsonrpc":"2.0","id":1.5,"method":"ping"}\n')
        p.stdin.flush()
        self.assertEqual(Frames(p.stdout).receive()["id"], 1.5)
        p.stdin.close()
        self.assertEqual(p.wait(timeout=4), 0)
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), before)
        self.assertEqual(Path(self.binding["identity_database"]).read_bytes(), database_before)

    def test_cli_cancel_private_permissions_live_cleanup_and_unknown_file_refusal(self):
        self.start()
        client = self.client()
        self.request(8)
        request = client[1].event("request", 8)
        self.wait_call("status", "ab_first")
        selector = {k:request[k] for k in ["id", "request_generation"]}
        for expected in [0, 1]:
            p = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "request-control-cli.ts"), "cancel", str(self.slot)])
            stdout, stderr = p.communicate(json.dumps(selector).encode() + b"\n", timeout=3)
            self.assertEqual(p.returncode, expected, stderr)
            self.assertEqual(json.loads(stdout)["accepted"], expected == 0)
            self.assertNotIn(self.descriptor["token"].encode(), stdout + stderr)
        for filename in ["control.json", "control.sock"]:
            path = self.slot / filename
            path.chmod(0o644)
            p = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "request-control-cli.ts"), "observe", str(self.slot)])
            p.communicate(timeout=3)
            self.assertEqual(p.returncode, 2)
            path.chmod(0o600)
        p = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "request-control-cli.ts"), "cleanup", str(self.slot)])
        p.communicate(timeout=3)
        self.assertEqual(p.returncode, 2)
        self.assertTrue(self.slot.exists())
        (self.slot / "unexpected").touch()
        self.bridge.kill()
        self.bridge.wait(timeout=3)
        p = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "request-control-cli.ts"), "cleanup", str(self.slot)])
        p.communicate(timeout=3)
        self.assertEqual(p.returncode, 2)
        self.assertTrue((self.slot / "unexpected").exists())
        (self.slot / "unexpected").unlink()
        p = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "request-control-cli.ts"), "cleanup", str(self.slot)])
        p.communicate(timeout=3)
        self.assertEqual(p.returncode, 0)

    def test_bounded_raw_observation_and_native_cancel_share_signal(self):
        self.start()
        client = self.client(raw=True)
        self.bridge.stdin.write((json.dumps(dict(jsonrpc="2.0", id="large", method="tools/call", params=dict(name="bash", arguments=dict(extra="x" * (300 * 1024))))) + "\n").encode())
        self.bridge.stdin.flush()
        observed = client[1].event("request", "large")
        self.assertEqual(observed["payload_omitted"], "size_limit")
        self.assertGreater(observed["payload_bytes"], 256 * 1024)
        self.assertNotIn("payload", observed)
        self.assertTrue(self.rpc.receive()["result"]["isError"])
        client[1].event("retired", "large")
        self.assertEqual(self.calls(), [])
        self.request("native")
        request = client[1].event("request", "native")
        self.wait_call("status", "ab_first")
        self.bridge.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"native"}}\n')
        self.bridge.stdin.flush()
        self.assertIn("Cancellation requested", self.rpc.receive()["result"]["content"][0]["text"])
        client[1].event("retired", "native")
        self.assertFalse(self.cancel(client, request))
        self.assertEqual([c["handle"] for c in self.calls() if c["op"] == "cancel"], ["ab_first"])

    def test_sigterm_retires_endpoint_before_cancellation_grace(self):
        self.start()
        hold = self.root / "hold-ab_first"
        hold.touch()
        self.request(1)
        self.wait_call("run", "ab_first")
        self.bridge.terminate()
        deadline = time.monotonic() + 1
        while self.slot.exists() and time.monotonic() < deadline:
            time.sleep(.005)
        self.assertFalse(self.slot.exists(), "endpoint must retire before blocked dispatch/cancellation grace")
        hold.unlink()
        self.assertEqual(self.bridge.wait(timeout=4), 0)
        self.assertFalse(self.slot.exists())
        self.assertEqual([c["handle"] for c in self.calls() if c["op"] == "cancel"], ["ab_first"])

    def test_stdin_close_active_request_retires_endpoint_and_delivers_one_cancel(self):
        self.start()
        client = self.client(raw=True)
        self.request("stdin-close")
        client[1].event("request", "stdin-close")
        self.wait_call("status", "ab_first")
        self.bridge.stdin.close()
        response = client[1].event("response", "stdin-close")
        self.assertIn("Cancellation requested", response["payload"]["result"]["content"][0]["text"])
        client[1].event("retired", "stdin-close")
        self.assertEqual(self.bridge.wait(timeout=4), 0)
        self.assertFalse(self.slot.exists())
        self.assertEqual([c["handle"] for c in self.calls() if c["op"] == "cancel"], ["ab_first"])

    def test_crash_recovery_stale_recycled_endpoint(self):
        self.start()
        self.bridge.kill()
        self.bridge.wait(timeout=2)
        self.assertTrue(self.slot.exists())
        for operation in ["observe", "cancel"]:
            p = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "request-control-cli.ts"), operation, str(self.slot)])
            _, stderr = p.communicate(timeout=3)
            self.assertEqual(p.returncode, 2, stderr)
        # A PID reused by a live different incarnation must not be contacted or signalled.
        descriptor = json.loads((self.slot / "control.json").read_text())
        descriptor["bridge"]["pid"] = os.getpid()
        descriptor["bridge"]["incarnation"] = descriptor["bridge"]["incarnation"].rsplit(":", 1)[0] + ":0"
        (self.slot / "control.json").write_text(json.dumps(descriptor))
        p = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "request-control-cli.ts"), "cleanup", str(self.slot)])
        p.communicate(timeout=3)
        self.assertEqual(p.returncode, 0)
        self.assertFalse(self.slot.exists())

    def test_runner_identity_and_stale_slot_fail_closed(self):
        for change in [dict(provider_incarnation=self.binding["provider_incarnation"].rsplit(":", 1)[0] + ":0"), dict(parent=dict(id="aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", source="fixture")), dict(provider_instance_id="other")]:
            env = dict(self.env)
            env[BINDING] = json.dumps(dict(self.binding, **change))
            p = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "agent-bash-mcp.ts")], env)
            p.communicate(timeout=8)
            self.assertEqual(p.returncode, 1)
            self.assertFalse(self.slot.exists())
        self.start()
        old = (self.slot / "control.json").read_bytes()
        p = self.spawn([shutil.which("bun"), "--no-install", str(HERE / "agent-bash-mcp.ts")])
        p.communicate(timeout=8)
        self.assertEqual(p.returncode, 1)
        self.assertEqual((self.slot / "control.json").read_bytes(), old)

    def test_provider_cli_binds_real_outer_launch_and_controls_installed_shape(self):
        repo = HERE.resolve().parents[1]
        binary = repo / "target/debug/agent-runner-codex"
        self.assertTrue(binary.is_file(), "cargo build --offline is required")
        native = self.root / "native-codex"
        native.write_text('''#!/usr/bin/env python3
import json, os, subprocess, sys
if sys.argv[1:] == ['--version']:
    print('codex-cli 0.153.4'); sys.exit(0)
sys.stdin.read()
print(json.dumps({'type':'thread.started','thread_id':''' + repr(UUID) + '''}), flush=True)
bridge = subprocess.Popen([''' + repr(shutil.which("bun")) + ''', '--no-install', ''' + repr(str(HERE / "agent-bash-mcp.ts")) + '''], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
bridge.stdin.write(b'{"jsonrpc":"2.0","id":"provider-control","method":"tools/call","params":{"name":"bash","arguments":{"command":"first"}}}\\n')
bridge.stdin.flush()
reply = json.loads(bridge.stdout.readline())
assert 'Cancellation requested' in reply['result']['content'][0]['text']
bridge.stdin.close(); assert bridge.wait(timeout=4) == 0
for event in [{'type':'item.completed','item':{'type':'agent_message','text':'controlled'}},{'type':'turn.completed'}]:
    print(json.dumps(event), flush=True)
''')
        native.chmod(0o700)
        config = self.root / "config/agent-runner-codex"
        config.mkdir(parents=True)
        prompt = self.root / "system.md"
        prompt.write_text("Fixture system instructions")
        runtime = dict(codex_bin=str(native), bun_bin=shutil.which("bun"), bash_mcp_path=str(HERE / "agent-bash-mcp.ts"), system_prompt_file=str(prompt), agent_bash_bin=str(self.root / "agent-bash"), agent_runner_bin=str(self.root / "agent-bash"))
        (config / "config.toml").write_text("\n".join(k + "=" + json.dumps(v) for k, v in runtime.items()))
        provider = self.spawn([str(binary), "launch"])
        _, boot, ticks = incarnation(provider.pid).split(":")
        with sqlite3.connect(self.binding["identity_database"]) as db:
            db.execute("INSERT INTO pid_identity VALUES (?,?,?,?,?)", (provider.pid, boot, int(ticks), UUID, "codex-fixture"))
        request = dict(contract="oulipoly.provider/v1", request_id="actual-outer-request", provider_instance_id="codex-fixture", host=dict(app="fixture", config_root=str(config.parent), data_root=str(self.root), env=dict(HOME=str(self.root))), params=dict(settings_id="codex", mode="arg", model=dict(name="gpt-high", provider_args=["-m", "gpt-6-astra", "-c", 'model_reasoning_effort="high"'], inputs=dict(prompt="Fixture task", named={})), argv=["codex", "exec", "--dangerously-bypass-approvals-and-sandbox", "-m", "gpt-6-astra", "-c", 'model_reasoning_effort="high"'], working_directory=str(self.root), env={DIR:str(self.slot), "OULIPOLY_PARENT_INVOCATION":json.dumps(self.binding["parent"]), "OULIPOLY_DATA_DIR":str(self.root)}))
        provider.stdin.write(json.dumps(request).encode())
        provider.stdin.close()
        deadline = time.monotonic() + 7
        while not (self.slot / "control.sock").exists() and time.monotonic() < deadline:
            if provider.poll() is not None:
                self.fail("provider exited before control: " + provider.stdout.read().decode())
            time.sleep(.01)
        self.assertTrue((self.slot / "control.sock").exists())
        cli = self.spawn([str(binary), "request-control", "--config-root", str(config.parent), "observe", str(self.slot), "--raw"])
        frames = Frames(cli.stdout)
        attached = frames.event("attached")
        self.assertEqual(attached["binding"]["provider_pid"], provider.pid)
        self.assertEqual(attached["binding"]["request_id"], request["request_id"])
        self.assertEqual(attached["binding"]["parent"]["id"], UUID)
        active = attached["active"]
        target = active[0] if active else frames.event("request", "provider-control")
        self.wait_call("status", "ab_first")
        cli.stdin.write((json.dumps({k:target[k] for k in ["id", "request_generation"]}) + "\n").encode())
        cli.stdin.flush()
        self.assertTrue(frames.event("cancel")["accepted"])
        self.assertIn("Cancellation requested", frames.event("response", "provider-control")["payload"]["result"]["content"][0]["text"])
        frames.event("retired", "provider-control")
        self.assertEqual(provider.wait(timeout=5), 0)
        self.assertFalse(self.slot.exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
