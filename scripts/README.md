Build the release binary, then install the provider and its integrations:

```bash
cargo build --release
python3 scripts/install-provider.py
```

`install-provider.py` installs the existing build under
`~/.config/oulipoly-agent-runner/agent-runner-codex`, writes the six runtime
configuration paths, and creates `~/.local/bin/agent-runner-codex`. It discovers
Codex, Bun, Agent Bash, and Agent Runner from `PATH` and uses `~/ai/AGENTS.md`
as the system prompt. Before writing anything, it verifies that the installed
OpenCode Bash override exactly matches the vendored source and recorded SHA-256.
Previous artifacts, runtime config, and executable link are saved under the
runner configuration root's `backups/` directory.

The installation leaves model routing, Codex authentication, and global Codex
configuration unchanged. To test installation in isolated directories:

```bash
python3 scripts/install-provider.py \
  --config-root /tmp/codex-install/config \
  --install-root /tmp/codex-install/artifacts \
  --bin-dir /tmp/codex-install/bin
```

Use `--binary` for an alternate already-built artifact. Runtime paths and the
installed OpenCode override path also have explicit options; see `--help`.
The runtime configuration always lives at
`CONFIG_ROOT/agent-runner-codex/config.toml`, even with a separate artifact root.

`install-labels.py` stages the temporary Codex migration routes for review:

```bash
python3 scripts/install-labels.py --stage-root /tmp/codex-labels
```

The generated `models/` directory contains `codex-gpt-low`,
`codex-gpt-medium`, `codex-gpt-high`, `codex-gpt-xhigh`, and `codex-gpt-max`. Each selects `gpt-6-astra`, the corresponding native Codex
reasoning effort, and the five existing accounts `codex` through `codex5`.
Both headless and interactive model arguments are included. `providers.patch`
shows the five account implementation executable changes, canonical
`settings_id` fields, managed PTY commands, and `--resume` templates.
`models.patch` shows each proposed model file change.

After installing and validating the provider binary and its runtime config,
apply those changes with:

```bash
python3 scripts/install-labels.py --stage-root /tmp/codex-labels --apply
```

The default binary location is
`~/.config/oulipoly-agent-runner/agent-runner-codex/agent-runner-codex`.
`--config-root` and `--provider-path` select different installation locations.
The installer backs up `providers.toml` under the configuration root's
`backups/` directory and routes the five Codex accounts through the provider's
managed interactive launcher. It preserves account instructions and tool
restrictions. Existing different settings IDs are rejected for review. Before
activation, the binary must support both the requested model catalog and the
managed `interactive` launcher; an older headless-only provider cannot pass.
In the default temporary-label mode, existing OpenCode labels and the default
provider stay unchanged. A differing existing `codex-gpt-*` label causes an
error before installation.

To promote the standard `gpt-low`, `gpt-medium`, `gpt-high`, `gpt-xhigh`, and
`gpt-max` names to Codex Astra, stage and apply with the explicit standard-label
mode:

```bash
python3 scripts/install-labels.py --standard-labels \
  --stage-root /tmp/codex-standard-labels
python3 scripts/install-labels.py --standard-labels \
  --stage-root /tmp/codex-standard-labels --apply
```

Review `models.patch` before applying. This mode replaces those five existing
model files and saves their previous contents under `backups/codex-astra-*/models/`
alongside the provider configuration backup. It preserves the temporary
`codex-gpt-*` aliases, other model labels, and the configured default provider.
Before any mode applies routing changes, the installed provider must
advertise every target label with the exact selected model, reasoning arguments,
and all five eligible accounts. Install the updated provider first when
promoting the standard labels.

To register `gpt-luna-low`, `gpt-luna-medium`, `gpt-luna-high`, `gpt-luna-xhigh`,
and `gpt-luna-max` with Codex:

```bash
python3 scripts/install-labels.py --luna-labels --stage-root /tmp/codex-luna-labels
python3 scripts/install-labels.py --luna-labels --stage-root /tmp/codex-luna-labels --apply
```

This mode uses `gpt-5.6-luna` with the matching effort and all five Codex accounts.
It creates missing routes and saves replaced routes under
`backups/codex-luna-*/models/`.

To register `gpt-terra-low`, `gpt-terra-medium`, `gpt-terra-high`,
`gpt-terra-xhigh`, and `gpt-terra-max` with `gpt-5.6-terra`:

```bash
python3 scripts/install-labels.py --terra-labels --stage-root /tmp/codex-terra-labels
python3 scripts/install-labels.py --terra-labels --stage-root /tmp/codex-terra-labels --apply
```

Terra uses the same account pool and saves replaced routes under
`backups/codex-terra-*/models/`. Neither family includes `ultra`.
`--luna-labels`, `--terra-labels`, and `--standard-labels` are mutually exclusive;
apply the desired families separately after installing the updated provider and
managed catalog. Each mode requires every selected route to be advertised before
it changes any installed configuration, and preserves unrelated labels and the
configured default provider.

`benchmark-models/codex-exec-bench.toml` is generated separately and never
installed into production routing. It uses `gpt-5.6-luna` at low reasoning for
the repository's live-test requirement. Prepare an isolated runner, default
model directory, and state directory with:

```bash
python3 scripts/verify-live.py --account codex3
```

To spend two bounded model turns on the Bash and same-session resume checks,
add `--run`. Each run uses a fresh temporary directory; `--output-dir` can name
an explicitly selected empty directory. It copies the installed runner and
writes adjacent `config_home`/`data_dir` settings, so the runner's provider
registry and execution model directory share the same isolated configuration.
It also copies Agent Bash and writes its adjacent `agent-bash.toml` with the
isolated state directory and runner helper. That file controls helper
attestation; environment overrides alone do not isolate the installed spooler.
The selected native Codex account supplies its existing authentication. The
resume must recall a random token from the first turn and keep the same native
session ID. The verifier reads the native rollout and requires exactly one
completed Bash call per turn, each with `DONE rc=0` and its expected output;
model text alone cannot pass the check. Logs, tool evidence, and result JSON
stay in the printed verification directory.

For the PTY path, use `scripts/verify-pty-live.py --run --account codex3
--runner /path/to/runner --provider /path/to/agent-runner-codex`. This launches a
real runner PTY and a Luna/low child with isolated runner and spooler state.
It requires the Bash override to bind the native session automatically, then
checks the mailbox, the actual notification user turn, and an assistant receipt
that contains the child's result after reading its log. The test terminates its
own PTY after recording the evidence. Existing PTYs keep their startup tool
inventory; the updated routing takes effect on the next launch.
Pass `--mcp-bridge /path/to/installed/integrations/codex/agent-bash-mcp.ts`
to verify the installed bridge together with the installed executables.

Add `--child-account codex4` with `--account codex3` to force the child onto
another account. The verifier also requires the child's native thread to exist
in that account's SQLite database and to be absent from the parent's database.

Both live verifiers accept `--runner` and `--provider` to test worktree builds.
They remove only enclosing runner session capabilities from the inherited
environment; those identities cannot be reused in the isolated runner.

The files under `examples/` use a `binary` reference resolved through `PATH`;
the installer generates absolute `path` references to the selected installed
provider. No credentials are copied or created by this script.
