#!/usr/bin/env python3
"""Stage or apply Codex routes for Astra, Luna, Terra, or Sol labels."""

import argparse
from datetime import datetime, timezone
import difflib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tomllib
import uuid


ACCOUNTS = ("codex", "codex2", "codex3", "codex4", "codex5")
EFFORTS = ("low", "medium", "high", "xhigh", "max")


def model_text(effort, provider_path, model="gpt-6-astra"):
    args = json.dumps(["-m", model, "-c", f'model_reasoning_effort="{effort}"'])
    header = f'''# Temporary Codex route for the OpenCode migration.
provider = {{ path = {json.dumps(str(provider_path))} }}

[[inputs]]
default_input = true
description = "The text prompt"
name = "prompt"
required = true
type = "string"
'''
    return header + "".join(
        f'\n[[providers]]\nargs = {args}\ninteractive_args = {args}\nname = "{account}"\n'
        for account in ACCOUNTS
    )


def update_providers(original, provider_path, config_root):
    before = tomllib.loads(original)
    updated = original
    for account in ACCOUNTS:
        existing_settings = before[account].get("settings_id")
        if existing_settings is not None and existing_settings != account:
            raise ValueError(f"Refusing to replace noncanonical settings_id for {account}")
        if existing_settings is None:
            header = f"[{account}]\n"
            if updated.count(header) != 1:
                raise ValueError(f"Expected one account section for {account}")
            updated = updated.replace(header, header + f'settings_id = "{account}"\n', 1)
        # The runner owns the PTY, while the provider owns Codex's native tool
        # and instruction setup. Headless calls still use the JSON endpoint.
        account_pattern = re.compile(rf"(?m)^\[{re.escape(account)}\]\n(?:(?!^\[).|\n)*")
        account_match = account_pattern.search(updated)
        if account_match is None:
            raise ValueError(f"Expected one account section for {account}")
        account_block = account_match.group(0)
        for key, value in {
            "command": json.dumps(str(provider_path)) if any(c.isspace() for c in str(provider_path)) else str(provider_path),
            "interactive_args": ["interactive", "--settings-id", account,
                                 "--config-root", str(config_root)],
        }.items():
            pattern = rf'(?m)^{key}\s*=.*$'
            replacement = f'{key} = {json.dumps(value)}'
            if re.search(pattern, account_block):
                account_block = re.sub(pattern, lambda _: replacement, account_block)
            else:
                account_block += replacement + "\n"
        updated = updated[:account_match.start()] + account_block + updated[account_match.end():]
        resume_pattern = re.compile(rf"(?m)^\[{re.escape(account)}\.resume\]\n(?:(?!^\[).|\n)*")
        resume_block = f'[{account}.resume]\nkind = "flag"\nflag = "--resume"\n\n'
        resume_match = resume_pattern.search(updated)
        if resume_match:
            updated = updated[:resume_match.start()] + resume_block + updated[resume_match.end():]
        else:
            updated += "\n" + resume_block
        block_pattern = re.compile(
            rf"(?m)^\[{re.escape(account)}\.implementation\]\n(?:(?!^\[).|\n)*"
        )
        matches = list(block_pattern.finditer(updated))
        if len(matches) != 1:
            raise ValueError(f"Expected one implementation section for {account}")
        match = matches[0]
        replacement, count = re.subn(
            r'(?m)^executable\s*=.*$',
            lambda _: f'executable = {json.dumps(str(provider_path))}',
            match.group(0),
        )
        if count != 1:
            raise ValueError(f"Expected one executable for {account}")
        updated = updated[:match.start()] + replacement + updated[match.end():]
    after = tomllib.loads(updated)
    expected = tomllib.loads(original)
    for account in ACCOUNTS:
        expected[account]["settings_id"] = account
        expected[account]["implementation"]["executable"] = str(provider_path)
        expected[account]["command"] = json.dumps(str(provider_path)) if any(c.isspace() for c in str(provider_path)) else str(provider_path)
        expected[account]["interactive_args"] = ["interactive", "--settings-id", account,
                                                "--config-root", str(config_root)]
        expected[account]["resume"] = {"kind": "flag", "flag": "--resume"}
    assert after == expected, "Unexpected provider configuration edit"
    assert before.keys() == after.keys()
    return updated


