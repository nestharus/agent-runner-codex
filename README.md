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

`gpt-luna-{low,medium,high,xhigh,max}`, `gpt-terra-{low,medium,high,xhigh,max}`,
and `gpt-sol-{low,medium,high,xhigh,max}` use the same Codex adapter and five-account
pool with `gpt-5.6-luna`, `gpt-5.6-terra`, and `gpt-5.6-sol`, respectively. Each label selects its
corresponding native reasoning effort. No family registers `ultra`.

Quota probing uses the installed `~/.local/bin/chatgpt-usage` adapter against
each selected native auth file. Standalone authentication refresh is unsupported;
Codex retains its native token refresh during execution.

Native `server_overloaded` failures, including remote compaction failures, map to
`provider_unavailable` when the runner explicitly selects
`host.env.OULIPOLY_HOST_TERMINAL_UNAVAILABLE_V1=1`, as defined by the SDK-owned
[terminal-unavailable extension](contract/extensions/terminal-unavailable/README.md).
This preserves the native
failure separately from account quota exhaustion and rate limits. It does not
exhaust or rotate an account, or replay the failed turn. The exec JSON surface
only exposes Codex's normalized message, so the adapter does not infer the raw
HTTP status or whether Codex normalized a service overload or ramp-rate limit.
Older hosts receive `nonzero_exit` with fixed `codex.exec: server_overloaded`
evidence. Successful recovery, cancellation, and signals retain precedence.

The compatibility account IDs are `codex`, `codex2`, `codex3`, `codex4`, and
`codex5`. They select `~/.codex` through `~/.codex5`, respectively. The provider
sets `CODEX_HOME` explicitly even when launched from another Codex account.
The invocation environment is inherited; profile and Agent Bash bindings are
explicit additions. Credentials stay in their existing native account homes.

## Native execution and tools

The adapter is pinned to **Codex CLI 0.153.4**, using stable
`codex exec --json`, `codex exec resume`, and the interactive Codex CLI inside
Agent Runner's PTY. An unverified CLI version is rejected
before model execution. It does not depend on app-server dynamic tools.

A pinned model catalog removes metadata-forced native tools in addition to the
feature flags. Native inventory tests support every Astra, Luna, and Terra
effort and the Luna benchmark against a local Responses endpoint without spending
model tokens. Deterministic launch fixtures cover both invocation modes and all
five accounts; these checks do not establish live service availability.
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
per-invocation session file for headless launches. PTY launches use Codex's
`tools/call` metadata `_meta.threadId`; the unchanged Bash override reports that
identity through the runner's authenticated live-session handshake before
dispatching work. Neither path guesses the latest transcript.

The provider's `interactive` launcher applies the same managed tool inventory
and system instructions to PTY sessions. Account `system_prompt_override` is
read from `providers.toml` and passed as developer instructions. The runner
retains terminal rendering, input, process ownership, and notification delivery.
The Bash bridge preserves interactive delivery and cancellation behavior even
though MCP itself uses pipes. Sessionless launches default to `gpt-xhigh`;
model labels explicitly select their own model and effort.

Codex's interactive CLI does not support the exec-only user-configuration
isolation flags. Managed PTY launches therefore use a private configuration
home under the owning account's `agent-runner-managed/tui/`, with links to its
existing authentication, sessions, archived sessions, and thread writer locks.
Native SQLite stays with that account. Credentials are not copied, and token
refresh writes through to the native auth file. The small configuration homes
are retained because native SQLite records rollout paths through them.
Project configuration is excluded from managed launches. Managed PTY support
currently requires Unix; unsupported platforms are rejected explicitly.
The tested native account layout uses file authentication and account-local
SQLite. Custom keyring or storage layouts have not been verified.

## Lifecycle and sessions

### Opt-in MCP request control (Linux exec stdio only)

The provider CLI supports a separate same-user observer/controller, without
wrapping `agents`, intercepting Codex stdio, changing model routing, or modifying
the Bash adapter. This requires Linux `/proc`, glibc, Bun's FFI/SQLite support,
Unix `SO_PEERCRED`, and the runner's existing `pid-identity.db` identity table
(`os_pid`, `os_boot_id`, `os_pid_starttime_ticks`, `invocation_uuid`,
`provider_name`). Other platforms and unverified bindings fail closed.
Interactive TUI and shared app-server launches are not supported by this surface.

For one disposable launch, select a **nonexistent** directory immediately below
an existing canonical, non-symlink, same-user `0700` directory. The resulting
`DIR/control.sock` path must fit in 103 UTF-8 bytes. Set
`AGENT_RUNNER_CODEX_REQUEST_CONTROL_DIR=DIR` only in that launch's environment;
then invoke the unchanged canonical `agents -a ... -p ... -f ...` command.
Do not put this setting in global/model configuration or export it to a backlog
of launches. The bridge consumes it before Bash can launch children. A slot is
exclusive: an existing slot is rejected, never reused or automatically stolen.
With neither activation nor internal binding set, no control files or socket
are created and no identity database is read.

After the socket appears, a separately launched controller may run:

```sh
agent-runner-codex request-control observe DIR
agent-runner-codex request-control observe DIR --raw
agent-runner-codex request-control cancel DIR < selector.json
agent-runner-codex request-control cleanup DIR
```

For isolated configuration, place `--config-root ROOT` immediately after
`request-control`. No provider/model is executed by these commands. `observe`
emits JSON Lines: `attached` (outer launch/parent/process binding plus a snapshot
of active requests), `request`, `session`, `response`, `retired`, and `cancel`.
Select exactly the observed native `id` and its fresh `request_generation`:

```json
{"id":17,"request_generation":"<64-hex request generation from observation>"}
```

