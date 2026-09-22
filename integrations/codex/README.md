# Codex Agent Bash integration

`agent-bash-mcp.ts` exposes a single MCP tool, `bash`, and invokes the exact
OpenCode implementation in `../opencode/tools/bash.ts`. That file is byte for
byte equal to the stable `agent-bash-tool/main` source at commit
`dea893c984a944aec7e6b277a9adafec2882954d`, recorded in
`../opencode/BASH_SOURCE.json`. The retained-output snapshot and
`accept-output` path preserves acquired output across local receipt failures
without replaying consumption. This source identity does not claim equality
with an external OpenCode installation.

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

Before spawning Codex, the provider creates a private empty file and exports its
absolute path as `AGENT_RUNNER_CODEX_SESSION_FILE`. It writes the native Codex
thread ID plus newline atomically when `thread.started` arrives. The MCP server
waits up to five seconds for that ID on a tool call. Known resumes may instead
set `AGENT_RUNNER_CODEX_SESSION_ID`. Missing or invalid bindings reject the call;
the server never uses an inherited parent `CODEX_THREAD_ID` as its own identity.

MCP cancellation notifications abort the corresponding adapter call. Stdin
closure or process termination aborts active calls and gives their cancellation
up to 1.5 seconds before exiting. Ordinary synchronous workloads remain leased
to the MCP process; asynchronous headless child dispatches survive a normal
turn exit exactly as in the source adapter. MCP uses pipes in both modes.
Managed interactive launches set `AGENT_RUNNER_CODEX_INTERACTIVE=1` to preserve
the adapter's interactive delivery and owner leases. Headless identity uses the
session file; interactive identity uses `_meta.threadId` and the session-binding
acknowledgement. That acknowledgement does not confirm host consumption of the
tool result.

Run the deterministic tests with:

```sh
python3 integrations/codex/test_mcp.py
```

They check pinned source, MCP discovery, argument rejection, environment and
workdir propagation, native session binding, snapshot-before-receipt ordering,
receipt failure output retention, headless child delivery, cancellation, and
stdin-close cleanup using a fake spooler.
The tests do not issue model requests or run production Agent Runner jobs.

The compiled provider embeds five integration assets. Interactive launches stage
those bytes into a private generation. Headless exec selects the configured
on-disk bridge, which loads its adjacent Bash. The installer compares its
prospective assets with the binary's `--integration-hashes` output, then reads
back installed files, runtime config and executable link. A future launch's
effective policy and remote output acknowledgement require separate evidence.
