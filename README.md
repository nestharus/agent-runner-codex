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

Standard labels select `gpt-6-astra`: `gpt-low` uses low effort, while
`gpt-medium`, `gpt-high`, `gpt-xhigh`, and `gpt-max` all use medium effort.
The existing `codex-gpt-{low,medium,high,xhigh,max}` compatibility labels retain
their corresponding **native** efforts; the last three are not equivalent to
the standard aliases. Use `codex-gpt-high`, `codex-gpt-xhigh`, or `codex-gpt-max`
when explicitly requesting those native Astra efforts. No `gpt-astra-*` labels
are registered. Native sub-agent delegation remains disabled for every label.

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
model tokens. Standard high/xhigh/max aliases are checked at medium; the
compatibility labels still exercise native high/xhigh/max. Deterministic launch fixtures cover both invocation modes and all
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
though MCP itself uses pipes. Sessionless managed PTY launches default to
`gpt-xhigh`, hence Astra/medium.
Explicit model labels select their catalog model and effort. Exact native
`-m gpt-6-astra -c 'model_reasoning_effort="xhigh"'` (or high/max) PTY
arguments retain that native effort via the compatibility catalog.

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

Launch events provide sequential stdout/stderr, heartbeat, thread identity,
submitted-turn, assistant-response, and terminal records. A completed native turn
is required before a successful exit. Hosts selecting `launch_output_v1` receive
an output-completion receipt with SHA-256 digests, byte counts, and event counts
before the terminal record, including cancellation and failed turns. Requests are locked and journaled; exact
completed retries replay without another model turn. Reusing a request ID with
changed inputs fails. Interrupted requests require reconciliation, preventing an
ambiguous invocation from being silently submitted twice. Native effects begin
only after durable process-group identity publication; cleanup retains process
group ownership on errors, cancellation, and host deadlines. CLI launch output
uses nonblocking writes with a two-second no-progress limit, so an open host
pipe that stops consuming cannot indefinitely delay native-group cleanup.
Delivery failure exits nonzero and does not append another response to a partial
event. Incomplete requests retain their journal and require reconciliation;
they do not claim a completed output receipt or silently execute again. A failed
delivery of an already-completed replay leaves its durable receipt unchanged.

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
python3 tests/test_cli_install.py --binary target/release/agent-runner-codex
python3 integrations/codex/test_mcp.py
python3 tests/verify_codex_inventory.py --binary target/release/agent-runner-codex
```

Install the release binary and `integrations/` tree with
`python3 scripts/install-provider.py`. The destination is
`~/.config/oulipoly-agent-runner/agent-runner-codex/`; its `config.toml` contains
absolute paths for `codex_bin`, `bun_bin`, `bash_mcp_path`, `system_prompt_file`,
`agent_bash_bin`, and `agent_runner_bin`. The provider `--version` command does
not read an envelope or wait for stdin; the installer also probes with stdin
explicitly disconnected.

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

Standard-label staging includes all five routes. Applying backs up and replaces
only changed route files; already-equivalent low/medium routes keep their exact
bytes, including comments and formatting. On an existing standard Astra setup,
only high/xhigh/max effort arguments change to medium. Both label families use
the same Codex accounts, system prompt, and Bash tool configuration. The installed
provider must advertise the new arguments before activation; stale standard
high/xhigh/max argument pairs are rejected, not silently accepted or rewritten.

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
the provider launcher and sets the matching resume flag. Exact managed native
model/effort pairs remain accepted by the PTY launcher. Headless admission
requires arguments matching the selected label; migrate stale standard aliases
with `--standard-labels` after installing the updated provider. Installing an
updated provider and routing affects new PTY launches; an already-running Codex
session keeps the tool inventory with which it started.

### Bounded JSONL record continuation (AGE-343)

Session-turn paging keeps the caller's existing source/response/inline/turn
budgets, including for old `codex-stp1-` continuations. It can stage a partial
record across source quanta rather than requiring a whole record to fit the
remaining quantum. No record is skipped based on an unparsed type prefix.
Projection, turn IDs (original record byte offsets), sequence and body digests
are computed only after a complete JSONL record is parsed. Oversized projected
bodies retain the existing `omitted_oversize` metadata/digest contract.

Resource envelope:

- Native source reads, including identity metadata, stay within the requested
  source quota (at most 8 MiB; Runner currently supplies 1 MiB). Identity scanning
  uses a one-byte buffer to avoid uncharged read-ahead: at most the source quota
  in header-read calls. This deliberately trades header syscall overhead for
  exact accounting. Native record reads remain chunked.
- A newline-complete record may contain at most 8,388,608 bytes. A prefix without
  a newline at that ceiling returns `session_turn_record_ceiling_exceeded`,
  never completion or a silent skip. Metadata that cannot leave any work budget
  and insufficient response metadata capacity use
  `session_turn_page_budget_too_small`. Hosts must stop unchanged-input retries
  for these deterministic limits; raising budgets invalidates old page tokens.
- Framing assembles at most 8 MiB of record/page bytes per call. Immutable staging
  reads are separate from **native** source accounting: at most one prefix
  below 8 MiB, plus at most one same-sized collision-validation read; cursor
  JSON is limited to 32 KiB per frame; packed cursor lookup streams a hash bucket
  under the storage-budget bound described below. Prefix staging writes less than
  8 MiB per call.
  Buffer allocation capacity may exceed logical byte length; JSON/projection
  allocations are additionally proportional to this bounded record. This is
  not a measured or allocator-enforced whole-process RSS cap.
- At most 256 turns, 524,288 response bytes and 65,536 inline body bytes remain
  supported; JSON parsing retains serde_json's depth limit. Work is bounded by
  these limits, the paging-store budget, and directory discovery limits (100,000 rollouts,
  400,000 entries), not the entire transcript length. Staging can reread/hash a
  prefix on each quantum; total catch-up work is not claimed to be single-pass.

Staging lives beside private provider-owned paging cursors as immutable,
content-addressed `record-<sha256>.part` files, published and synchronized before
any referring cursor. Tokens bind the staging digest, record start, account,
provider, settings, session, projection, nonce, budgets and existing file
identity checks. Interrupted calls may leave unreferenced staging files; do not
manually remove staging while a retained cursor may reference it. There is no
new automatic garbage collection or replay eviction. New allocations are bounded
by the paging-state admission limits below; already-retained content, including
content above those limits, is preserved and charged to admission.

At incomplete EOF, snapshot completion means all currently frozen bytes were
examined, **not** that the partial record was projected or that the live session
has stopped. The resume token retains that prefix; append supplies its suffix.
Neither this result nor `last_success_at` proves current native/model-context
coverage. Existing inode/truncation/mtime checks remain in force; same-inode
rewrites combined with file growth are not newly content-attested.

Upgrade accepts legacy cursors without staging fields. Downgrade compatibility
is not symmetric: an older provider rejects new cursors that contain a staged
prefix. A binary-only rollback after checkpoint advancement is therefore unsafe;
retain the compatible reader and its provider-state with the checkpoint, or
obtain a separately reviewed rollback/reconciliation plan. Never reset a native
session, rewrite native records, or manually advance a production cursor to
bypass this boundary. Canonical catch-up does not reconstruct native model
context or settle a native UI/history complaint.

### Paging-state admission, packed cursors (AGE-353), and containment

The canonical `host.data_root/provider-state/codex/session-pages-v1` scope
(or the existing default data root) has a **512 MiB accounted storage budget**.
Each file charges its logical length rounded up to 4 KiB, plus a 4 KiB object
allowance; empty files charge 4 KiB. The corresponding maximum object count is
131,072 (512 MiB / 4 KiB), not an independent allowance for that many large files.
This replaces AGE-343's independent 4,096-object ceiling: an inherited store of
4,464 ~503-byte cursors charges about 35 MiB, rather than being unserviceable
solely because of its file count. These are accounting units, not a universal
filesystem allocation, metadata, provider-wide disk, or process RSS guarantee.
Actual filesystem failures remain distinct I/O errors; quota refusal returns
fixed `session_turn_staging_capacity_exceeded`, never a completed page.

New cursors keep the same opaque hash tokens and JSON contents but append to
256 fixed hash-prefix buckets (`cursors-xx.pack`), instead of allocating one
inode per token. Each frame is `sha256`, a space, serialized cursor JSON, and a
newline. Reads stream bounded frames (32 KiB cursor plus framing), checking each
complete frame's digest; they scan the selected bucket, not all retained cursor
contents. Worst-case bucket scan is bounded by the storage budget, not the
native-source quantum. Bucketing is not a guarantee of uniform hash distribution
or constant lookup latency. Repeated observations consume retained history bytes
but at most 256 new cursor objects. Prefix files still consume individual charged
objects. There is no expiry, replay eviction, or reference-based GC: finite retained
history can eventually exhaust the budget and requires an explicit operator decision.
No infinite-history serviceability is claimed.

A directory-inode advisory lock serializes complete paging requests across
threads, processes, and aliases. Admission rescans actual files with constant
accumulation memory; unexpected non-file entries fail closed. Never replace or
unlink that directory while serving requests. No native-history lock is added.
New files and pack growth reserve their full incremental charge before writing.
Prefix files retain synchronized temporary-file/rename publication. Pack appends
sync the file and directory before returning a token. After interruption only an
incomplete final frame (which could not have supplied an issued token) is truncated
and synced before a replacement append. Complete frames and legacy files are never
removed, including unreferenced frames. Complete corrupt frames fail closed rather
than being silently repaired. Temporary/orphan files remain charged on restart.
Exact byte-validated dedup works even above limits; packed dedup resynchronizes
publication, including a complete frame left by a writer that exited before fsync.
Per-record/source/response limits and cursor identity/budget/generation checks remain
unchanged. Legacy partial-record dependencies are retained and resolved unchanged.

**Upgrade and rollback:** no migration, relocation, cleanup, or DB-reference scan
is needed. Leave every legacy hash-named JSON and `record-*.part` file in place.
New readers accept both legacy and packed tokens. Old binaries can still read
legacy tokens, but cannot read newly packed tokens and still enforce the obsolete
object cap. Before enabling the new writer, stop/drain all old paging binaries
sharing this data root, including in-flight provider operations; switch every
invocation route to the new reader/writer. Cooperating new processes can run
concurrently immediately, through the existing directory lock. Mixed old/new
paging service is **not supported**: unchanged legacy readability is not permission
to hand new tokens to old readers. Once any packed token is issued, rollback to an
old-only reader would violate replay obligations. Use a forward correction (or
containment that retains this reader); do not delete packs or revert to an old
binary as recovery. Root must verify cutover/fleet readiness and the actual stalled
observation/delivery separately; fixture replay is not receiver evidence.

A separate ticket-prefixed containment commit changes only the compile-time
paging switch. It returns `session_turn_paging_paused` before any paging read,
state-directory preparation, staging or cursor mutation. It pauses canonical and
user-observation **v1 paging**; legacy non-paging reads and unrelated provider
operations remain unchanged. It is not an old-parser downgrade or full ingestion
restoration. Forward restoration uses the same state and checkpoint. Matching
Runner code keeps fixed capacity/pause terminals stopped across routine
re-enqueue/import; a separately authorized caller must explicitly rearm the exact
terminal/generation after resolving its cause. Neither code nor tests authorize
installation, rearming production state, or containment activation.
