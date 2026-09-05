# agent-runner-codex

Temporary Codex provider for Agent Runner's `oulipoly.provider/v1` contract.
The repository starts with a complete copy of `agent-runner-opencode`; the exact
source revision is recorded in `OPENCODE_BASELINE` and the baseline Git commit.
`OPENCODE_CONTRACT_REVISION` records the newer shared contract snapshot used by
the installed runner.
The active adapter retains the shared envelope, contract schemas, encoding,
terminal classification, durable filesystem helpers, and native process gate.
Codex-specific launch and rollout handling replace OpenCode's native boundaries.

## Models and accounts

`gpt-low`, `gpt-medium`, `gpt-high`, `gpt-xhigh`, and `gpt-max` select
`gpt-6-astra` with the corresponding native reasoning effort. The temporary
`codex-gpt-*` names remain equivalent aliases. Native sub-agent delegation
remains disabled for every label.

Quota probing uses the installed `~/.local/bin/chatgpt-usage` adapter against
each selected native auth file. Standalone authentication refresh is unsupported;
Codex retains its native token refresh during execution.

The compatibility account IDs are `codex`, `codex2`, `codex3`, `codex4`, and
`codex5`. They select `~/.codex` through `~/.codex5`, respectively. The provider
sets `CODEX_HOME` explicitly even when launched from another Codex account.
The invocation environment is inherited; profile and Agent Bash bindings are
explicit additions. Credentials stay in their existing native account homes.

## Native execution and tools

The adapter is pinned to **Codex CLI 0.153.4**, using stable
`codex exec --json` and `codex exec resume`. An unverified CLI version is rejected
before model execution. It does not depend on app-server dynamic tools.

A pinned model catalog removes metadata-forced native tools in addition to the
feature flags. Native inventory tests verify Astra efforts and the Luna
benchmark against a local Responses endpoint without spending model tokens.
The remaining built-in tools only request user input or inspect MCP resources;
Agent Bash is the sole execution tool.

The system instruction source is `~/ai/AGENTS.md`, passed through
`model_instructions_file`. Provider-specific extra instructions use the developer
instruction channel. Managed launches bypass user configuration, disable native
shell execution and sub-agents, and expose the MCP `agent_bash.bash` tool. Child
agents use the normal `agents` command through Bash, preserving Agent Runner's
contract rather than translating Codex native sub-agent handles.

`integrations/opencode/tools/bash.ts` is the **unchanged, byte-identical**
OpenCode Bash override. The Codex MCP adapter imports its actual implementation;
it does not maintain a second shell implementation. The provenance manifest and
integration tests record the exact source and installed-byte comparison. MCP
receives inherited environment variable names via `env_vars`, avoiding values in
command arguments. The native thread ID is handed to the tool through a private
per-invocation session file.

## Lifecycle and sessions

Launch events provide sequential stdout/stderr, heartbeat, thread identity,
submitted-turn, assistant-response, and terminal records. A completed native turn
is required before a successful exit. Hosts selecting `launch_output_v1` receive
an output-completion receipt with SHA-256 digests, byte counts, and event counts
before the terminal record, including cancellation and failed turns. Requests are locked and journaled; exact
completed retries replay without another model turn. Reusing a request ID with
changed inputs fails. Interrupted requests require reconciliation, preventing an
ambiguous invocation from being silently submitted twice. Native effects begin
only after durable process-group identity publication; cleanup retains process
group ownership on errors, cancellation, and host deadlines.

Session capture, lookup, reads, and enumeration understand native Codex JSONL
rollouts, including archived sessions, fork metadata, partial trailing writes,
and account-isolated transcript identity. Concurrent resumes of one session are
serialized. A new Codex thread receives its ID from Codex.
The negotiated `session_turn_pages_v1` protocol provides bounded native transcript
pages and account-bound opaque cursors for the runner's completion and resume
checks.

OpenCode session import, cross-account session replacement/rotation, arbitrary
provider settings CRUD, and the shared app-server experiment are not implemented
by this temporary adapter. They are not advertised as available capabilities.
Native Codex sessions can be resumed within their owning account. The copied
OpenCode-only implementation remains in the baseline Git history, rather than
being exposed as a Codex capability with different semantics.

## Build and install

```sh
cargo test
cargo build --release
python3 integrations/codex/test_mcp.py
python3 tests/verify_codex_inventory.py --binary target/release/agent-runner-codex
```

Install the release binary and `integrations/` tree with
`python3 scripts/install-provider.py`. The destination is
`~/.config/oulipoly-agent-runner/agent-runner-codex/`; its `config.toml` contains
absolute paths for `codex_bin`, `bun_bin`, `bash_mcp_path`, `system_prompt_file`,
`agent_bash_bin`, and `agent_runner_bin`.

Stage and activate the temporary labels after installing and validating the
provider:

```sh
python3 scripts/install-labels.py --stage-root /tmp/agent-runner-codex-labels
python3 scripts/install-labels.py --stage-root /tmp/agent-runner-codex-labels --apply
agents -m codex-gpt-high -p /path/to/project 'Your task'
```

The default label installer preserves existing standard labels and backs up the
provider configuration before changing Codex implementation paths and assigning
canonical account settings IDs. To move the five standard `gpt-*` labels to
Codex Astra after validating the provider, stage and apply the promotion:

```sh
python3 scripts/install-labels.py --standard-labels --stage-root /tmp/agent-runner-astra-labels
python3 scripts/install-labels.py --standard-labels --stage-root /tmp/agent-runner-astra-labels --apply
agents -m gpt-high -p /path/to/project 'Your task'
```

Promotion backs up the five previous route files. Both label families use the
same Codex accounts, system prompt, and Bash tool configuration.

`examples/benchmark-models/codex-exec-bench.toml` is a separately named
`gpt-5.6-luna`/`low` live-test route. Use it in isolated runner configuration; the
production label installer does not activate benchmark routes.
`python3 scripts/verify-live.py --account codex3 --run` checks a real Bash call
and same-session resume in an isolated runner. See [scripts/README.md](scripts/README.md)
for staging, backups, and verification options.