def atomic_write(path, text):
    if path.exists() and path.read_bytes() == text.encode():
        return
    pending = path.with_name(path.name + ".codex-migration-pending")
    pending.write_text(text)
    pending.replace(path)


def verify_provider_models(provider_path, config_root, models):
    request_id = f"label-installer-{uuid.uuid4()}"
    request = {
        "contract": "oulipoly.provider/v1",
        "request_id": request_id,
        "host": {"app": "label-installer", "config_root": str(config_root.absolute())},
        "params": {},
    }
    try:
        completed = subprocess.run(
            [str(provider_path), "discovery.models"], input=json.dumps(request),
            capture_output=True, text=True, timeout=15,
        )
        response = json.loads(completed.stdout)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit("Installed provider model discovery failed; install and validate the current provider before applying labels") from error
    if (completed.returncode != 0 or not isinstance(response, dict)
            or response.get("contract") != request["contract"]
            or response.get("request_id") != request_id or response.get("ok") is not True):
        raise SystemExit("Installed provider did not return successful model discovery")
    result = response.get("result")
    catalog = result.get("models") if isinstance(result, dict) else None
    if not isinstance(catalog, list):
        raise SystemExit("Installed provider returned an invalid model catalog")
    for filename, text in models.items():
        name = Path(filename).stem
        matches = [entry for entry in catalog if isinstance(entry, dict) and entry.get("name") == name]
        expected = tomllib.loads(text)["providers"]
        if len(matches) != 1:
            raise SystemExit(f"Installed provider must advertise exactly one {name} route before applying labels")
        entry = matches[0]
        accounts = entry.get("eligible_accounts")
        if (entry.get("provider_model") != expected[0]["args"][1]
                or any(entry.get("provider_args") != provider["args"]
                       or entry.get("provider_args") != provider["interactive_args"] for provider in expected)
                or not isinstance(accounts, list)
                or any(provider["name"] not in accounts for provider in expected)):
            raise SystemExit(f"Installed provider's {name} model, arguments, or eligible accounts do not match the proposed label")


def verify_interactive_launcher(provider_path):
    try:
        result = subprocess.run([str(provider_path), "interactive", "--help"], input="",
                                capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.SubprocessError) as error:
        raise SystemExit("Managed Codex PTY launcher is unavailable; install the current provider first") from error
    if result.returncode != 0 or not all(token in result.stdout for token in
                                        ("agent-runner-codex interactive", "--settings-id", "--resume")):
        raise SystemExit("Managed Codex PTY launcher is unavailable; install the current provider first")


def staged_model_text(existing, proposed):
    """Retain equivalent installed route bytes, including comments/newlines."""
    if not existing.exists():
        return proposed
    original = existing.read_bytes().decode("utf-8")
    if tomllib.loads(original) == tomllib.loads(proposed):
        return original
    return proposed


