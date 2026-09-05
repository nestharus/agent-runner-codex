#!/usr/bin/env python3
"""Prepare an isolated Luna smoke/resume run; --run explicitly enables model use."""

import argparse
import json
import os
from pathlib import Path
import re
import runpy
import shutil
import signal
import subprocess
import tempfile
import tomllib
import uuid


def isolated_environment(config_home, data_dir):
    environment = os.environ.copy()
    # These capabilities belong to the invoking session, not the isolated
    # runner. Inheriting them can report our new thread to the parent's socket.
    for name in ("OULIPOLY_PARENT_INVOCATION", "OULIPOLY_COMPLETION_REGISTRATION_AUTHORITY",
                 "OULIPOLY_LIVE_SESSION_BIND_SOCKET", "OULIPOLY_LIVE_SESSION_BIND_TOKEN"):
        environment.pop(name, None)
    environment.update({"OULIPOLY_CONFIG_HOME":str(config_home), "XDG_CONFIG_HOME":str(config_home),
                        "OULIPOLY_DATA_DIR":str(data_dir)})
    return environment


def selected_tables(text, account):
    headers = list(re.finditer(r"(?m)^\[([^]\n]+)\]\s*$", text))
    selected = []
    for i, header in enumerate(headers):
        if header.group(1) == account or header.group(1).startswith(account + "."):
            selected.append(text[header.start():headers[i + 1].start() if i + 1 < len(headers) else len(text)])
    result = "".join(selected)
    assert tomllib.loads(result) == {account: tomllib.loads(text)[account]}
    return result


