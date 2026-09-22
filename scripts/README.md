Build the release binary, then install the provider and its integrations:

```bash
cargo build --release
python3 scripts/install-provider.py
```

`install-provider.py` installs the existing build under
`~/.config/oulipoly-agent-runner/agent-runner-codex`, writes the six runtime
configuration paths, and creates `~/.local/bin/agent-runner-codex`. It discovers
Codex, Bun, Agent Bash, and Agent Runner from `PATH` and uses `~/ai/AGENTS.md`
as the system prompt. Before writing anything, it verifies the vendored Codex
Bash against the pinned stable source manifest, checks all integration assets
against `--integration-hashes` from the selected binary, and refuses to
overwrite an installed Codex Bash with an unrecognized digest. It does not
inspect `~/.config/opencode`. Previous artifacts, runtime config, and
executable link are saved under the runner configuration root's `backups/`
directory. Individual replacements are atomic; a failed replacement or readback
attempts to restore that backup. There is no single atomic transaction spanning
the binary, integration directory, config, and link.

The installation leaves model routing, Codex authentication, and global Codex
configuration unchanged. To test installation in isolated directories:

```bash
python3 scripts/install-provider.py \
  --config-root /tmp/codex-install/config \
  --install-root /tmp/codex-install/artifacts \
  --bin-dir /tmp/codex-install/bin
```

Use `--binary` for an alternate already-built artifact. The selected binary
must embed the exact prospective integration assets. Runtime paths have
explicit options; see `--help`. An older installer cannot enforce this guard,
so a rollback must use this release's installer and a reviewed matching bundle.
The runtime configuration always lives at
`CONFIG_ROOT/agent-runner-codex/config.toml`, even with a separate artifact root.

`install-labels.py` stages the temporary Codex migration routes for review:

```bash
python3 scripts/install-labels.py --stage-root /tmp/codex-labels
```

The generated `models/` directory contains `gpt-astra-low`,
`gpt-astra-medium`, `gpt-astra-high`, `gpt-astra-xhigh`, and `gpt-astra-max`.
Each selects `gpt-6-astra`, the corresponding native Codex
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
In the default named-Astra mode, existing standard labels and the default
provider stay unchanged. A differing existing `gpt-astra-*` label causes an
error before installation; use `--astra-labels` to stage its reviewed replacement
with the same backup behavior as the other explicit family modes.

To promote the standard `gpt-low`, `gpt-medium`, `gpt-high`, `gpt-xhigh`, and
`gpt-max` names to Codex Sol, stage and apply with the explicit standard-label
mode:

```bash
python3 scripts/install-labels.py --standard-labels \
  --stage-root /tmp/codex-standard-labels
python3 scripts/install-labels.py --standard-labels \
  --stage-root /tmp/codex-standard-labels --apply
```

Review `models.patch` before applying. Every standard label selects `gpt-6-sol`
at the matching native effort, including high, xhigh, and max. The managed
no-model PTY default remains `gpt-xhigh`, now Sol/xhigh. The preserved
`gpt-astra-*` routes select Astra at matching efforts; exact native PTY argument
pairs remain supported.

This mode stages all five model files, but equivalent existing Sol routes retain
their bytes, including comments and formatting. Only changed files are replaced
and their previous contents saved under `backups/codex-sol-*/models/`
alongside the provider configuration backup. It preserves the named
`gpt-astra-*` aliases, other model labels, and the configured default provider.
Before any mode applies routing changes, the installed provider must
advertise every target label with the exact selected model, reasoning arguments,
and all five eligible accounts. Install the updated provider first when
promoting the standard labels. Stale high/xhigh/max route arguments are rejected
by strict headless admission; stale Astra routes at any effort are also rejected.
Update the source-backed routes with this mode
rather than adding arbitrary argument overrides.

To explicitly replace and back up existing `gpt-astra-low`, `gpt-astra-medium`,
`gpt-astra-high`, `gpt-astra-xhigh`, and `gpt-astra-max` routes:

```bash
python3 scripts/install-labels.py --astra-labels --stage-root /tmp/codex-astra-labels
python3 scripts/install-labels.py --astra-labels --stage-root /tmp/codex-astra-labels --apply
```

To register `gpt-luna-low`, `gpt-luna-medium`, `gpt-luna-high`, `gpt-luna-xhigh`,
and `gpt-luna-max` with Codex:

```bash
python3 scripts/install-labels.py --luna-labels --stage-root /tmp/codex-luna-labels
python3 scripts/install-labels.py --luna-labels --stage-root /tmp/codex-luna-labels --apply
```

This mode uses `gpt-6-luna` with the matching effort and all five Codex accounts.
It creates missing routes and saves replaced routes under
`backups/codex-luna-*/models/`.

To register `gpt-terra-low`, `gpt-terra-medium`, `gpt-terra-high`,
`gpt-terra-xhigh`, and `gpt-terra-max` with `gpt-5.6-terra`:

```bash
python3 scripts/install-labels.py --terra-labels --stage-root /tmp/codex-terra-labels
python3 scripts/install-labels.py --terra-labels --stage-root /tmp/codex-terra-labels --apply
```

Terra uses the same account pool and saves replaced routes under
`backups/codex-terra-*/models/`.

To register `gpt-sol-low`, `gpt-sol-medium`, `gpt-sol-high`, `gpt-sol-xhigh`,
and `gpt-sol-max` with `gpt-6-sol`:

```bash
python3 scripts/install-labels.py --sol-labels --stage-root /tmp/codex-sol-labels
python3 scripts/install-labels.py --sol-labels --stage-root /tmp/codex-sol-labels --apply
```

Sol uses the same account pool and saves replaced routes under
`backups/codex-sol-*/models/`. No family includes `ultra`.
`--astra-labels`, `--luna-labels`, `--terra-labels`, `--sol-labels`, and
`--standard-labels` are mutually exclusive;
apply the desired families separately after installing the updated provider and
managed catalog. Each mode requires every selected route to be advertised before
it changes any installed configuration, and preserves unrelated labels and the
configured default provider.

`benchmark-models/codex-exec-bench.toml` is generated separately and never
installed into production routing. It uses `gpt-6-luna` at low reasoning for
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

## Offline verification evidence

`tests/verify_codex_inventory.py --binary /exact/provider --label gpt-high`
checks the managed headless native request against a loopback Responses server.
Repeat with `gpt-xhigh`, `gpt-luna-low`, `codex-exec-bench`, `gpt-max`, and
`gpt-astra-xhigh` to cover Sol, Luna, the benchmark, and Astra routes at their
matching native efforts.
Use `--output-dir /new/evidence/path` to retain isolated configs, request/response
bodies and raw provider results.

The native TUI inventory producer requires explicit private dependencies:

```sh
python3 tests/verify_codex_tui_inventory.py \
  --boundary-report /private/harness/boundary.json --binary /private/provider \
  --native-codex /private/native-codex --bun /private/bun \
  --no-model --output-dir /tmp/short-new-path
```

Run it only inside an externally established private user/network/mount/PID
namespace with production HOME, /root, /run and /tmp masked; use env-i,
private HOME/XDG/CODEX state, read-only candidate dependencies, dropped DAC
readsearch/override capabilities and PID1 reaping. Probe the boundary before
bringing up private loopback. Loopback is not a sandbox; do not use installed
production Codex or Runner, and do not manufacture a boundary report. The
separate headless inventory producer has ambient lookups and does not acquire
this isolation by virtue of the TUI changes. Neither check proves service
availability or effective policy outside the observed launch.

The TUI producer provisions a fake spooler, a fail-closed fake runner, a
synthetic system prompt and a non-inherited environment. It checks the native
request's model (Sol and Luna now use GPT-6), effort, tools, instructions,
exact rollout identity and native session binding. For the tool result, it
requires the exact supervised `printf inventory-tool-call` dispatch before
bounded snapshot acquisition, the same bound owner on every fake-spooler call,
the `ab_test` handle through observation and receipt, later progression, and
the response body. The fake spooler returns fixed bytes, so this checks the
recorded command-to-output relationship but cannot prove shell execution or
durable storage. Session binding acknowledges identity, not consumption of the
tool body. Remote acknowledgement and physical drain remain unconfirmed. Use
a new, short output directory for its Unix socket.

`python3 tests/test_tui_inventory_oracle.py -v` runs five pure in-memory checks
for the fixed-output oracle, including command, order, handle, and owner-drift
negative controls. It starts no native host and does not replace the private
synthetic suite or native inventory run.

The synthetic suite also needs that *externally established* boundary. Inside
it, set `CODEX_INVENTORY_TEST_BUN` to an absolute private Bun executable and
`CODEX_INVENTORY_TEST_PRIVATE_ROOT` to a fresh short private harness root with
an actual `boundary.json` probe report. It must record true `masks`,
`read_only_inputs` and `DAC_denied`, and the current user/net/mnt/pid identities
from `/proc/self/ns`. The four producer failure controls compare namespace
identities and create unique `failure-cases/` directories without reusing them.
The suite does not create or independently prove this boundary. Then run:

```sh
python3 tests/test_tui_inventory_producer.py -v
```

Six synthetic bridge/oracle tests check retained output, malformed/order/hash
controls, identity ACK and receipt failures. Four producer PTY/HTTP failure
controls use fake provider and native executables to check request retention,
early exits and validation failure; neither set tests actual native Codex. A
Bun-only host run or a partial six-test selection is not the ten-test gate.
Do not treat an unexecuted suite as a pass; retain the gap until an externally
established private boundary and fake-provider setup are actually available.

Set `CODEX_TEST_EVIDENCE_DIR` to a fresh absolute directory to retain fake launch
and interactive fixture configs/native argv plus installer fixtures/backups.
The fake Rust fixtures retain only their named configuration and call files,
not the large lifecycle journals; complete runner output is captured separately.
