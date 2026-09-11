# Pinned native hook probe

This harness uses actual public Codex **0.153.4** configuration-loader and hook-engine
crates, not a fake hook parser or an installed/paid TUI session. Obtain the public
source at commit `3d2ee51ca2d5db578f328aa75e20aa22c0197c9a`
(`rust-v0.153.4`), for example from the GitHub `openai/codex` archive for that
commit. Keep it outside either implementation checkout. Do not patch native
product sources. The build helper seeds the harness with upstream `Cargo.lock` (Cargo adds only
the harness root/prunes unused packages), creates a separate crate and private
physical HOME/config/data/tmp, uses two Cargo jobs, and limits build time. Cargo's
normal dependency acquisition is permitted; select a dedicated output directory,
not another work unit's build cache.

```sh
python3 tests/native/build_probe.py \
  --native-source /private/planning/codex-3d2ee51ca2d5db578f328aa75e20aa22c0197c9a/codex-rs \
  --output-dir /private/planning/age356-native-probe

timeout 180 python3 tests/test_session_registration.py \
  --binary /absolute/provider-target/debug/agent-runner-codex \
  --bun /absolute/path/to/bun \
  --native-probe /private/planning/age356-native-probe/target/debug/age356-native-probe
```

The Python fixtures use a separate physical private home for each test, close
all sockets/processes, and delete those fixtures. The native loader's filesystem
adapter rejects reads outside that fixture. Explicit system/managed/requirements
paths replace host policy **in the test only**; no production policy is read or
modified. No native executable, model, UI, auth request, thread start, or tool
operation is launched. Native `Hooks` supplies the hook payload and executes the
real provider helper. A synthetic authenticated receiver calls the actual provider
`session.capture` on private rollout fixtures. Runner binding/promotion and cached
acknowledgement tests are separate Rust tests in the runner repository.

Evidence includes actual declaration-key/hash equivalence, untrusted-project
exclusion, matching/wrong/old-path trust state, feature-off, managed-only exclusion,
authorised system hooks, synchronous no-tool capture/ack, empty successful stdout,
and structured stop on both initial and subsequent submitted-turn hooks. This
is not full TUI behavior, deployed executable attestation, a universal stop on
hook crash/timeout, or proof of every managed/cloud policy path. The provider's
stdio `config/read`/`configRequirements/read` preflight has deterministic fake-server
coverage and pinned source schema grounding, not an executed native app-server
compatibility test here. Source-bound limits must remain visible in handoff.

The read-only filesystem fixture is adapted from upstream Apache-2.0 tests;
see `UPSTREAM-LICENSE`. Native product crates remain external unmodified inputs.

## Preflight process consequence (source evidence, not an executed probe)

Native `app-server/src/lib.rs` initializes SQLite with
`init_sqlite_state_db_with_fresh_start_on_corruption` before serving config RPCs.
The provider's config-only preflight therefore is not read-only at process level.
The provider constrains transport to stdio and applies the pinned native ephemeral
remote-control-disabled startup marker so persisted remote control cannot accept
model work during the probe. Normal native initialization/recovery effects still
require a consuming-workflow decision before delivery. No private native loader
fixture result is evidence that this app-server initialization is harmless.

## Configuration-only library alternative (not production admission)

```sh
timeout 45 python3 tests/native/test_config_only.py \
  --native-probe /private/planning/age356-native-probe/target/debug/age356-native-probe -v
```

The harness's `config-only` mode exits after the actual native layer loader,
typed conversion and exact-requirement application, before constructing hooks.
Three private fixtures discriminate CLI settings from real managed redirection,
show system instruction paths and distinct managed feature requirements, and
compare fixture file names, modes and bytes before/after (including deliberately
invalid database sentinels). No fake app-server echo participates. These are
library-boundary experiments, not an execution of provider compatibility checks,
SQLite recovery/backfill, the native CLI, or a production-safe isolated server.
File snapshots do not detect transient writes or prove absence of reads; the
no-datastore-startup claim additionally rests on the harness call graph.

The mode uses test-only local policy paths and the default empty cloud loader.
It cannot be installed as the compatibility implementation without separately
preserving actual system/managed/cloud policy acquisition and native-version
coherence. Native `features list` omits owned settings and hook requirements;
`doctor` additionally initializes auth and runs runtime/reachability checks.
The existing production app-server preflight remains rejected, not approved by
these tests. Its removal/replacement is still blocked on that integration choice.
