# Agent Runner Codex Provider

Codex terminal adapter for Agent Runner's `oulipoly.provider/v1`
external-provider contract.

The provider will use `agent-provider-sdk` for shared transport, containment,
custody, profile validation, tool policy, and conformance testing. Codex-specific
command lines, authentication, `CODEX_HOME` isolation, threads, streaming events,
and app-server lifecycle remain in this repository.

The initial implementation will establish two separately named, non-production
profiles:

- `codex-exec-bench` — `codex exec --json` correctness and memory baseline;
- `codex-appserver-bench` — reusable stdio app-server experiment.

Neither profile will replace root routing until system instructions, sole Agent
Bash enforcement, session behavior, upgrades, failure isolation, and the common
`gpt-luna-low` benchmark matrix pass.

## Status

Bootstrap phase. The implementation is tracked by the Agent Provider team's
Codex Provider project.

## Local layout

```text
~/projects/agent-runner-codex/
├── trunk/       # clean main integration checkout
└── worktrees/   # isolated ticket branches
```
