#!/usr/bin/env python3
"""Verify managed Bash and automatic notifications in a real Runner/Codex PTY."""

import argparse
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import runpy
import select
import shlex
import signal
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time
import tomllib
import uuid


def rows(database, statement, parameters=()):
    if not database.exists():
        return []
    with sqlite3.connect(f"file:{database}?mode=ro", uri=True, timeout=2) as connection:
        connection.row_factory = sqlite3.Row
        try:
            return [dict(row) for row in connection.execute(statement, parameters)]
        except sqlite3.OperationalError as error:
            if "no such table" in str(error):
                return []
            raise


def locate(provider, config, account, session):
    request = {"contract":"oulipoly.provider/v1", "request_id":str(uuid.uuid4()),
               "provider_instance_id":account, "host":{"app":"pty-verifier", "config_root":str(config)},
               "params":{"settings_id":account, "session_id":session}}
    result = subprocess.run([str(provider), "session.locate_transcript"], input=json.dumps(request),
                            capture_output=True, text=True, timeout=20)
    result.check_returncode()
    response = json.loads(result.stdout)
    if not response.get("result", {}).get("located"):
        raise RuntimeError("Bound PTY session has no located native transcript")
    return Path(response["result"]["path"])


def transcript(path):
    records = []
    for line in path.read_text().splitlines():
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError:
            break
    return records


def process_start(pid):
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
        return int(stat[stat.rfind(")")+2:].split()[19])
    except (OSError, ValueError, IndexError):
        return None