def run_logged(command, environment, log, timeout):
    with log.open("w") as output:
        process = subprocess.Popen(command, env=environment, stdin=subprocess.DEVNULL,
                                   stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            raise RuntimeError(f"Live check timed out; inspect {log}")
    lines = log.read_text().splitlines()
    records = [json.loads(line.removeprefix("OULIPOLY_RESULT=")) for line in lines if line.startswith("OULIPOLY_RESULT=")]
    if code != 0 or not records or not records[-1].get("success"):
        raise RuntimeError(f"Live check failed; inspect {log}")
    result = records[-1]
    sessions = [json.loads(line.removeprefix("OULIPOLY_SESSION=")) for line in lines if line.startswith("OULIPOLY_SESSION=")]
    for session in sessions:
        if session.get("agent_runner_invocation_id", session.get("id")) == result.get("id"):
            result.update(session)
    return result


def verify_bash_calls(provider, config, account, session, markers):
    request = {"contract":"oulipoly.provider/v1", "request_id":f"verify-tools-{uuid.uuid4()}",
               "provider_instance_id":account, "host":{"app":"live-verifier", "config_root":str(config)},
               "params":{"settings_id":account, "session_id":session}}
    located = subprocess.run([str(provider), "session.locate_transcript"], input=json.dumps(request),
                             capture_output=True, text=True, timeout=30)
    response = json.loads(located.stdout)
    if located.returncode != 0 or not response.get("result", {}).get("located"):
        raise RuntimeError("Native rollout could not be located for tool verification")
    rollout = Path(response["result"]["path"])
    calls, results = [], {}
    for line in rollout.open():
        row = json.loads(line)
        payload = row.get("payload", {})
        if row.get("type") != "response_item" or not isinstance(payload, dict):
            continue
        if payload.get("type") == "function_call" and payload.get("name") == "bash" and payload.get("namespace") == "mcp__agent_bash":
            calls.append(payload)
        if payload.get("type") == "function_call_output":
            results[payload.get("call_id")] = payload.get("output")
    if len(calls) != len(markers):
        raise RuntimeError(f"Expected {len(markers)} actual Bash calls in the native rollout, found {len(calls)}")
    evidence = []
    for call, marker in zip(calls, markers):
        command = json.loads(call["arguments"]).get("command", "")
        output = results.get(call["call_id"])
        output_text = output if isinstance(output, str) else "\n".join(block.get("text", "") for block in (output or []))
        if marker not in command or "DONE rc=0" not in output_text or marker not in output_text:
            raise RuntimeError(f"Actual Bash call for {marker} did not complete successfully; inspect {rollout}")
        evidence.append({"call_id":call["call_id"], "command":command, "marker":marker, "exit_code":0})
    return {"rollout":str(rollout), "calls":evidence}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="Run two bounded live model turns")
    parser.add_argument("--account", choices=("codex", "codex2", "codex3", "codex4", "codex5"), default="codex3")
    parser.add_argument("--config-root", type=Path, default=Path.home()/".config/oulipoly-agent-runner")
    parser.add_argument("--runner", type=Path, help="Installed runner executable; defaults to CONFIG_ROOT/runner/oulipoly-agent-runner")
    parser.add_argument("--provider", type=Path, help="Provider executable to verify; defaults to the installed provider")
    parser.add_argument("--mcp-bridge", type=Path, help="Bash MCP bridge to verify; defaults to the installed runtime configuration")
    parser.add_argument("--output-dir", type=Path, help="Empty output directory; defaults to a new temporary directory")
    parser.add_argument("--timeout", type=int, default=180, help="Maximum seconds per live turn")
    args = parser.parse_args()
    output = args.output_dir.expanduser().absolute() if args.output_dir else Path(tempfile.mkdtemp(prefix="codex-live-"))
    output.mkdir(parents=True, exist_ok=True)
    if any(output.iterdir()):
        raise ValueError("Output directory must be empty to avoid reusing another test session")
    source = args.config_root.expanduser().absolute()
    runner_source = (args.runner or source/"runner/oulipoly-agent-runner").expanduser().absolute()
    runner = output/"runner/oulipoly-agent-runner"
    runner.parent.mkdir()
    shutil.copy2(runner_source, runner)
    config_home = output/"config"
    config = config_home/"oulipoly-agent-runner"
    (config/"models").mkdir(parents=True)
    (config/"agent-runner-codex").mkdir()
    (runner.parent/"config.toml").write_text(f'data_dir = {json.dumps(str(output/"data"))}\nconfig_home = {json.dumps(str(config_home))}\n')
    provider = (args.provider or source/"agent-runner-codex/agent-runner-codex").expanduser().absolute()
    labels = runpy.run_path(str(Path(__file__).with_name("install-labels.py")))
    providers = labels["update_providers"]((source/"providers.toml").read_text(), provider, config)
    (config/"providers.toml").write_text(selected_tables(providers, args.account))
    (config/"config.toml").write_text(f'default_provider = {json.dumps(args.account)}\ndiagnostics_model = "codex-exec-bench"\n')
    if (source/"sessions.toml").exists():
        shutil.copy2(source/"sessions.toml", config/"sessions.toml")
    label = labels["model_text"]("low", provider, "gpt-5.6-luna")
    sections = label.split("[[providers]]")
    label = sections[0] + "".join("[[providers]]" + section for section in sections[1:] if f'name = "{args.account}"' in section)
    (config/"models/codex-exec-bench.toml").write_text(label)
    runtime = tomllib.loads((source/"agent-runner-codex/config.toml").read_text())
    if args.mcp_bridge:
        runtime["bash_mcp_path"] = str(args.mcp_bridge.expanduser().absolute())
    spooler = output/"agent-bash/agent-bash"
    spooler.parent.mkdir()
    shutil.copy2(runtime["agent_bash_bin"], spooler)
    (spooler.parent/"agent-bash.toml").write_text(f'state_root = {json.dumps(str(output/"agent-bash-state"))}\nagent_runner_bin = {json.dumps(str(runner))}\n')
    runtime["agent_bash_bin"] = str(spooler)
    runtime["agent_runner_bin"] = str(runner)
    (config/"agent-runner-codex/config.toml").write_text("".join(f'{key} = {json.dumps(value)}\n' for key, value in runtime.items()))
    token = str(uuid.uuid4())
    prompt = output/"prompt.txt"
    prompt.write_text(f"This is a bounded integration smoke test. Remember verification token {token} for the next turn. Use the available Agent Bash MCP tool exactly once with command `printf 'codex-bash-ok\\n'`. Then reply exactly `CODEX-SMOKE-OK codex-bash-ok`. Do not inspect files or launch child agents.\n")
    resume_prompt = output/"resume-prompt.txt"
    resume_prompt.write_text("This continues the same bounded test. Use the available Agent Bash MCP tool exactly once with command `printf 'codex-resume-ok\\n'`. Then reply `CODEX-RESUME-OK codex-resume-ok` followed by the verification token I gave in the previous turn. Do not inspect files or launch child agents.\n")
    plan = {"model":"gpt-5.6-luna", "effort":"low", "account":args.account, "runner":str(runner), "config_root":str(config), "data_dir":str(output/"data"), "verification_token":token}
    (output/"plan.json").write_text(json.dumps(plan, indent=2)+"\n")
    print(f"Isolated verification directory: {output}", flush=True)
    if not args.run:
        print("Prepared only. Pass --run with a fresh output directory to perform the two live turns.")
        return
    environment = isolated_environment(config_home, output/"data")
    first = run_logged([str(runner), "--pin-provider", args.account, "-m", "codex-exec-bench", "-p", str(output), "-f", str(prompt)], environment, output/"smoke.log", args.timeout)
    session = first.get("provider_session_id")
    if not session or "CODEX-SMOKE-OK codex-bash-ok" not in (output/"smoke.log").read_text():
        raise RuntimeError("Initial turn did not return its expected response and native session ID")
    (output/"initial-result.json").write_text(json.dumps(first, indent=2)+"\n")
    first_tools = verify_bash_calls(provider, config, args.account, session, ["codex-bash-ok"])
    (output/"initial-tools.json").write_text(json.dumps(first_tools, indent=2)+"\n")
    print(f"Initial turn passed; native session: {session}", flush=True)
    second = run_logged([str(runner), "resume", "--session-id", session, "-m", "codex-exec-bench", "-p", str(output), "-f", str(resume_prompt)], environment, output/"resume.log", args.timeout)
    response = (output/"resume.log").read_text()
    if second.get("provider_session_id") != session or "CODEX-RESUME-OK codex-resume-ok" not in response or token not in response:
        raise RuntimeError("Resume did not preserve the native session and verification token")
    tools = verify_bash_calls(provider, config, args.account, session, ["codex-bash-ok", "codex-resume-ok"])
    (output/"result.json").write_text(json.dumps({"smoke":first,"resume":second,"tools":tools,"status":"passed"}, indent=2)+"\n")
    print(f"Smoke and resume passed. Results: {output/'result.json'}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError) as error:
        raise SystemExit(str(error))
