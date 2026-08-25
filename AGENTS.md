# Agent Runner Codex Provider — Agent Entry Point

## Purpose

This repository owns the Codex terminal adapter for Agent Runner's versioned
external-provider contract. Provider-neutral behavior belongs in
`agent-provider-sdk`; Codex command, app-server, authentication, configuration,
thread/session, and tool integration belong here.

## Repository layout

The checkout lives at `~/projects/agent-runner-codex/trunk`. Isolated task
branches belong in `~/projects/agent-runner-codex/worktrees/<ticket-or-task>`.
Keep `trunk` on `main` as the clean integration checkout. Do implementation work
in a worktree, merge and push the verified result to remote `main`, then remove
the task worktree and branch when complete.

## Provider requirements

- Implement `oulipoly.provider/v1` using stable Codex stdio surfaces. Treat
  `codex exec --json` as the one-process correctness baseline and `codex
  app-server` as a separately tested shared-runtime optimization.
- Do not make experimental WebSocket or dynamic-tool protocols a production
  dependency without an explicit compatibility work unit.
- Validate that requested system-prompt and Agent Bash/tool restrictions are
  effective before accepting a launch.
- Preserve the invocation environment. Apply only explicit profile or isolated
  `CODEX_HOME` transformations; do not introduce an environment allow list.
- Keep credentials, tokens, and machine-specific state out of source control,
  argv, fixtures, benchmark reports, and errors.

## Tests

- Use deterministic fake-server fixtures for contract and lifecycle coverage.
- Live tests must use the configured low-cost model, separately named benchmark
  profiles, and isolated configuration without changing production routing.
- Shared app-server tests must cover identity isolation, thread ownership,
  cancellation, draining, restart, idle unload, upgrades, and exec fallback.
- Memory reports must measure the complete process tree and distinguish private
  PSS from shared-server memory.

## Documentation

Keep `README.md` current with the pinned Codex version/surfaces, implemented
capabilities, auth/config isolation, build/install steps, and benchmark modes.
