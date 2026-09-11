# agent-runner-codex

Temporary Codex provider for Agent Runner's `oulipoly.provider/v1` contract.
The repository starts with a complete copy of `agent-runner-opencode`; the exact
source revision is recorded in `OPENCODE_BASELINE` and the baseline Git commit.
`OPENCODE_CONTRACT_REVISION` records the imported OpenCode contract revision,
not an attestation of the currently installed Runner. `contract/v1/UPSTREAM.md`
identifies the historical shared snapshot and the local observation extension.
The active adapter retains the shared envelope, contract lineage, encoding,
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
`tools/call` metadata `_meta.threadId`. PTY identity registration does not require
Bash: provider-owned global **SessionStart** and **UserPromptSubmit** command
hooks synchronously send exact native session/cwd metadata through the runner's
authenticated capture/bind handshake. Capture must validate native rollout
materialization, identity, invocation and workspace before acknowledgement. The
Bash bridge also requires this acknowledgement before any tool operation.
Neither path guesses the latest transcript.

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

### Interactive registration admission and limits

Start managed PTY sessions through Agent Runner; a direct provider launch without
its authenticated binding environment is rejected. Each launch stages the running
provider binary's embedded registration helper, MCP bridge, shim, unchanged Bash
implementation and catalog into its own private generation. Missing/stale default
installation integration assets are repaired there, **not** rewritten in place.
Concurrent launches and already-running generations retain their own bytes. Custom
integration paths are never repaired: they must match this release or fail before
TUI exec with an explicit `scripts/install-provider.py --binary ...` remedy.
External executable dependencies must exist, be executable, and not be writable
by other users. No downloads, native upgrades, global backups, project trust edits,
or broad hook-trust bypass are performed.

The selected generated `CODEX_HOME/config.toml` declares both synchronous hooks
and their exact path-qualified native declaration trust hashes. These hashes cover
normalized declarations, **not helper bytes**; release-payload staging provides
the separate content check. Only TUI hooks are enabled; unrelated disabled native
features and headless exec's hooks-disabled policy remain unchanged. Existing
system/managed hooks remain subject to native policy and are explicitly permitted.
The original account's ordinary user configuration is still excluded by the
existing isolated-profile policy; this does not import or trust arbitrary user hooks.

Before TUI exec, a bounded disposable native **stdio app-server** performs only
`initialize`, `config/read` and `configRequirements/read` (no thread, tools or model).
Admission rejects known managed-only hooks requirements, incompatible feature
or provider-owned catalog/storage/auth requirements, displaced instruction paths,
disabled required features, malformed responses or timeout. This is
a native configuration snapshot, not an exclusive inventory or a guarantee against
managed policy changes before TUI load. The app-server is not used as the model
runtime or a new shared-runtime optimization. Its transport is explicitly stdio
and native ephemeral remote-control startup is disabled. **Config-only requests
do not make native startup read-only:** the pinned server initializes its configured
SQLite store and may perform native corruption recovery before answering. This
extra initialization against account storage is **rejected for delivery**: managed
requirements can redirect the store before validation, and the 15-second probe can
interrupt native backfill with a 900-second lease. The current implementation is
not corrected or delivery-ready. The configuration-only library experiment in
`tests/native/test_config_only.py` is not a production replacement: it omits cloud
auth/transport and uses fixture policy paths. A complete replacement remains an
unresolved native capability/integration decision. No production state was tested.

SessionStart runs on the **first submitted turn**, not on opening an empty TUI.
UserPromptSubmit repeats the exact check because native consumes SessionStart even
when it stops a turn: a later submission must not bypass failed registration. The
runner retains its first successful binding, acknowledges authenticated exact
duplicates without recapture, and rejects reassignment. This provider requires the
companion Runner cached-acknowledgement change; deliver the two updates together. Starting a different native
thread requires a new Runner invocation. Delayed rollout visibility receives
bounded retries with unchanged capture validation. Successful callbacks produce
empty stdout; helper failures request native structured `continue:false` stops.
Native command crashes/timeouts can still be advisory, so this is **not** a universal
guarantee that model execution stops. Runner missing-identity failure and receipt
fencing remain final authority. Never-submitted empty TUI exit remains an explicit
no-bind outcome, not resumable success; no SessionEnd hook is added. Detached-root
custody is a separate lifecycle concern.

