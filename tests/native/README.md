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
hook crash/timeout, or proof of every managed/cloud policy path. Interactive
preparation does not invoke this harness or a native config/runtime probe. Separate
fake-executable tests record every invocation and require only `--version` plus
one normal TUI exec; they do not infer effective policy from emitted settings.

The read-only filesystem fixture is adapted from upstream Apache-2.0 tests;
see `UPSTREAM-LICENSE`. Native product crates remain external unmodified inputs.

## Removed preflight consequence (preserved source evidence)

The former app-server preflight initialized SQLite, including conditional recovery
and backfill, before serving config RPCs. Managed policy could redirect that store;
the short probe deadline could interrupt a longer backfill lease. That code is now
removed, not replaced by another startup probe. These observations remain valid
for the prior implementation. Loader fixtures never proved it harmless. Normal
native startup and its policy/storage effects remain native-owned.

## Configuration-only library evidence (not a product obligation)

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
It is retained only as evidence that real local native requirements can redirect
settings and exclude hooks; it is not a proposed installed companion, complete
policy snapshot or provider admission gate. Native system/managed/cloud enforcement
runs during the normal native startup, not in a new provider policy emulator.
