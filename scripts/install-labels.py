#!/usr/bin/env python3
"""Stage or apply the temporary Astra labels without rewriting other models."""

import argparse
from datetime import datetime, timezone
import difflib
import json
from pathlib import Path
import re
import shutil
import tomllib


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


def update_providers(original, provider_path):
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
        block_pattern = re.compile(
            rf"(?m)^\[{re.escape(account)}\.implementation\]\n(?:(?!^\[).|\n)*"
        )
        matches = list(block_pattern.finditer(updated))
        if len(matches) != 1:
            raise ValueError(f"Expected one implementation section for {account}")
        match = matches[0]
        replacement, count = re.subn(
            r'(?m)^executable\s*=.*$',
            f'executable = {json.dumps(str(provider_path))}',
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
    assert after == expected, "Unexpected provider configuration edit"
    assert before.keys() == after.keys()
    return updated


def atomic_write(path, text):
    pending = path.with_name(path.name + ".codex-migration-pending")
    pending.write_text(text)
    pending.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config-root", type=Path, default=Path.home()/".config/oulipoly-agent-runner")
    parser.add_argument("--stage-root", type=Path, required=True, help="Directory for reviewable labels and provider diff")
    parser.add_argument("--provider-path", type=Path, help="Installed provider binary; defaults to CONFIG_ROOT/agent-runner-codex/agent-runner-codex")
    parser.add_argument("--apply", action="store_true", help="Install after the Codex provider binary has been validated")
    args = parser.parse_args()
    provider_path = (args.provider_path or args.config_root/"agent-runner-codex/agent-runner-codex").expanduser().absolute()
    args.stage_root.mkdir(parents=True, exist_ok=True)
    models_stage = args.stage_root/"models"
    models_stage.mkdir(exist_ok=True)
    providers_file = args.config_root/"providers.toml"
    original = providers_file.read_text()
    proposed = update_providers(original, provider_path)
    (args.stage_root/"providers.toml.proposed").write_text(proposed)
    (args.stage_root/"providers.patch").write_text("".join(difflib.unified_diff(
        original.splitlines(keepends=True), proposed.splitlines(keepends=True),
        fromfile=str(providers_file), tofile=str(providers_file),
    )))
    models = {f"codex-gpt-{effort}.toml":model_text(effort, provider_path) for effort in EFFORTS}
    for name, text in models.items():
        parsed = tomllib.loads(text)
        assert [entry["name"] for entry in parsed["providers"]] == list(ACCOUNTS)
        (models_stage/name).write_text(text)
    benchmarks_stage = args.stage_root/"benchmark-models"
    benchmarks_stage.mkdir(exist_ok=True)
    (benchmarks_stage/"codex-exec-bench.toml").write_text(model_text("low", provider_path, "gpt-5.6-luna"))
    if args.apply:
        if not provider_path.is_file():
            raise SystemExit(f"Codex provider is not installed at {provider_path}")
        for name, text in models.items():
            destination = args.config_root/"models"/name
            if destination.exists() and destination.read_text() != text:
                raise SystemExit(f"Refusing to replace different existing label {destination}")
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
        backup = args.config_root/"backups"/f"codex-astra-{stamp}"
        backup.mkdir(parents=True)
        shutil.copy2(providers_file, backup/"providers.toml")
        atomic_write(providers_file, proposed)
        for name, text in models.items():
            atomic_write(args.config_root/"models"/name, text)
        print(f"Installed {len(models)} labels; provider configuration backup: {backup}")
    else:
        print(f"Staged {len(models)} labels and five Codex account settings/implementation changes at {args.stage_root}")


if __name__ == "__main__":
    main()
