#!/usr/bin/env python3
"""Install an already-built Codex provider without changing model routes or auth."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib


REPOSITORY = Path(__file__).resolve().parents[1]


def absolute(path):
    return path.expanduser().absolute()


def executable(name, override=None):
    found = str(override) if override else shutil.which(name)
    if not found:
        raise ValueError(f"Required executable is unavailable: {name}")
    path = absolute(Path(found))
    if not path.is_file() or not os.access(path, os.X_OK):
        raise ValueError(f"Required executable is not executable: {path}")
    return path


def validate_bash(installed):
    vendored = REPOSITORY / "integrations/opencode/tools/bash.ts"
    source = REPOSITORY / "integrations/opencode/BASH_SOURCE.json"
    expected = json.loads(source.read_text())["sha256"]
    original = installed.read_bytes()
    copied = vendored.read_bytes()
    if original != copied or hashlib.sha256(copied).hexdigest() != expected:
        raise ValueError("Installed OpenCode Bash differs from the repository vendor or its provenance hash; synchronize and review the source before installing")
    return expected


def config_text(config):
    text = "".join(f"{key} = {json.dumps(str(value))}\n" for key, value in config.items())
    assert tomllib.loads(text) == {key: str(value) for key, value in config.items()}
    return text


def copy_backup(path, destination):
    if path.is_symlink():
        destination.symlink_to(os.readlink(path))
    elif path.is_dir():
        shutil.copytree(path, destination, symlinks=True)
    elif path.exists():
        shutil.copy2(path, destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=REPOSITORY / "target/release/agent-runner-codex")
    parser.add_argument("--config-root", type=Path, default=Path.home() / ".config/oulipoly-agent-runner")
    parser.add_argument("--install-root", type=Path, help="Defaults to CONFIG_ROOT/agent-runner-codex")
    parser.add_argument("--bin-dir", type=Path, default=Path.home() / ".local/bin", help="Directory for the agent-runner-codex symlink")
    parser.add_argument("--opencode-bash", type=Path, default=Path.home() / ".config/opencode/tools/bash.ts")
    parser.add_argument("--system-prompt-file", type=Path, default=Path.home() / "ai/AGENTS.md")
    parser.add_argument("--codex-bin", type=Path)
    parser.add_argument("--bun-bin", type=Path)
    parser.add_argument("--agent-bash-bin", type=Path)
    parser.add_argument("--agent-runner-bin", type=Path)
    args = parser.parse_args()

    config_root = absolute(args.config_root)
    install_root = absolute(args.install_root or config_root / "agent-runner-codex")
    runtime_root = config_root / "agent-runner-codex"
    bin_dir = absolute(args.bin_dir)
    binary = executable("agent-runner-codex", args.binary)
    installed_bash = absolute(args.opencode_bash)
    bash_sha = validate_bash(installed_bash)
    version = subprocess.run([str(binary), "--version"], capture_output=True, text=True, timeout=10)
    if version.returncode != 0 or not version.stdout.startswith("agent-runner-codex "):
        raise ValueError("The build artifact did not identify itself as agent-runner-codex")
    prompt = absolute(args.system_prompt_file)
    prompt_bytes = prompt.read_bytes()
    if not prompt_bytes or len(prompt_bytes) > 1024 * 1024:
        raise ValueError("System prompt must be nonempty and at most 1 MiB")
    prompt_bytes.decode("utf-8")
    config = {
        "codex_bin": executable("codex", args.codex_bin),
        "bun_bin": executable("bun", args.bun_bin),
        "bash_mcp_path": install_root / "integrations/codex/agent-bash-mcp.ts",
        "system_prompt_file": prompt,
        "agent_bash_bin": executable("agent-bash", args.agent_bash_bin).resolve(),
        "agent_runner_bin": executable("oulipoly-agent-runner", args.agent_runner_bin).resolve(),
    }
    text = config_text(config)
    target_binary = install_root / "agent-runner-codex"
    target_integrations = install_root / "integrations"
    target_config = runtime_root / "config.toml"
    target_link = bin_dir / "agent-runner-codex"
    if target_link.exists() and target_link.is_dir():
        raise ValueError(f"Refusing to replace a directory at {target_link}")

    install_root.mkdir(parents=True, exist_ok=True)
    runtime_root.mkdir(parents=True, exist_ok=True)
    bin_dir.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".install-", dir=install_root) as temporary:
        staging = Path(temporary)
        shutil.copy2(binary, staging / "agent-runner-codex")
        (staging / "agent-runner-codex").chmod(0o755)
        shutil.copytree(REPOSITORY / "integrations", staging / "integrations", symlinks=True)
        (staging / "config.toml").write_text(text)
        if validate_bash(installed_bash) != bash_sha:
            raise ValueError("OpenCode Bash changed during installation")
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
        backup = config_root / "backups" / f"codex-provider-{stamp}"
        backup.mkdir(parents=True)
        for path, name in [(target_binary, "agent-runner-codex"), (target_integrations, "integrations"), (target_config, "config.toml"), (target_link, "bin-link")]:
            copy_backup(path, backup / name)
        (backup / "manifest.json").write_text(json.dumps({"binary": str(target_binary), "integrations": str(target_integrations), "config": str(target_config), "symlink": str(target_link)}, indent=2) + "\n")
        (staging / "agent-runner-codex").replace(target_binary)
        if target_integrations.exists() or target_integrations.is_symlink():
            target_integrations.rename(staging / "previous-integrations")
        (staging / "integrations").rename(target_integrations)
        # The config root may be on another filesystem from the artifact root.
        with tempfile.NamedTemporaryFile(mode="w", prefix=".config-", dir=runtime_root, delete=False) as pending:
            pending.write(text)
            pending_config = Path(pending.name)
        pending_config.replace(target_config)
        with tempfile.TemporaryDirectory(prefix=".codex-link-", dir=bin_dir) as link_stage:
            pending_link = Path(link_stage) / "agent-runner-codex"
            pending_link.symlink_to(target_binary)
            pending_link.replace(target_link)
    print(f"Installed {version.stdout.strip()} at {target_binary}")
    print(f"Runtime config: {target_config}")
    print(f"OpenCode Bash SHA-256: {bash_sha}")
    print(f"Previous installation backup: {backup}")
    print("Model labels remain staged; use install-labels.py after verification.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit(str(error))
