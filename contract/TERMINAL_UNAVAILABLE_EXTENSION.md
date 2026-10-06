# Terminal-unavailability extension provenance

The complete `extensions/terminal-unavailable` directory was originally copied
unchanged from `nestharus/agent-provider-sdk` commit
`c167f598308f45154b3323810b4115b88dd80a3c`, directory
`crates/provider-contract/contract/extensions/terminal-unavailable`.

## Original import SHA-256 identities

| File | SHA-256 |
| --- | --- |
| `v1.schema.json` | `8839e947ac2da0a5143caa49aa302d789a9b5cdc881923fbf63abb648d890df0` |
| `README.md` | `fe738b05a4bb43efc07203002ca4edbfbc7a89451597784201ed525b29082620` |

## Current directory provenance

The current `README.md` is copied byte-for-byte from SDK commit
`abc636e7b85c35c22b6aa7a624bf4f32585b8805`, at the same directory path.
Its SHA-256 is
`772e44e7d642f56eada378c377355d29310973625ebee5a4634813dd88230ed4`.
This documentation update removes peer snapshot equality requirements while
preserving semantic compatibility and rollback obligations. The extension
identifier, selection, fallback, and `v1.schema.json` bytes are unchanged; the
schema still matches its original import digest above. These source and digest
records establish local provenance, not runtime peer equality.

This independently versioned extension is applied to the existing deployed
provider/v1 route. It does not replace that route's base snapshot with the SDK's
older pinned snapshot. The route's common schema adds `provider_unavailable` to
its terminal-signal enum; all existing envelope and process-status validation
remains in effect. The new kind is emitted only when the current request selects
`host.env.OULIPOLY_HOST_TERMINAL_UNAVAILABLE_V1=1`. The runner selects it on launch
and terminal classification requests; providers may continue returning existing
kinds. An unselected host receives the existing `nonzero_exit` fallback.

The runner records a typed `provider_unavailable` failure and terminal reason.
The category does not mark account quota exhausted or automatically retry the
turn. Native error recognition remains in each adapter. Retain the previous
working route through replacement verification and the rollback decision; if it
cannot be retained, continue on that route. Rollback restores its required
compatible readers and durable state without requiring equal peer source
revisions or snapshot digests. Restored older providers still work with the
updated runner through the existing kinds and unselected fallback; extension
adoption does not establish compatibility for unrelated base-schema changes.
