"""Opt-in Linux pairing of a built Runner, this adapter and a fake Codex.

No installed routes, profiles, accounts or models are used. See README for
explicit binary, secure scratch and optional synthetic evidence inputs.
"""
import json
import os
from pathlib import Path
import queue
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest

HERE = Path(__file__).resolve().parent
TIMEOUT = 30


class Lines:
    def __init__(self, argv, cwd, env):
        self.argv, self.cwd, self.env = argv, str(cwd), env
        self.proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.pending, self.seen, self.errors = queue.Queue(), [], []
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.error_reader = threading.Thread(target=self._errors, daemon=True)
        self.reader.start()
        self.error_reader.start()

    def _read(self):
        for line in self.proc.stdout:
            try:
                self.pending.put(json.loads(line))
            except ValueError:
                self.pending.put({"invalid_json": line})
        self.pending.put(None)

    def _errors(self):
        self.errors.extend(self.proc.stderr.readlines())

    def send(self, value):
        self.proc.stdin.write(json.dumps(value) + "\n")
        self.proc.stdin.flush()

    def until(self, predicate):
        deadline = time.monotonic() + TIMEOUT
        while True:
            value = self.pending.get(timeout=max(0.001, deadline - time.monotonic()))
            if value is None:
                raise AssertionError("process ended before expected event: " + str(self.seen))
            self.seen.append(value)
            if predicate(value):
                return value

    def event(self, name):
        return self.until(lambda v: v.get("event") == name or v.get("entry") == name)

    def finish(self):
        code = self.proc.wait(timeout=TIMEOUT)
        self.reader.join(TIMEOUT)
        self.error_reader.join(TIMEOUT)
        while not self.pending.empty():
            value = self.pending.get_nowait()
            if value is not None:
                self.seen.append(value)
        return code

    def cleanup(self):
        if self.proc.poll() is None:
            try:
                self.send({"cmd": "cancel"})
                self.proc.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                self.proc.kill()
                self.proc.wait(timeout=5)
        self.reader.join(5)
        self.error_reader.join(5)
        for stream in (self.proc.stdin, self.proc.stdout, self.proc.stderr):
            stream.close()

    def record(self):
        return {"argv": self.argv, "cwd": self.cwd, "env": self.env,
                "pid": self.proc.pid, "exit": self.proc.poll(),
                "events": self.seen, "stderr": self.errors}


class Fixture:
    def __init__(self, root, adapter):
        self.root, self.adapter = Path(root), str(Path(adapter).resolve())
        self.home = self.root / "home"
        self.home.mkdir()
        self.native = self.root / "codex"
        shutil.copyfile(HERE / "fixtures/codex_pairing_native.py", self.native)
        self.native.chmod(0o700)
        self.requester = self.root / "agent-bash"
        self.requester.write_text("#!/bin/sh\nexit 97\n")
        self.requester.chmod(0o700)
        self.config = self.root / "config"
        cfg = self.config / "agent-runner-codex"
        cfg.mkdir(parents=True)
        (self.root / "system.md").write_text("synthetic pairing system instructions\n")
        (self.root / "mcp.ts").write_text("// synthetic; never executed\n")
        shutil.copyfile(HERE.parent / "integrations/codex/models.json", self.root / "models.json")
        values = {"codex_bin": self.native, "bun_bin": self.requester,
                  "bash_mcp_path": self.root / "mcp.ts", "system_prompt_file": self.root / "system.md",
                  "agent_bash_bin": self.requester, "agent_runner_bin": self.requester}
        (cfg / "config.toml").write_text("".join(k + " = " + json.dumps(str(v)) + "\n"
                                                 for k, v in values.items()))
        self.calls = self.root / "calls.jsonl"

    def settings(self, shape="stdin"):
        prompt = "synthetic preparation sentinel"
        argv = ["codex2", "exec", "--dangerously-bypass-approvals-and-sandbox", "-m", "gpt-6-astra",
                "-c", 'model_reasoning_effort="high"']
        model = {"name": "gpt-astra-high", "provider_args": argv[3:],
                 "inputs": {"prompt": prompt, "named": {}}}
        launch = {"argv": argv.copy(), "env": {"CALLS": str(self.calls), "SENTINEL": "opaque-env", "PAIRING_PROVIDER_ROOT": str(self.root)}}
        mode = "stdin"
        if shape == "arg":
            mode = "arg"
            launch["argv"].append(prompt)
        elif shape == "launch-prompt":
            model["inputs"]["prompt"] = None
            launch["prompt"] = prompt
        elif shape == "different-prompts":
            launch["prompt"] = "unused synthetic launch prompt"
        elif shape == "system-override":
            launch["system_prompt_override"] = "synthetic override"
        return {"settings_id": "codex2", "mode": mode, "model": model, "launch": launch}

    def request(self, text, shape="stdin", allow=False):
        provider = {"executable": self.adapter, "settings": self.settings(shape),
                    "config_root": str(self.config), "env": {"HOME": str(self.home)},
                    "agent_bash_bin": str(self.requester)}
        provider.update({"bash_allow": ["printf allowed"]} if allow else {"bash_authority": "trusted-task"})
        return {"store": str(self.root / "store"), "launch_dir": str(self.root / "launch"),
                "cwd": str(self.root), "env": {"PATH": "/usr/bin:/bin", "HOME": str(self.home)},
                "messages": [text], "outage_closure_cap": 2, "delivery_attempt_cap": 2,
                "provider": provider, "workload": {"isolation": "unprivileged-userns"}}

    def native_calls(self):
        return [json.loads(x) for x in self.calls.read_text().splitlines()] if self.calls.exists() else []