def selected_effort(effort, standard_labels):
    if standard_labels and effort in ("high", "xhigh", "max"):
        return "medium"
    return effort


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config-root", type=Path, default=Path.home()/".config/oulipoly-agent-runner")
    parser.add_argument("--stage-root", type=Path, required=True, help="Directory for reviewable labels and provider diff")
    parser.add_argument("--provider-path", type=Path, help="Installed provider binary; defaults to CONFIG_ROOT/agent-runner-codex/agent-runner-codex")
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument("--standard-labels", action="store_true", help="Promote gpt-low/medium/high/xhigh/max to Codex Astra, backing up and replacing existing labels")
    selection.add_argument("--luna-labels", action="store_true", help="Register gpt-luna-low/medium/high/xhigh/max with Codex Luna, backing up and replacing existing labels")
    selection.add_argument("--terra-labels", action="store_true", help="Register gpt-terra-low/medium/high/xhigh/max with Codex Terra, backing up and replacing existing labels")
    selection.add_argument("--sol-labels", action="store_true", help="Register gpt-sol-low/medium/high/xhigh/max with Codex Sol, backing up and replacing existing labels")
    parser.add_argument("--apply", action="store_true", help="Install after the Codex provider binary has been validated")
    args = parser.parse_args()
    provider_path = (args.provider_path or args.config_root/"agent-runner-codex/agent-runner-codex").expanduser().absolute()
    args.stage_root.mkdir(parents=True, exist_ok=True)
    models_stage = args.stage_root/"models"
    models_stage.mkdir(exist_ok=True)
    providers_file = args.config_root/"providers.toml"
    original = providers_file.read_text()
    proposed = update_providers(original, provider_path, args.config_root.expanduser().absolute())
    (args.stage_root/"providers.toml.proposed").write_text(proposed)
    (args.stage_root/"providers.patch").write_text("".join(difflib.unified_diff(
        original.splitlines(keepends=True), proposed.splitlines(keepends=True),
        fromfile=str(providers_file), tofile=str(providers_file),
    )))
    family = "luna" if args.luna_labels else "terra" if args.terra_labels else "sol" if args.sol_labels else "astra"
    prefix = f"gpt-{family}" if family != "astra" else "gpt" if args.standard_labels else "codex-gpt"
    model = f"gpt-5.6-{family}" if family != "astra" else "gpt-6-astra"
    models = {f"{prefix}-{effort}.toml":model_text(selected_effort(effort, args.standard_labels), provider_path, model) for effort in EFFORTS}
    models = {name: staged_model_text(args.config_root/"models"/name, text)
              for name, text in models.items()}
    model_diffs = []
    for name, text in models.items():
        parsed = tomllib.loads(text)
        assert [entry["name"] for entry in parsed["providers"]] == list(ACCOUNTS)
        (models_stage/name).write_text(text)
        existing = args.config_root/"models"/name
        old_text = existing.read_text() if existing.exists() else ""
        model_diffs.extend(difflib.unified_diff(
            old_text.splitlines(keepends=True), text.splitlines(keepends=True),
            fromfile=str(existing) if existing.exists() else "/dev/null", tofile=str(existing),
        ))
    (args.stage_root/"models.patch").write_text("".join(model_diffs))
    benchmarks_stage = args.stage_root/"benchmark-models"
    benchmarks_stage.mkdir(exist_ok=True)
    (benchmarks_stage/"codex-exec-bench.toml").write_text(model_text("low", provider_path, "gpt-5.6-luna"))
    if args.apply:
        if not provider_path.is_file():
            raise SystemExit(f"Codex provider is not installed at {provider_path}")
        for name, text in models.items():
            destination = args.config_root/"models"/name
            if not (args.standard_labels or args.luna_labels or args.terra_labels or args.sol_labels) and destination.exists() and destination.read_text() != text:
                raise SystemExit(f"Refusing to replace different existing label {destination}")
        verify_provider_models(provider_path, args.config_root, models)
        verify_interactive_launcher(provider_path)
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
        backup = args.config_root/"backups"/f"codex-{family}-{stamp}"
        backup.mkdir(parents=True)
        shutil.copy2(providers_file, backup/"providers.toml")
        (backup/"models").mkdir()
        for name, text in models.items():
            existing = args.config_root/"models"/name
            if existing.exists() and existing.read_bytes() != text.encode():
                shutil.copy2(existing, backup/"models"/name)
        atomic_write(providers_file, proposed)
        for name, text in models.items():
            atomic_write(args.config_root/"models"/name, text)
        print(f"Installed {len(models)} labels; provider configuration backup: {backup}")
    else:
        print(f"Staged {len(models)} labels and five Codex account settings, implementation, and PTY routes at {args.stage_root}")


if __name__ == "__main__":
    main()
