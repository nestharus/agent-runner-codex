# Migration verification — 2026-09-04

Validated the installed release against Codex CLI **0.153.4**.

- `cargo test`: **52 passed**, covering provider schemas, launch completion
  receipts, cancellation, large output, replay, quota, native sessions, and paging.
- `python3 integrations/codex/test_mcp.py`: **8 passed**, including the unchanged
  Bash implementation, child dispatch, environment propagation, and cancellation.
- Native inventory checks passed for all five Astra efforts and the isolated
  Luna benchmark using a local Responses stub. The configured system instruction
  text matched `~/ai/AGENTS.md` after Codex's trailing whitespace normalization.
  Agent Bash was the only execution tool. Four native helpers remained for user
  input and MCP resource inspection; native sub-agents were absent.
- The project-configuration positive control exposed the injected MCP tool when
  configuration isolation was deliberately removed; managed launches excluded it.
- Installed OpenCode Bash, the OpenCode repository copy, and the Codex integration
  were byte-identical at SHA-256
  `23dbb0dfd555e3ac720659e5b22a1fe0119c5ebe13102b09231ab47e4fd42c2d`.
- `python3 scripts/verify-live.py --account codex3 --run`: **passed** using the
  separately named Luna/low benchmark. Both turns completed through Agent Runner;
  resume retained the native session and recalled a random verification token.
  The native rollout recorded exactly one Bash call per turn, each with
  `DONE rc=0` and the expected command output.

Live verification isolated both Agent Runner and Agent Bash configuration and
state. It used existing native account authentication and did not change
production routing. Astra model execution was verified against the local stub;
the live model turns used Luna. Capability limits are documented in the README.