def alive(pid):
    try:
        return not Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].startswith("Z")
    except FileNotFoundError:
        return False


def require_dead(pid):
    deadline = time.monotonic() + 5
    while alive(pid) and time.monotonic() < deadline:
        time.sleep(0.02)
    if alive(pid):
        raise AssertionError(f"owned synthetic process survived: {pid}")


@unittest.skipUnless(os.environ.get("OULIPOLY_PAIRING_RUNNER") and os.environ.get("OULIPOLY_PAIRING_CODEX"),
                     "requires explicit built Runner and Codex adapter")
class RunnerPairing(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="pair-", dir=os.environ["OULIPOLY_PAIRING_SCRATCH"])
        self.fixture = Fixture(self.directory.name, os.environ["OULIPOLY_PAIRING_CODEX"])
        self.runs, self.requests = [], []

    def tearDown(self):
        try:
            for run in reversed(self.runs):
                run.cleanup()
            evidence = os.environ.get("OULIPOLY_PAIRING_EVIDENCE")
            if evidence:
                target = Path(evidence) / self.id().split(".")[-1]
                target.mkdir(parents=True, exist_ok=True)
                (target / "runs.json").write_text(json.dumps([r.record() for r in self.runs], indent=2))
                (target / "requests.json").write_text(json.dumps(self.requests, indent=2))
                shutil.copytree(self.fixture.root, target / "fixture", dirs_exist_ok=True)
        finally:
            self.directory.cleanup()

    def start(self, text="first", shape="stdin", allow=False):
        request = self.fixture.request(text, shape, allow)
        self.requests.append(request)
        path = self.fixture.root / "request.json"
        path.write_text(json.dumps(request))
        run = Lines([os.environ["OULIPOLY_PAIRING_RUNNER"], "native-root", "--request", str(path)],
                    self.fixture.root, {"PATH": "/usr/bin:/bin", "HOME": str(self.fixture.home)})
        self.runs.append(run)
        declared = run.event("provider-described")
        self.assertEqual(declared["agreed_contract"], "oulipoly.provider/v1")
        self.assertEqual(declared["resident_session"], 1)
        setup = run.event("setup-completed")
        self.assertEqual(setup["launch"]["argv"][1], "resident.serve")
        return run

    def turn(self, run, index):
        ack = run.until(lambda v: v.get("event") == "ack" and v.get("index") == index)
        end = run.until(lambda v: v.get("event") == "turn-end" and v.get("input") == index)
        self.assertEqual(end["message_id"], ack["message_id"])
        self.assertTrue(end["own_turn_end"])
        self.assertEqual(end["endpoint_durability"], "not-established")
        self.assertEqual(end["canonical_publication"], "not-established")
        self.assertEqual(end["native_report"]["physical_custody"], "not-certified-by-report")
        return ack, end

    def close(self, run):
        run.send({"cmd": "close"})
        self.assertEqual(run.finish(), 87)
        terminal = next(v for v in run.seen if v.get("event") == "terminal")
        self.assertEqual(terminal["status"], "closed")
        self.assertEqual(terminal["owed"], 0)
        self.assertTrue(terminal["all_harnesses_reaped"])

    def settings_and_two_turns(self, shape):
        run = self.start(shape=shape, allow=(shape == "arg"))
        _, first = self.turn(run, 0)
        run.send({"cmd": "send", "text": "second", "ref": "f1"})
        run.event("follow-up-admitted")
        _, second = self.turn(run, 1)
        self.assertEqual(first["session"], second["session"])
        self.close(run)
        calls = self.fixture.native_calls()
        self.assertEqual(len(calls), 2)
        self.assertEqual([c["prompt"] for c in calls], ["first", "second"])
        self.assertIn("resume", calls[1]["argv"])
        self.assertTrue(all(c["sentinel"] == "opaque-env" for c in calls))
        self.assertTrue(all(c["home"].startswith(str(self.fixture.home)) for c in calls))

    def test_stdin_settings(self):
        self.settings_and_two_turns("stdin")

    def test_arg_settings_with_allow_policy(self):
        self.settings_and_two_turns("arg")

    def test_launch_prompt_settings(self):
        self.settings_and_two_turns("launch-prompt")

    def test_different_prompts_settings(self):
        self.settings_and_two_turns("different-prompts")

    def test_system_override_settings(self):
        self.settings_and_two_turns("system-override")

    def test_native_failure_is_inserted_and_reported(self):
        run = self.start("fail synthetic")
        _, end = self.turn(run, 0)
        self.assertEqual(end["native_report"]["status_code"], 1)
        self.close(run)
        self.assertEqual(len(self.fixture.native_calls()), 1)

    def test_nonconsumption_holds_input_until_cancel(self):
        run = self.start("noconsume synthetic")
        rejected = run.event("rejected")
        self.assertEqual(rejected["code"], -32010)
        self.assertEqual(rejected["insertion"], "unresolved")
        self.assertEqual(rejected["retry"], "not-authorized")
        run.send({"cmd": "close"})
        run.event("close-not-applied")
        run.send({"cmd": "cancel"})
        self.assertEqual(run.finish(), 82)
        self.assertFalse(any(v.get("event") == "ack" for v in run.seen))
        self.assertEqual(len(self.fixture.native_calls()), 1)
        terminal = next(v for v in run.seen if v.get("event") == "terminal")
        self.assertEqual(terminal["owed"], 1)
        message = terminal["harnesses"][0]["messages"][0]
        self.assertEqual(message["label"], "rejected-unresolved")
        self.assertEqual(message["turn_end"], "not-recorded")
        with sqlite3.connect(self.fixture.root / "store/intent.sqlite3") as db:
            self.assertEqual(db.execute("SELECT count(*) FROM attempt WHERE outcome='rejected-unresolved'").fetchone()[0], 1)
            self.assertEqual(db.execute("SELECT count(*) FROM message WHERE stop='rejected-unresolved' AND ack_label IS NULL").fetchone()[0], 1)

    def test_cancel_reaps_native_descendant(self):
        run = self.start("hang " + str(self.fixture.root))
        run.event("ack")
        run.event("agent-message")
        self.assertTrue((self.fixture.root / "descendant.pid").is_file())
        run.send({"cmd": "cancel"})
        self.assertEqual(run.finish(), 82)
        terminal = next(v for v in run.seen if v.get("event") == "terminal")
        self.assertTrue(terminal["all_harnesses_reaped"])
        self.assertEqual(terminal["root_pid1"]["live"], [])
        self.assertTrue(terminal["root_pid1"]["end_observed"])
        exited = next(v for v in run.seen if v.get("event") == "exited")
        self.assertTrue(exited["namespace"]["drained"])
        # Native PIDs are namespace-local, so never probe them as host PIDs.
        self.assertEqual(len(self.fixture.native_calls()), 1)


    def test_late_record_failure_keeps_publication_unestablished(self):
        run = self.start("recordfail synthetic")
        ack, end = self.turn(run, 0)
        diagnostic = run.event("endpoint-record-error")
        self.assertEqual(diagnostic["message_id"], ack["message_id"])
        self.assertEqual(diagnostic["input"], 0)
        self.assertEqual(diagnostic["attribution"], "endpoint-native-tag")
        self.assertEqual(diagnostic["private_details"], "withheld")
        self.assertEqual(diagnostic["canonical_publication"], "not-established")
        self.assertEqual(end["native_report"]["status_code"], 0)
        self.close(run)
        self.assertEqual(len(self.fixture.native_calls()), 1)

    def test_owner_loss_cancel_recovery_does_not_rerun(self):
        run = self.start("hang " + str(self.fixture.root))
        run.event("ack")
        run.event("agent-message")
        launched = next(v for v in run.seen if v.get("event") == "launched")
        serve_pid = launched["pid"]
        run.proc.kill()
        run.proc.wait(timeout=TIMEOUT)
        self.assertTrue(alive(serve_pid))
        request = {"store": str(self.fixture.root / "store"), "purpose": "cancel",
                   "env": {"PATH": "/usr/bin:/bin", "HOME": str(self.fixture.home)}}
        self.requests.append(request)
        path = self.fixture.root / "recover.json"
        path.write_text(json.dumps(request))
        recovery = Lines([os.environ["OULIPOLY_PAIRING_RUNNER"], "native-root", "--recover", str(path)],
                         self.fixture.root, {"PATH": "/usr/bin:/bin", "HOME": str(self.fixture.home)})
        self.runs.append(recovery)
        self.assertEqual(recovery.finish(), 82)
        terminal = next(v for v in recovery.seen if v.get("event") == "terminal")
        self.assertEqual(terminal["status"], "cancelled")
        require_dead(serve_pid)
        self.assertEqual(len(self.fixture.native_calls()), 1)


