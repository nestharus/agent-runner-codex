# AGE-355 provider candidate

## Disposition

Implemented and locally verified on assigned base `dc0f0f0`, in the isolated
`age355-native-receipt` worktree. Candidate only: root owns observational review,
Runner dispatch, paired integration, publication and delivery. No nested agents,
SDK/Runner edits, vendored-contract changes, new wire fields, model workloads,
production logs/configuration access, installation, push or merge.

The root's correction supersedes the earlier assessment's SDK-first dependency
and exclusive-input-origin requirement. SDK alignment is unresolved future
integration, **not** a prerequisite for this provider work. No claim of a matching
canonical SDK snapshot is made.

## Implemented decision

Reuse active `session.read_turns`, `oulipoly.session_turn_pages/v1`, projection
`user_observation`. The bounded paging engine already observes append while
launch is active and survives independent observer processes; adding another
reader/query/event protocol would duplicate working machinery.

`src/session_turn_pages.rs::observable_user_text` now prevents lossy receipt
projection of mixed/nontext input and excludes contextual/unknown/malformed
current metadata. It accepts wholly textual native arrays, and, when present,
aligned `content_item_kinds = ["user.text", ...]`. Missing/null classifications
remain eligible under the trusted-writer assumption. Native item IDs/runtime-turn
IDs do not prove origin. Canonical ingestion and prompt-acceptance/submission
semantics are unchanged. Existing anchor/source/settings/session/nonce/budget
binding, canonical normalization/trailer behavior, fixed snapshots, errors and
restart machinery are retained.

Source basis: assigned brief and both prior research/assessment files; inspected
current adapter directly. Pinned upstream source was read (not executed):
`openai/codex` tag `rust-v0.153.4`, `codex-rs/core/src/session/mod.rs`
(`response_item_from_user_input` assigns `user.text` for InputText/OutputText),
`core/src/context/compaction_summary.rs` (`compaction.summary`), and
`protocol/src/models{.rs,/item_metadata.rs}` (optional classification vector,
transparent string kinds). These support metadata interpretation, not live
native-version attestation. Tests below are private synthetic fixtures.

## Runner integration contract and examples

README section **Active native notification receipt (AGE-355)** gives the full
boundary and sequence. Advertise/select the existing capability through host env
`OULIPOLY_HOST_SESSION_TURN_PAGES_V1=1`; use the current provider-carried schema and
existing AGE-347 accounting extension, not an assumed canonical SDK identity.

Example `session.read_turns` params (normal request envelope also carries bound
`provider_instance_id` and host account/data-root settings):

```json
{
  "settings_id": "codex2",
  "session_id": "11111111-2222-3333-4444-555555555555",
  "read_protocol": "oulipoly.session_turn_pages/v1",
  "turn_projection": "user_observation",
  "expected_delivery_nonce": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "start_mode": "tail",
  "after_token": null,
  "snapshot_id": null,
  "page_token": null,
  "max_turns": 64,
  "max_response_bytes": 131072,
  "max_source_bytes": 524288,
  "max_inline_body_bytes": 0
}
```

Persist the tail `resume_token` before submission. Set `start_mode=beginning` and
`after_token=<persisted token>` for an active tick. For continuation set
`start_mode=continuation`, clear `after_token`, and send returned `snapshot_id`
and `next_page_token` as `page_token`. No launch/resume/model call is involved.

A completed result with one `role=user` turn and
`canonical_text_sha256=<SHA256 of normalized full envelope>` is usable receipt
**evidence**, after consumer identity/page/accounting checks and whole-snapshot
uniqueness. `body_state=omitted_oversize`, null body and `source_final=false` are
normal. Match the canonical digest, not `body_sha256`, a substring, a nonce alone,
or an assistant event. The fixture envelope is
`<notification nonce="<64 a characters>" seq="1,2">ready</notification>`;
its digest is computed from the same exact text sent to native stdin.