Offline verification:

```sh
python3 tests/test_session_registration.py --binary target/debug/agent-runner-codex --bun /absolute/path/to/bun
```

The helper-only suite does not prove native discovery. `tests/native/registration_probe.rs`
is a separate no-model loader/hook-engine harness against public upstream commit
`3d2ee51ca2d5db578f328aa75e20aa22c0197c9a` (`rust-v0.153.4`). Compile it against those
unmodified crates and supply `--native-probe /absolute/path/to/age356-native-probe`
to run actual global/untrusted-project discovery, declaration trust, feature-off,
managed-only/system-hook, structured-stop and helper-failure fixtures. See
`tests/native/README.md` for the isolated build recipe and evidence boundaries.

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

Canonical session-turn paging keeps the caller's existing source/response/inline/turn
budgets, including for old `codex-stp1-` continuations. The observation-only
resource change below supersedes this native/staging accounting for that projection. It can stage a partial
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

The canonical `host.data_root/provider-state/codex/session-pages-v1` scope (or
the existing default data root) has a **512 MiB accounted storage budget**. Each
file charges its logical length rounded up to 4 KiB, plus a 4 KiB object
allowance; empty files charge 4 KiB. The corresponding maximum object count is
131,072 (512 MiB / 4 KiB), not an independent allowance for that many large
files. This replaces AGE-343's independent 4,096-object ceiling: an inherited
store of 4,464 ~503-byte cursors charges about 35 MiB, rather than being
unserviceable solely because of its file count. These are accounting units, not
a universal filesystem allocation, metadata, provider-wide disk, or process RSS
guarantee. Actual filesystem failures remain distinct I/O errors; quota refusal
returns fixed `session_turn_staging_capacity_exceeded`, never a completed page.

New canonical cursors keep the same opaque hash tokens and JSON contents but
append to 256 fixed hash-prefix buckets (`cursors-xx.pack`), instead of
allocating one inode per token. Each frame is `sha256`, a space, serialized
cursor JSON, and a newline. Reads stream bounded frames (32 KiB cursor plus
framing), checking each complete frame's digest; they scan the selected bucket,
not all retained cursor contents. Worst-case bucket scan is bounded by the
storage budget, not the native-source quantum. Bucketing is not a guarantee of
uniform hash distribution or constant lookup latency. Repeated canonical pages
consume retained history bytes but at most 256 new cursor objects. New
observation pages use the separate source-backed strategy below, not this
retained pool. Prefix files still consume individual charged objects. There is
no expiry, replay eviction, or reference-based GC: finite retained history can
eventually exhaust the budget and requires an explicit operator decision. No
infinite-history serviceability is claimed.

A directory-inode advisory lock serializes complete canonical paging requests
and legacy/packed observation-token loads across threads, processes, and
aliases. Source-backed observation requests do not acquire canonical admission.
Admission rescans actual files with constant accumulation memory; unexpected
non-file entries fail closed. Never replace or unlink that directory while
serving requests. No native-history lock is added. New files and pack growth
reserve their full incremental charge before writing. Prefix files retain
synchronized temporary-file/rename publication. Pack appends sync the file and
directory before returning a token. After interruption only an incomplete final
frame is truncated and synced before a replacement append. Under cooperating
process interruption that suffix has not supplied an issued token; arbitrary
disk corruption (such as loss of an issued frame newline) is not covered by that
crash-recovery guarantee. Complete frames and legacy files are never removed,
including unreferenced frames. Complete corrupt frames fail closed rather than
being silently repaired. Temporary/orphan files remain charged on restart. Exact
byte-validated dedup works even above limits; packed dedup resynchronizes
publication, including a complete frame left by a writer that exited before
fsync. Per-record/source/response limits and cursor identity/budget/generation
checks remain unchanged. Legacy partial-record dependencies are retained and
resolved unchanged.