class Endpoint(Lines):
    def __init__(self, fixture, prepared):
        super().__init__([fixture.adapter] + prepared["invocation"]["args"], fixture.root,
                         {"PATH": "/usr/bin:/bin", "HOME": str(fixture.home)})
        self.sequence = 0
        self.sessions = []

    def rpc(self, method, params):
        self.sequence += 1
        self.send({"jsonrpc": "2.0", "id": self.sequence, "method": method, "params": params})
        return self.until(lambda v: "method" not in v and v.get("id") == self.sequence)

    def prompt(self, session, text, key):
        return self.rpc("session/prompt", {"sessionId": session,
                        "prompt": [{"type": "text", "text": text}],
                        "_meta": {"oulipoly.ai/messageKey": key}})

    def idle(self, message):
        return self.until(lambda v: v.get("method") == "session/update" and
                          v["params"]["update"].get("state") == "idle" and
                          v["params"]["update"].get("_meta", {}).get("oulipoly.ai/lastUserMessageId") == message)

    def cleanup(self):
        if self.proc.poll() is None:
            for session in self.sessions:
                self.send({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}})
            self.proc.stdin.close()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        super().cleanup()


@unittest.skipUnless(os.environ.get("OULIPOLY_PAIRING_CODEX"), "requires explicit built Codex adapter")
class EndpointSemantics(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="ep-", dir=os.environ["OULIPOLY_PAIRING_SCRATCH"])
        self.fixture = Fixture(self.directory.name, os.environ["OULIPOLY_PAIRING_CODEX"])
        self.runs, self.operations = [], []
        settings = self.fixture.settings()
        host = {"app": "synthetic-pairing", "config_root": str(self.fixture.config),
                "data_root": str(self.fixture.root / "data"), "env": {"HOME": str(self.fixture.home),
                "OULIPOLY_HOST_RESIDENT_SESSION_V1": "1"}}
        def invoke(op, params):
            request = {"contract": "oulipoly.provider/v1", "request_id": "pair-" + op,
                       "host": host, "params": params}
            argv = [self.fixture.adapter, op]
            env = {"PATH": "/usr/bin:/bin", "HOME": str(self.fixture.home)}
            result = subprocess.run(argv, input=json.dumps(request), text=True, capture_output=True,
                                    cwd=self.fixture.root, env=env, timeout=TIMEOUT)
            response = json.loads(result.stdout)
            self.operations.append({"argv": argv, "cwd": str(self.fixture.root), "env": env,
                                    "request": request, "response": response,
                                    "exit": result.returncode, "stderr": result.stderr})
            self.assertTrue(response["ok"])
            return response["result"]
        policy = invoke("policy.evaluate", settings)
        self.assertTrue(policy["accepted"])
        self.assertEqual(policy["stdin"], settings["model"]["inputs"]["prompt"])
        template = {"settings_id": settings["settings_id"], "mode": settings["mode"],
                    "model": settings["model"], "argv": policy["argv"], "env": policy["env"]}
        self.prepared = invoke("resident.prepare", {"protocol": "oulipoly.resident_session/v1", "launch": template})
        self.assertEqual(self.fixture.native_calls(), [])

    def tearDown(self):
        try:
            for run in reversed(self.runs):
                run.cleanup()
            evidence = os.environ.get("OULIPOLY_PAIRING_EVIDENCE")
            if evidence:
                target = Path(evidence) / self.id().split(".")[-1]
                target.mkdir(parents=True, exist_ok=True)
                (target / "runs.json").write_text(json.dumps([r.record() for r in self.runs], indent=2))
                (target / "operations.json").write_text(json.dumps(self.operations, indent=2))
                shutil.copytree(self.fixture.root, target / "fixture", dirs_exist_ok=True)
        finally:
            self.directory.cleanup()

    def endpoint(self, existing=None):
        ep = Endpoint(self.fixture, self.prepared)
        self.runs.append(ep)
        initialized = ep.rpc("initialize", {"protocolVersion": 2,
                             "clientInfo": {"name": "synthetic-pairing", "version": "0"}})
        self.assertEqual(initialized["result"]["protocolVersion"], 2)
        declaration = initialized["result"]["_meta"]["oulipoly.ai/residentSession"]
        self.assertEqual(declaration["protocol"], "oulipoly.resident_session/v1")
        self.assertEqual(declaration["acp_schema"], "schema-v2.0.0-alpha.7")
        if existing:
            return ep, ep.rpc("session/resume", {"sessionId": existing, "cwd": str(self.fixture.root)})
        opened = ep.rpc("session/new", {"cwd": str(self.fixture.root), "mcpServers": []})
        session = opened["result"]["sessionId"]
        ep.sessions.append(session)
        return ep, session

    def test_insert_end_report_dedup_and_resume(self):
        ep, session = self.endpoint()
        first = ep.prompt(session, "first", "key-first")
        message = first["result"]["messageId"]
        idle = ep.idle(message)
        report = idle["params"]["update"]["_meta"]["oulipoly.ai/nativeTurn"]
        self.assertEqual(report["custody"], "complete")
        self.assertEqual(report["status"]["code"], 0)
        duplicate = ep.prompt(session, "first", "key-first")
        self.assertEqual(duplicate["result"]["messageId"], message)
        self.assertTrue(duplicate["result"]["_meta"]["oulipoly.ai/duplicate"])
        ep.cleanup()
        ep2, resumed = self.endpoint(session)
        ep2.sessions.append(session)
        self.assertIn("result", resumed)
        duplicate = ep2.prompt(session, "first", "key-first")
        self.assertEqual(duplicate["result"]["messageId"], message)
        second = ep2.prompt(session, "second", "key-second")
        ep2.idle(second["result"]["messageId"])
        self.assertEqual(len(self.fixture.native_calls()), 2)
        self.assertIn("resume", self.fixture.native_calls()[1]["argv"])

    def test_missing_launch_evidence_blocks_recovery_without_rerun(self):
        ep, session = self.endpoint()
        inserted = ep.prompt(session, "first", "key-first")
        ep.idle(inserted["result"]["messageId"])
        ep.cleanup()
        root = self.fixture.root / "data/provider-state/codex/resident/sessions" / session
        inputs = list((root / "inputs").glob("*.json"))
        self.assertEqual(len(inputs), 1)
        record = json.loads(inputs[0].read_text())
        record.update(phase="accepted", dispatched=True, native_turn=None,
                      consumption_seen=False, inserted_unix_ms=None)
        inputs[0].write_text(json.dumps(record))
        self.operations.append({"fixture_mutation": "lost completed launch evidence; input set dispatched accepted",
                                "input": str(inputs[0]), "record": record})
        # Deliberate loss of previously completed synthetic evidence. No live actor.
        for path in (root / "turns").iterdir():
            if path.suffix in [".json", ".jsonl"]:
                path.unlink()
        ep2, resumed = self.endpoint(session)
        self.assertIn("error", resumed, "missing launch evidence must refuse recovery")
        self.assertEqual(resumed["error"]["code"], -32012)
        self.assertEqual(len(self.fixture.native_calls()), 1)
        retained = json.loads(inputs[0].read_text())
        self.assertNotEqual(retained["phase"], "not_inserted")
        ep2.sessions.append(session)
        # Shutdown may fail because launch custody remains unresolved; retain its exit.



if __name__ == "__main__":
    unittest.main()