Consumer obligations unchanged: original anchor, full nonce/digest/attempt
binding, finite uniqueness, bounded fair ticks, fixed stops, persisted CAS
checkpoints and final stop/ownership/ACK recheck. Publish exact-attempt mailbox
receipt separately from invocation terminal/idle state. Explicit partial/full
consumer ACK remains independent authority. The provider does not settle mail.

Upgrade caveat: do not promote older-provider cached match counts as evidence of
these exclusions. For unresolved attempts requiring the new projection, reobserve
from the original persisted anchor under the chosen provider revision; do not
reset that anchor or resubmit. Cross-revision cached page interpretation is not
attested, despite retained token readability. Root/Runner owns this integration
choice, paired tests and any historical evidence disposition.

## Verification actually executed

All commands ran synchronously with explicit outer timeouts. Full logs are
machine-local under this worktree's `.tmp/age355/` (not source-controlled):

| Command | Result | Log |
|---|---|---|
| `timeout 180s cargo test --test codex_session_pages age355 -- --nocapture` | 3 passed | `age355-targeted-test.log` |
| `timeout 90s cargo test --test codex_launch age355 -- --nocapture` (initial) | 1 failed: fixture expected status `error`; existing exit-1 contract correctly returned `exited` | `active-test-initial-failed.log` |
| Same active command after correcting assertion to `exited` and exit code 1 | 1 passed | `active-test.log` |
| `timeout --kill-after=10s 240s cargo test --all-targets` | 135 top-level tests passed, 4 ignored subprocess helpers; no failures | `full-test.log` |
| `cargo fmt --all -- --check` / `git diff --check` | Both succeeded | `fmt-check.log` / `diff-check.log` |

The ignored helpers are invoked by applicable parent tests, not evidence of
universal coverage. Existing `durable_fs.rs` dead-code warnings remain; they were
not suppressed or changed. Initial fixture failure is retained, not relabeled.

New checks:
- Active fake native exec writes the **actual submitted prompt** as a current-
  metadata user record, then remains running with no assistant output. A normal
  provider contract query returns its exact digest before release; launch remains
  active. After release native exits 1, and receipt observation remains replayable.
- Known contextual, unknown/malformed metadata, mixed/nontext content, wrong
  roles, event echoes and nested replacement history do not project as receipt;
  missing/null metadata remains eligible; canonical ingestion is unchanged.
- Pre-anchor identical history, longer quotation, wrong interior nonce and wrong
  terminal trailer cannot supply the selected exact match. Split chunks and CRLF/
  CR normalize correctly. Two exact new records retain distinct byte IDs as
  ambiguous evidence, not a manufactured confirmation.
- Partial appended current-metadata input survives independent observer process
  restarts; neither partial JSON nor complete JSON without newline yields a turn.
  Completing newline yields one stable, replayable exact digest.
- Existing full-suite tests reverify wrong provider/account/session/nonce/budgets,
  stale/replaced/truncated sources, partial-prefix tampering, fixed snapshot append,
  source-I/O accounting, schema/fitting, record ceilings and key recovery.

Active fixture uses a 5-second readiness bound, 10-second fake-native bound,
release-on-unwind guard and scoped launch join; subprocess observations are
synchronously reaped and all fixture roots are TempDirs. No permanent service or
background workload was introduced. Build outputs and test logs remain local for
root readback; fixture state is cleaned.

## Limits / unresolved integration

No live native execution, stochastic efficacy, human provenance, authenticity,
power-loss durability, future uniqueness or response/task-success claim. Exact
unclassified user-role replay by the trusted writer remains indistinguishable;
unknown classifications fail closed and may need future pinned-version work.
Tail anchors the last complete-record boundary, including the documented
preexisting-partial-suffix limitation. Same-inode rewrite-plus-growth outside a
retained partial prefix remains outside the append-only guarantee.

SDK reconciliation, Runner periodic scheduling/fairness/fixed-stop/CAS/ACK races,
paired route schema admission and upgrade handling remain root-owned work.
Provider tests do not establish those consumer properties. No new provider-owned
extension was necessary. PTY native ACK and native response-progress correlation
remain explicitly future work; neither is silently inferred from transport ACK,
prompt submission or the next assistant output.