def cleanup_workloads(output, runner, environment):
    database = output/"data/pid-identity.db"
    # Prevent a detached completion from starting more test turns during teardown.
    for session in rows(database, "SELECT session_id FROM session_runtime"):
        subprocess.run([str(runner), "mailbox", "pause", "--session-id", session["session_id"]],
                       env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
    spooler = output/"agent-bash/agent-bash"
    for path in (output/"agent-bash-state").glob("*/meta.json"):
        metadata = json.loads(path.read_text())
        if metadata.get("state") not in ("DONE", "FAILED", "CANCELLED"):
            subprocess.run([str(spooler), "cancel", path.parent.name], env=environment,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
    # Runner's PTY children use their own sessions/process groups. Killing only
    # the harness's runner PID would not own those groups. Verify each exact
    # recorded native PID incarnation before addressing its process group.
    for generation in rows(database, "SELECT spawned_os_pid,identity_os_pid_starttime_ticks FROM runtime_generation WHERE lifecycle_state != 'exited'"):
        native_pid = generation["spawned_os_pid"]
        if native_pid and process_start(native_pid) == generation["identity_os_pid_starttime_ticks"]:
            try:
                if os.getpgid(native_pid) == native_pid:
                    os.killpg(native_pid, signal.SIGTERM)
                else:
                    os.kill(native_pid, signal.SIGTERM)
            except ProcessLookupError:
                pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="Spend bounded Luna/low turns in a PTY and its child")
    parser.add_argument("--account", default="codex3", choices=("codex", "codex2", "codex3", "codex4", "codex5"))
    parser.add_argument("--child-account", choices=("codex", "codex2", "codex3", "codex4", "codex5"),
                        help="Use another native account for the child and verify SQLite ownership")
    parser.add_argument("--runner", type=Path, required=True)
    parser.add_argument("--provider", type=Path, required=True)
    parser.add_argument("--mcp-bridge", type=Path,
                        help="Bash MCP bridge to verify; defaults to this checkout's bridge")
    parser.add_argument("--config-root", type=Path, default=Path.home()/".config/oulipoly-agent-runner")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    output = (args.output_dir or Path(tempfile.mkdtemp(prefix="codex-pty-live-"))).absolute()
    prepare = Path(__file__).with_name("verify-live.py")
    bridge = (args.mcp_bridge or Path(__file__).resolve().parents[1]/"integrations/codex/agent-bash-mcp.ts").expanduser().absolute()
    subprocess.run([sys.executable, str(prepare), "--account", args.account,
                    "--runner", str(args.runner.absolute()), "--provider", str(args.provider.absolute()),
                    "--mcp-bridge", str(bridge),
                    "--config-root", str(args.config_root.absolute()), "--output-dir", str(output)], check=True)
    config = output/"config/oulipoly-agent-runner"
    runner = output/"runner/oulipoly-agent-runner"
    token = "PTY-CHILD-" + uuid.uuid4().hex
    child_prompt = output/"child-prompt.txt"
    child_prompt.write_text(f"This is a bounded notification test. Reply exactly {token}. Do not use tools or launch children.\n")
    command = shlex.join(["agents", "-m", "codex-exec-bench", "-p", str(output), "-f", str(child_prompt)])
    prompt = ("This is a bounded PTY integration test. Use the Bash MCP tool exactly once to run "
              + json.dumps(command) + ". This launches a child agent asynchronously. Reply PTY-WAITING "
              "and end your turn. Do not poll or wait. When its completion notification arrives, "
              "use Bash to read the notification's log path and reply PTY-RECEIVED followed by the "
              "PTY-CHILD token found in that log. Do not launch any more children.")
    providers_file = config/"providers.toml"
    provider_text = providers_file.read_text()
    data = tomllib.loads(provider_text)
    initial_args = data[args.account]["interactive_args"]
    import re
    provider_text, count = re.subn(r"(?m)^interactive_args\s*=.*$",
                                  lambda _: "interactive_args = " + json.dumps(initial_args + ["--prompt", prompt]),
                                  provider_text)
    if count != 1:
        raise RuntimeError("Isolated PTY test requires exactly one provider account")
    providers_file.write_text(provider_text)
    parent_model = "codex-exec-bench"
    child_account = args.child_account or args.account
    if child_account != args.account:
        # Separate model pools force account selection without changing any
        # installed route. Both labels select precisely Luna at low effort.
        labels = runpy.run_path(str(Path(__file__).with_name("install-labels.py")))
        prepare_helpers = runpy.run_path(str(prepare))
        all_accounts = labels["update_providers"]((args.config_root/"providers.toml").read_text(),
                                                  args.provider.absolute(), config)
        providers_file.write_text(provider_text + "\n" + prepare_helpers["selected_tables"](all_accounts, child_account))
        parent_model = "gpt-luna-low"
        (config/f"models/{parent_model}.toml").write_text((config/"models/codex-exec-bench.toml").read_text())
        model = labels["model_text"]("low", args.provider.absolute(), "gpt-5.6-luna")
        parts = model.split("[[providers]]")
        model = parts[0] + "".join("[[providers]]" + part for part in parts[1:]
                                  if f'name = "{child_account}"' in part)
        (config/"models/codex-exec-bench.toml").write_text(model)
    plan = {"mode":"pty_interactive", "model":"gpt-5.6-luna", "effort":"low", "account":args.account,
            "child_account":child_account,
            "runner":str(runner), "provider":str(args.provider.absolute()), "output_dir":str(output)}
    (output/"pty-plan.json").write_text(json.dumps(plan, indent=2)+"\n")
    if not args.run:
        print(f"Prepared PTY verification: {output}. Pass --run with a fresh directory to execute.")
        return
    helper = runpy.run_path(str(prepare))
    environment = helper["isolated_environment"](output/"config", output/"data")
    environment.update({"TERM":"xterm-256color", "COLUMNS":"120", "LINES":"40"})
    pid, master = pty.fork()
    if pid == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        os.execve(str(runner), [str(runner), "repl", parent_model, "-p", str(output)], environment)
    deadline = time.monotonic() + args.timeout
    rollout = None
    session = None
    reaped = False
    try:
        with (output/"pty.raw").open("wb") as raw:
            while time.monotonic() < deadline:
                readable, _, _ = select.select([master], [], [], 0.25)
                if readable:
                    try:
                        chunk = os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO:
                            raise
                        chunk = b""
                    raw.write(chunk)
                    raw.flush()
                    # Answer bounded terminal probes; these are terminal replies,
                    # never prompt submissions or session identity guesses.
                    for query, reply in [(b"\x1b[6n", b"\x1b[1;1R"),
                                         (b"\x1b[c", b"\x1b[?1;2c"),
                                         (b"\x1b[>c", b"\x1b[>0;0;0c"),
                                         (b"\x1b[?u", b"\x1b[?0u")]:
                        if query in chunk:
                            os.write(master, reply)
                generations = rows(output/"data/pid-identity.db",
                                   "SELECT session_id FROM runtime_generation WHERE runtime_mode='pty_interactive' AND session_id IS NOT NULL")
                if generations and session is None:
                    if len(generations) != 1:
                        raise RuntimeError("Expected exactly one bound PTY generation")
                    session = generations[0]["session_id"]
                    rollout = locate(args.provider.absolute(), config, args.account, session)
                    print(f"PTY automatically bound native session: {session}", flush=True)
                if rollout:
                    records = transcript(rollout)
                    messages = [r["payload"] for r in records if r.get("type") == "response_item"
                                and r.get("payload", {}).get("type") == "message"]
                    receipts = [m for m in messages if m.get("role") == "assistant"
                                and any("PTY-RECEIVED" in part.get("text", "") and token in part.get("text", "")
                                        for part in m.get("content", []))]
                    notifications = [m for m in messages if m.get("role") == "user"
                                     and any("[OULIPOLY NOTIFICATIONS]" in p.get("text", "") for p in m.get("content", []))]
                    mailbox = rows(output/"data/pid-identity.db",
                                   "SELECT seq,handle,enqueued_at,delivered_at,delivery_attempts,delivery_error FROM mailbox WHERE session_id=?", (session,))
                    if receipts and notifications and mailbox and all(r["delivered_at"] for r in mailbox):
                        calls = [r["payload"] for r in records if r.get("type") == "response_item"
                                 and r.get("payload", {}).get("type") == "function_call"]
                        if len(calls) < 2 or any(c.get("namespace") != "mcp__agent_bash" or c.get("name") != "bash" for c in calls):
                            raise RuntimeError("PTY did not use the managed Bash override for child dispatch and receipt")
                        evidence = {**plan, "status":"passed", "session_id":session, "rollout":str(rollout),
                                    "mailbox":mailbox, "notification_user_turns":len(notifications),
                                    "assistant_receipts":len(receipts), "bash_calls":len(calls)}
                        if child_account != args.account:
                            children = rows(output/"data/pid-identity.db",
                                            "SELECT session_id,provider_name FROM runtime_generation WHERE runtime_mode='headless'")
                            if len(children) != 1 or children[0]["provider_name"] != child_account:
                                raise RuntimeError("Child did not use the requested distinct account")
                            child_session = children[0]["session_id"]
                            child_db = Path.home()/f".{child_account}"/"state_5.sqlite"
                            parent_db = Path.home()/f".{args.account}"/"state_5.sqlite"
                            child_native = rows(child_db, "SELECT id FROM threads WHERE id=?", (child_session,))
                            parent_native = rows(parent_db, "SELECT id FROM threads WHERE id=?", (child_session,))
                            if len(child_native) != 1 or parent_native:
                                raise RuntimeError("Child native SQLite identity leaked across account boundary")
                            evidence["child_session_id"] = child_session
                            evidence["native_sqlite_account_isolation"] = True
                        (output/"pty-result.json").write_text(json.dumps(evidence, indent=2)+"\n")
                        print(f"Managed Bash, automatic PTY binding and notification receipt passed: {output/'pty-result.json'}")
                        return
                ended, status = os.waitpid(pid, os.WNOHANG)
                if ended:
                    reaped = True
                    raise RuntimeError(f"PTY exited before receipt (wait status {status}); inspect {output}")
            raise RuntimeError(f"PTY notification test timed out; inspect {output}")
    finally:
        try:
            cleanup_workloads(output, runner, environment)
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            print(f"Test workload cleanup needs inspection at {output}: {error}", file=sys.stderr)
        if not reaped:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            stop = time.monotonic() + 5
            while time.monotonic() < stop:
                if os.waitpid(pid, os.WNOHANG)[0]:
                    reaped = True
                    break
                time.sleep(0.1)
            if not reaped:
                os.killpg(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
        os.close(master)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        raise SystemExit(str(error))
