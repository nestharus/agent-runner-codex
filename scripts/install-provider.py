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
SOURCE_COMMIT = "dea893c984a944aec7e6b277a9adafec2882954d"
REQUIRED_BASH_SHA256 = "64e82c7a8677122155d7e6a9955fa87dd8d31cc491b8d922a178b250c2e47bc8"
PREVIOUS_BASH_SHA256 = "23dbb0dfd555e3ac720659e5b22a1fe0119c5ebe13102b09231ab47e4fd42c2d"
ASSETS = (
    "codex/agent-bash-mcp.ts",
    "codex/session-registration.ts",
    "codex/opencode-tool-shim.ts",
    "codex/models.json",
    "opencode/tools/bash.ts",
)


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


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def bundle_file(root, name):
    if root.is_symlink():
        raise ValueError("Codex integration root is linked")
    path = root
    for component in Path(name).parts:
        path = path / component
        if path.is_symlink():
            raise ValueError(f"Codex bundle contains linked path: {name}")
    if not path.is_file():
        raise ValueError(f"Codex bundle file is missing: {name}")
    return path


def validate_bundle(root):
    source = bundle_file(root, "opencode/BASH_SOURCE.json")
    manifest = json.loads(source.read_text())
    expected = {
        "source_repository": "agent-bash-tool",
        "source_branch": "main",
        "source_commit": SOURCE_COMMIT,
        "source_path": "integrations/opencode/tools/bash.ts",
        "sha256": REQUIRED_BASH_SHA256,
        "verified_date": "2026-09-22",
    }
    if manifest != expected:
        raise ValueError("Codex Bash provenance manifest does not identify the pinned stable source")
    hashes = {}
    for name in ASSETS:
        path = bundle_file(root, name)
        hashes[name] = sha256(path)
    if hashes["opencode/tools/bash.ts"] != REQUIRED_BASH_SHA256:
        raise ValueError("Codex Bash differs from its pinned provenance hash")
    return hashes


def embedded_hashes(binary):
    result = subprocess.run([str(binary), "--integration-hashes"], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=10)
    if result.returncode != 0:
        raise ValueError("Provider binary cannot report its embedded Codex integration")
    try:
        identity = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ValueError("Provider binary returned invalid integration identity") from error
    if not isinstance(identity, dict) or identity.get("schema") != 1 or not isinstance(identity.get("assets"), dict):
        raise ValueError("Provider binary returned unsupported integration identity")
    return identity["assets"]


def validate_existing_bash(target_integrations):
    if not target_integrations.exists() and not target_integrations.is_symlink():
        return
    if target_integrations.is_symlink() or not target_integrations.is_dir():
        raise ValueError("Existing Codex integrations are not a directory")
    for parent in (target_integrations / "opencode", target_integrations / "opencode/tools"):
        if parent.is_symlink():
            raise ValueError("Existing Codex Bash has a linked parent; review before replacing it")
    installed = target_integrations / "opencode/tools/bash.ts"
    if installed.is_symlink() or not installed.is_file():
        raise ValueError("Existing Codex Bash is missing or linked; review before replacing it")
    digest = sha256(installed)
    if digest not in (PREVIOUS_BASH_SHA256, REQUIRED_BASH_SHA256):
        raise ValueError(f"Existing Codex Bash has unrecognized SHA-256 {digest}; review before replacing it")


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


def remove_path(path):
    if path.is_symlink() or path.is_file():
        path.unlink()
    elif path.exists():
        shutil.rmtree(path)


def restore_backup(backup, targets, present):
    for name, path in targets:
        remove_path(path)
        if present[name]:
            copy_backup(backup / name, path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=REPOSITORY / "target/release/agent-runner-codex")
    parser.add_argument("--config-root", type=Path, default=Path.home() / ".config/oulipoly-agent-runner")
    parser.add_argument("--install-root", type=Path, help="Defaults to CONFIG_ROOT/agent-runner-codex")
    parser.add_argument("--bin-dir", type=Path, default=Path.home() / ".local/bin", help="Directory for the agent-runner-codex symlink")
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
    bundle_hashes = validate_bundle(REPOSITORY / "integrations")
    binary_sha = sha256(binary)
    version = subprocess.run([str(binary), "--version"], stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=10)
    if version.returncode != 0 or not version.stdout.startswith("agent-runner-codex "):
        raise ValueError("The build artifact did not identify itself as agent-runner-codex")
    if embedded_hashes(binary) != bundle_hashes:
        raise ValueError("Provider binary embedded integration differs from the prospective Codex bundle")
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
    validate_existing_bash(target_integrations)
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
        if sha256(staging / "agent-runner-codex") != binary_sha or embedded_hashes(staging / "agent-runner-codex") != bundle_hashes:
            raise ValueError("Provider binary changed while staging")
        if validate_bundle(staging / "integrations") != bundle_hashes:
            raise ValueError("Codex integration changed while staging")
        validate_existing_bash(target_integrations)
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
        backup = config_root / "backups" / f"codex-provider-{stamp}"
        backup.mkdir(parents=True)
        targets = [("agent-runner-codex", target_binary), ("integrations", target_integrations),
                   ("config.toml", target_config), ("bin-link", target_link)]
        present = {name: path.exists() or path.is_symlink() for name, path in targets}
        for name, path in targets:
            copy_backup(path, backup / name)
        (backup / "manifest.json").write_text(json.dumps({"binary": str(target_binary), "integrations": str(target_integrations), "config": str(target_config), "symlink": str(target_link)}, indent=2) + "\n")
        try:
            (staging / "agent-runner-codex").replace(target_binary)
            if target_integrations.exists():
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
            if (sha256(target_binary) != binary_sha or
                    embedded_hashes(target_binary) != bundle_hashes or
                    validate_bundle(target_integrations) != bundle_hashes or
                    target_config.read_text() != text or
                    not target_link.is_symlink() or os.readlink(target_link) != str(target_binary)):
                raise ValueError("Installed Codex package failed readback")
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            try:
                restore_backup(backup, targets, present)
            except OSError as rollback_error:
                raise ValueError(f"Codex package installation failed ({error}); rollback failed ({rollback_error}); backup: {backup}") from rollback_error
            raise ValueError(f"Codex package installation failed ({error}); previous files restored from {backup}") from error
    print(f"Installed {version.stdout.strip()} at {target_binary}")
    print(f"Runtime config: {target_config}")
    print(f"Codex Bash SHA-256: {REQUIRED_BASH_SHA256}")
    print(f"Previous installation backup: {backup}")
    print("Model labels remain staged; use install-labels.py after verification.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        raise SystemExit(str(error))