Send this object on the observer's stdin, or to a separate `cancel` process's
stdin. String and numeric IDs are distinct; numeric IDs must be safe integers.
An accepted cancellation means the matching active AbortController was aborted,
**not** that a workload has already stopped. Verify the response and retirement
events and, when needed, downstream Agent Bash evidence. Cancellation before
request registration, after retirement, for another generation, or a duplicate
returns `accepted:false`. A reused native ID gets a new request generation.
`cancel` exits 0 for accepted, 1 for not active/already aborted, and 2 for invalid
or unavailable control. Observer disconnect/overflow may lose observations;
there is no replay log. A later attachment snapshots current active requests
(including their original input with `--raw`), never completed requests or
previous results. Stop an observer with SIGINT/SIGTERM.

The `0700` slot contains only a `0600` socket and `0600` capability descriptor.
The descriptor stores launch/process identity and random launch auth/generation,
never prompts, commands, environment values, or tool replies. Treat this file as
a same-user control credential; do not copy it into reports. Both socket peers
check kernel UID, PID and boot/start-time identity. Each connection uses a fresh
challenge and monotonic sequence, and each operation must match the token,
launch generation, outer request, invocation and native request generation.
The bridge verifies its ancestry and the provider's exact runner PID-sidecar
row before publishing control, and rechecks process liveness during use.
This boundary does not defend against root or a compromised same-user account
that can read/modify private files, the runner identity database, or binaries.

Default observation contains metadata only. `--raw` explicitly authorizes live
arguments/replies for that attached connection; payloads over 256 KiB are omitted
with byte counts. Active input references live only until request retirement;
no payload history is retained, no credential is printed, and
slow clients are disconnected rather than blocking MCP (16 connections maximum,
1 MiB input/output buffers per connection; 5-second unauthenticated deadline).
Redirecting `--raw` output is an explicit controller-owned sensitive recording.
The observer CLI also disconnects on stdout backpressure instead of accumulating
an unbounded output queue. Native MCP stdin/stdout remain solely model-owned.

Normal completion, stdin close and SIGTERM retire the endpoint before the existing
bounded Bash cancellation grace; already-attached observers can receive final
responses during that grace. After SIGKILL or another uncatchable crash,
`cleanup DIR` removes only that private slot's known files, after proving the
recorded bridge is dead or recycled; it never signals any process. It refuses a
live bridge, unknown identity, changed ownership/permissions, or unexpected files.
An empty partial-startup slot can also be removed. The caller owns removal of
the pre-existing parent directory. Startup identity verification waits at most
five seconds for the runner's post-spawn identity publication.

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
python3 tests/test_install_labels.py --binary target/release/agent-runner-codex
python3 integrations/codex/test_mcp.py
python3 integrations/codex/test_request_control.py
bun test integrations/codex/request-control-unix.test.ts
python3 tests/verify_codex_inventory.py --binary target/release/agent-runner-codex
```

Install the release binary and `integrations/` tree with
`python3 scripts/install-provider.py`. The destination is
`~/.config/oulipoly-agent-runner/agent-runner-codex/`; its `config.toml` contains
absolute paths for `codex_bin`, `bun_bin`, `bash_mcp_path`, `system_prompt_file`,
`agent_bash_bin`, and `agent_runner_bin`.
Request control requires the matching provider binary and complete `integrations/`
tree from the same reviewed build, not a bridge-only or binary-only update. Its
offline end-to-end fixture uses `target/debug/agent-runner-codex` (run `cargo build`
before that test). Installed artifact equality and actual defined-operator
acceptance remain separate checks; the offline fixtures do not establish them.

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

To register all five Luna efforts with Codex, including the existing low/max
labels:

```sh
python3 scripts/install-labels.py --luna-labels --stage-root /tmp/agent-runner-luna-labels
python3 scripts/install-labels.py --luna-labels --stage-root /tmp/agent-runner-luna-labels --apply
```

To register the equivalent Terra family:

```sh
python3 scripts/install-labels.py --terra-labels --stage-root /tmp/agent-runner-terra-labels
python3 scripts/install-labels.py --terra-labels --stage-root /tmp/agent-runner-terra-labels --apply
```

To register the equivalent Sol family:

```sh
python3 scripts/install-labels.py --sol-labels --stage-root /tmp/agent-runner-sol-labels
python3 scripts/install-labels.py --sol-labels --stage-root /tmp/agent-runner-sol-labels --apply
```

Each mode backs up existing routes in its selected family and creates missing
efforts. The installer checks that the installed provider advertises all five
routes before applying them. Other model families, default routing, and benchmark
routes remain unchanged. Install the updated provider and managed catalog first.

`examples/benchmark-models/codex-exec-bench.toml` is a separately named
`gpt-5.6-luna`/`low` live-test route. Use it in isolated runner configuration; the
production label installer does not activate benchmark routes.
`python3 scripts/verify-live.py --account codex3 --run` checks a real Bash call
and same-session resume in an isolated runner. See [scripts/README.md](scripts/README.md)
for staging, backups, and verification options.

`python3 scripts/verify-pty-live.py --run --account codex3 --runner /path/to/runner
--provider /path/to/agent-runner-codex` tests a real runner PTY with a Luna/low
parent and child. It requires automatic session binding, actual managed Bash
calls, a notification user turn in the native transcript, and an assistant
receipt containing the child's result. A database delivery flag alone cannot
pass this check.

The label installer also routes the five accounts' interactive commands through
the provider launcher and sets the matching resume flag. Existing managed labels
with the older native model-selection arguments remain accepted. Installing an
updated provider and routing affects new PTY launches; an already-running Codex
session keeps the tool inventory with which it started.