**Upgrade and rollback:** no migration, relocation, cleanup, or DB-reference
scan is needed. Leave every legacy hash-named JSON and `record-*.part` file in
place. New readers accept legacy JSON and packed `codex-stp1-` tokens, plus
source-backed `codex-obs1-` observation tokens. Preserve the sibling observation
authentication key as well as all partial-prefix dependencies. Old binaries can
still read legacy tokens, but cannot read newly packed tokens and still enforce
the obsolete object cap. Before enabling the new writer, stop/drain all old
paging binaries sharing this data root, including in-flight provider operations;
switch every canonical and observation invocation route to a reader/writer
supporting all three representations and a host accepting the observation
accounting extension. Cooperating new processes can run concurrently
immediately, through the existing directory lock. Mixed old/new paging service
is **not supported**: unchanged legacy readability is not permission to hand new
tokens to old readers. Once any packed or source-backed token is issued,
rollback to an incompatible reader would violate replay obligations. Use a
forward correction (or containment that retains this reader); do not delete
packs or revert to an old binary as recovery. Root must verify cutover/fleet
readiness and the actual stalled observation/delivery separately; fixture replay
is not receiver evidence.

The compile-time paging switch supports reader-preserving containment; no
separately verified compatible containment binary is established here. When
disabled, it returns `session_turn_paging_paused` before any paging read, state-
directory preparation, staging or cursor mutation. It pauses canonical and user-
observation **v1 paging**; legacy non-paging reads and unrelated provider
operations remain unchanged. It is not an old-parser downgrade or full ingestion
restoration. Forward restoration uses the same state and checkpoint. Canonical
Runner code keeps fixed capacity/pause terminals stopped across routine re-
enqueue/import; headless wake recovery is separately owned Runner work, not a
provider restoration claim; a separately authorized caller must explicitly rearm
the exact terminal/generation after resolving its cause. Neither code nor tests
authorize installation, rearming production state, or containment activation.

### Capacity-independent user observation (AGE-347)

`user_observation` v1 paging now issues provider-owned `codex-obs1-` tokens.
These bounded authenticated cursors carry positions and digests, not message
bodies. Beginning, empty tail anchors, continuations and resumes do not allocate
cursor or prefix files, even when unrelated retained canonical staging is full.
A partial record is reconstructed from its native byte range and checked against
the authenticated prefix digest before projection. Complete records still pass
through the same role, nonce-marker, timestamp, body and digest projection; an
unfinished record never becomes an observed turn.

This is an **observation-only resource-contract change**, not a larger canonical
quota. `max_source_bytes` still bounds identity-metadata reads plus forward
reads together. Observation may additionally reread **one prefix strictly below
8,388,608 bytes per call** from native source. No additional metadata allowance
is used. `source_bytes_examined` truthfully counts **all three native read
categories**, including reconstruction; it may exceed `max_source_bytes` only
for observation. Each observation result has exactly one accounting warning:

```text
codex_observation_io_v1:forward=<decimal>;reconstruction=<decimal>;metadata=<decimal>
```

The three counts sum to `source_bytes_examined`; `forward + metadata <=
max_source_bytes`, and `reconstruction < 8388608`. Hosts must validate this
observation envelope rather than applying the canonical total-source check to
it. The provider-carried `contract/v1/session.schema.json` applies an observation
maximum of 16,777,215 (maximum quantum plus maximum prefix) and requires the
accounting-warning shape; it retains the canonical maximum of 8,388,608.
JSON Schema constrains structure, not category-sum/per-request arithmetic: hosts
must enforce both. This is a **local provider extension**, not a claim of
upstream adoption; `contract/v1/UPSTREAM.md` retains immutable historical
snapshot references and hashes. A paired consumer's actual schema must also
accept this extension before use. Existing v1 fields carry the exchange, and
hosts must continue treating cursors as opaque. Reconstruction is **native I/O**, never
staging I/O. All reads count even if turn/response limits cause bytes to be read
again next call. Record assembly remains at most 8 MiB per call; response, turn,
inline-body and discovery ceilings are unchanged. Repeated small-quantum reads
can reconstruct/hash the growing prefix repeatedly, so total catch-up work is
not single-pass or linear in transcript length.

