# Codex Agent Bash integration

`agent-bash-mcp.ts` exposes a single MCP tool, `bash`, and invokes the exact
OpenCode implementation in `../opencode/tools/bash.ts`. That file is byte for
byte equal to the committed `agent-bash-tool` source recorded in
`../opencode/BASH_SOURCE.json` (commit `52975ab5449528ecc21dfce0dd31e4c6a8b9fbbf`).
It combines retained-output/local-receipt behavior with optional command workdir
forwarding. Installed OpenCode equality is **not verified** for this revision;
the manifest date records source verification, not installation verification.

The shim adapts only OpenCode's tool registration API. Bun compiles the pinned
file in memory, resolving `@opencode-ai/plugin` to the local registration shim.
No dependency installation or network access is needed. All command admission,
pinning, delivery selection, ownership, completion observation, and cancellation
logic executes from the unchanged source.

Launch the stdio server with:

```sh
bun --no-install integrations/codex/agent-bash-mcp.ts
```

The provider configures `mcp_servers.agent_bash` with `command` pointing to Bun,
`args` containing `--no-install` and this script's absolute path, `required=true`,
and a sufficiently long `tool_timeout_sec` for synchronous workloads. It sets
`env_vars` to every inherited environment variable's **name**, including the
session-binding variables below. This preserves the invocation environment
without embedding credential values in arguments or generated configuration.
The `AGENT_BASH_BIN` and `AGENT_BASH_AGENT_RUNNER_BIN` overrides retain their
upstream meaning.

For headless exec, before spawning Codex the provider creates a private empty
file and exports its absolute path as `AGENT_RUNNER_CODEX_SESSION_FILE`. It writes the native Codex
thread ID plus newline atomically when `thread.started` arrives. The MCP server
waits up to five seconds for that ID on a tool call. Known resumes may instead
set `AGENT_RUNNER_CODEX_SESSION_ID`. Missing or invalid bindings reject the call;
the server never uses an inherited parent `CODEX_THREAD_ID` as its own identity.

MCP cancellation notifications abort the corresponding adapter call. Stdin
closure or process termination aborts active calls and gives their cancellation
up to 1.5 seconds before exiting. Ordinary synchronous workloads remain leased
to the MCP process; asynchronous headless child dispatches survive a normal
turn exit exactly as in the source adapter. The MCP transport uses pipes in both
modes; transport does not select delivery policy. Managed interactive launches set `AGENT_RUNNER_CODEX_INTERACTIVE=1`,
which gives the unchanged adapter a synthetic `stdin.isTTY` and preserves its
interactive child delivery and owner leases. Interactive identity comes from
`_meta.threadId`, with session-binding acknowledgement before dispatch, not from
the headless session-file path. This identity acknowledgement is not an
acknowledgement that the host consumed the returned tool body.

Run the deterministic tests with:

```sh
python3 integrations/codex/test_mcp.py
```

They check source equality, MCP discovery, argument rejection, environment and
workdir propagation, native session binding, snapshot acquisition before local
receipt, headless child delivery, cancellation, and stdin-close cleanup using a
fake spooler, a fail-closed fake runner, and private HOME/XDG directories.
The tests do not issue model requests or run production Agent Runner jobs.

Terminal output is acquired and hash-validated before `accept-output` records a
local receipt and status requests progression. Acquired output survives later
control failures; remote ACK and physical drain remain explicitly unconfirmed.
This is a bounded snapshot, not proof of a complete historical log.

The compiled provider embeds the shared adapter and four Codex assets in
`src/registration.rs`; the provenance manifest is not embedded. Interactive
launch staging uses those compiled bytes and rejects mismatching custom integration inputs.
Headless exec does not stage: it passes the configured on-disk MCP path to Bun,
and that bridge loads its adjacent `../opencode/tools/bash.ts`. Configuration
validation does not compare these adapter bytes to the embedded release.
`scripts/install-provider.py` checks the supplied OpenCode copy against the
vendor and manifest, but that check does not prove the supplied binary embeds
those bytes. Build the selected source and verify its actual private staging
before coordinated delivery, and separately verify actual headless configured/
adjacent selection. Interactive staging success cannot identify headless bytes
(or vice versa); adjacent source equality is not installed verification.
