# Contract and PTY verification — 2026-09-05

The installed provider and runner were verified against Codex CLI **0.153.4**.

- Final provider `cargo test`: **70 passed**, including managed interactive
  launch, native terminal errors/signals, cross-account SQLite ownership, and
  instruction reset through the runner's actual policy environment merge.
- Bash MCP integration: **12 passed**. Label installer: **4 passed** against
  the release binary. Rust formatting, Python compilation, and diff checks passed.
- Native exec and TUI inventory checks used the pinned CLI with a local
  Responses server. Managed launches exposed Agent Bash as the only execution
  tool and retained the four native input/resource helpers. The TUI check also
  verified native thread metadata and exclusion of user/project MCP canaries.
- Real notification messages reached the original runner PTY: normal child
  completion, a paused/resumed batch, and an exit-code-7 completion. Replaying a
  completed callback retained the original mailbox row without a second delivery.
- Managed PTY launch bound its native session automatically, dispatched a Luna
  child through Bash, received the notification as a native user turn, and read
  the child's log. Same-account headless and PTY resumes retained the native
  session and recalled the previous result.
- The final installed-binary check used the installed Bash bridge and a codex3
  PTY with a codex4 Luna/low child. It recorded **one delivery attempt, one
  notification user turn, one assistant receipt, and two Bash calls**. The child
  thread existed only in its selected account's SQLite database. Both runtime
  generations exited and their exact native process incarnations were gone.
- Installed runner/provider binaries and bridge bytes matched the verified
  release/source artifacts. All five Codex account PTY commands and resume
  templates were activated with backups; existing model label contents were
  unchanged. Already-running PTYs retain their startup tool inventory.

Live probes used separate benchmark labels and isolated runner/spooler state.
They used existing native account authentication. The OpenCode Bash source
remains byte-identical at the SHA-256 recorded below.

These checks establish the implemented contract, not every optional operation.
Prompt-acceptance attestation, standalone auth refresh, session export/replace,
cross-account rotation, arbitrary settings CRUD, and shared app-server runtime
remain unsupported. Ambiguous PTY submission recovery remains conservative in
the runner. Managed PTY support currently requires Unix and the pinned CLI.

## Earlier migration baseline — 2026-09-04

Validated the installed release against Codex CLI **0.153.4**.

- `cargo test`: **53 passed**, covering provider schemas, launch completion
  receipts, cancellation, large output, replay, quota, native sessions, paging,
  and identical native launches for standard and temporary model aliases.
- `python3 integrations/codex/test_mcp.py`: **8 passed**, including the unchanged
  Bash implementation, child dispatch, environment propagation, and cancellation.
- Native inventory checks passed for all five Astra efforts under both the
  standard `gpt-*` and temporary `codex-gpt-*` labels, plus the isolated
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