Authentication uses HMAC-SHA256 and one fixed 32-byte random key at the private
sibling `provider-state/codex/observation-auth-v1/key`. It is outside the
canonical admission scope, not an observation cache or a growing storage pool.
Key initialization holds the directory-inode lock, writes and syncs one private
`key.preparing` slot, then atomically renames it to `key`. Only bounded, private
regular preparation residue (at most 32 bytes) is discarded on a fresh start
with no final key; even a complete pre-rename candidate has issued no tokens.
No directory scanning or per-page temporary files are introduced. A published
key is never replaced, and file plus directory sync must succeed before tokens
can be returned, including on retry after rename. A malformed/unsafe published
key fails with an explicit diagnostic, not implicit rotation: it may be damaged
issued authority. Unexpected preparation residue is refused, not broadly cleaned.
Old initializers use the same lock but can still strand a short final key if
interrupted before all routes drain/cut over; this correction does not repair
that ambiguous state or guarantee mixed-reader/writer operation. The private
provider-owned directory lineage and cooperating lock protocol remain required;
this is not protection against hostile same-user directory replacement. Preserve
this key with provider state; a missing key for an existing observation token
fails stale. No expiry, eviction or automatic
rotation is introduced. This does not guarantee admission on an actually full or
unwritable filesystem; those failures remain explicit.

Tokens bind provider/account/settings/session/projection/nonce, snapshot,
position, budgets and the existing append-only source-generation checks. A
fresh process can replay a continuation byte-identically while the source and
key remain valid, including after valid append beyond its frozen snapshot.
Continuation/resume source selection uses the cursor-bound device/inode within
the selected account, before opening identity headers. It reads and charges the
selected source's current first metadata record, not unrelated rollout headers
or a remembered discovery cost. Thus unrelated valid rollout creation (including
nonstandard-filename fallback neighbors) does not change a bound page's forward
quantum or response. Initial discovery still checks metadata ownership and
ambiguity; existing account-directory discovery ceilings remain enforced.

Final response fitting includes authenticated partial-prefix cursor growth.
An otherwise fitting user turn may have its inline body omitted, retaining its
exact length and digests, to make room for that final cursor. If necessary the
page retains the preceding fitting complete-record boundary instead; no observed
turn is removed, and all bytes actually read remain charged. An unfinished
suffix remains pending for continuation/resume, not an invented observation.
Inode replacement, truncation, same-length mtime changes, unavailable sources or
reconstructed-prefix digest changes fail explicitly, never as successful empty
observation. The existing append-only premise remains: these checks are not a
whole-transcript content attestation of arbitrary same-inode rewrites combined
with growth outside the reconstructed range. Snapshot completion still means
frozen EOF coverage, not delivery acknowledgement or native session completion.
Runner owns exact-envelope matching, durable settlement and submission fences.

Legacy file-backed and packed `codex-stp1-` observation checkpoints are accepted with their existing
binding and generation checks, then produce source-backed tokens. Their retained
staging dependencies are neither deleted nor evicted. Already-issued legacy
response bytes are not claimed to equal the upgraded response encoding or its
new accounting; replay within the new implementation is deterministic.
Canonical packing, allocation admission, prefix files, token encoding and native
accounting retain the AGE-353 behavior described above. Old binaries cannot consume `codex-obs1-` tokens;
rollback requires a compatible reader or an explicit bounded reconciliation
plan, never deletion/reset of retained observation or canonical evidence.

### Active native notification receipt (AGE-355)

The existing opt-in `session_turn_pages_v1` capability (host selection:
`OULIPOLY_HOST_SESSION_TURN_PAGES_V1=1`) and bounded
`session.read_turns` / `user_observation` projection are sufficient to query
receipt while a native exec is still running. No new operation, marker, wire
field, native parser in Runner, model call, or permanent observer service is
needed. This candidate reuses the **active provider-carried contract**, including
its documented AGE-347 local I/O extension; it does not claim a matching canonical
SDK snapshot. Canonical SDK alignment remains unresolved future integration,
not a prerequisite for developing or testing this provider candidate.

Receipt here means one exact canonical notification envelope newly observed as
user input in the bound trusted native history, within a completed finite
post-anchor snapshot. It is **not** human-origin authentication, exclusive input-
entry provenance, model understanding, assistant response, task success, or
power-loss durability. HMAC cursors authenticate checkpoints, not native content.
The trusted-writer/append-only premise remains necessary; unclassified exact
user-role replay by that writer is indistinguishable from input. In particular,
absence of a receipt is not permission to resend or evidence of rejection.

Provider interpretation is applied before returning structured turn evidence:

- Only top-level `response_item` / `message`, exact role `user`, is eligible.
  Event echoes, assistant/tool messages and nested replacement history are not
  searched. The first session metadata and cursor source/account/settings/session/
  projection/nonce/budget fences remain unchanged.
- Content must be a nonempty array entirely of textual `input_text` or
  `output_text` entries with string `text`. Images, audio, unknown/nontext entries,
  malformed content and mixed text/nontext records are excluded, not reduced to
  a misleading exact-text match. Canonical ingestion is unchanged.
- Optional `internal_chat_message_metadata_passthrough.content_item_kinds`, when
  present and non-null, must align one-for-one with content and contain only
  `user.text`. Context classifications (including `compaction.summary`), unknown
  kinds and malformed metadata are excluded. Missing/null classifications remain
  eligible under the stated premise; this is not a new positive-provenance gate.
  Native item/runtime-turn IDs neither authorize receipt nor replace the existing
  synthetic `session:byte:offset` record ID / null parent.
- Canonical text concatenates chunks without an inserted separator, strips only
  an optional whitespace-delimited **terminal** `[OULIPOLY-DELIVERY <expected nonce>]`
  trailer, normalizes CRLF and CR to LF, then trims outer whitespace. The interior
  envelope nonce remains hashed. A wrong trailer, wrong nonce, or longer quotation
  does not match the full expected digest. `body_sha256` is the separate serialized
  chunk digest; `canonical_text_sha256` is the receipt-comparison field even when
  inline bodies are omitted.

Runner integration sequence (existing fields only):

1. Before submission, bind the exact attempt/envelope digest and nonce; request
   `start_mode: "tail"`, null tokens, `turn_projection: "user_observation"`,
   `expected_delivery_nonce: <64 hex>`. Persist the returned `resume_token` as the
   original pre-submit anchor. A tail anchors the last complete-record boundary,
   not the wall-clock creation time of every byte of an incomplete suffix.
2. During the active invocation, request `start_mode: "beginning"` with
   `after_token: <anchor or persisted resume token>`. Continue an incomplete
   snapshot with `start_mode: "continuation"`, its `snapshot_id` and
   `next_page_token`, clearing `after_token`. Preserve all binding and budget
   fields. Resume subsequent snapshots from their returned `resume_token`.
3. Validate the typed provider response, its identity, snapshot/page/sequence,
   source accounting and whole-envelope canonical digest. Count across the
   **entire finite snapshot**; exactly one match permits the consumer's exact-
   attempt receipt decision. Zero is unknown; two is ambiguous. Future uniqueness
   is not promised. `snapshot_complete: true, source_final: false` never means the
   invocation finished. Do not wait for assistant output or reuse terminalization
   to publish receipt/ACK.
4. Persist progress with the consumer's CAS fences, fixed observation stop and
   fair page/byte/time budgets; recheck stop/ownership/explicit partial or full
   ACK at publication. Partial lines produce no record until newline/valid JSON;
   stale-source errors and budget failures retain uncertainty, not a fresh anchor
   or implicit retry of the delivery. Existing source reconstruction limits above
   still apply (total is not just the requested forward quantum).

Root owns scheduling, ACK/lifecycle races and rollout pairing. Do not promote
persisted match counts from an older provider's more permissive projection as
newly qualified evidence: unresolved attempts needing these exclusions must be
reobserved from their **original** anchor under one selected provider revision,
without resubmission or replacing that anchor. Old token mechanics remain
readable, but cached page interpretations are not retroactively requalified.
PTY native-ACK expansion and native response-progress correlation remain future
work; current PTY transport/drain ACK and old prompt-submission markers keep their
existing meaning.
