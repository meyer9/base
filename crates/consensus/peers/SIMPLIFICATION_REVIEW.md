# Consensus Peers Simplification Review

**Date:** September 22, 2026

No source change is justified in this review. The apparent redundancies either
serve distinct call patterns or have in-workspace consumers, while removing
public methods based only on a workspace-wide search would risk breaking
downstream users.

## Candidates reviewed

- `EnrValidation::is_valid` and `is_invalid` are logical complements, but both
  are used by consensus discovery callers (`crates/consensus/disc/src/driver.rs`)
  and `is_invalid` is also used by gossip (`crates/consensus/gossip/src/driver.rs`).
  Removing either would expand this review into behavior-preserving caller
  churn without reducing the supported surface safely.
- `NodeRecord::convert_ipv4_mapped` reports whether conversion occurred, while
  `into_ipv4_mapped` offers a consuming, chainable form. The latter delegates
  to the former, but they have different receiver and return contracts; neither
  is used outside the crate today. That does not establish that public callers
  do not rely on them.
- `PeerUtils` is not a one-operation forwarding wrapper: its three public
  methods perform distinct conversions, and are used in bootnode creation,
  dialing, discovery, and gossip paths.

## Outcome and validation

- APIs removed: none. Runtime behavior and supported behavior are unchanged.
- Complexity before/after: no source concepts or call paths removed; this review
  records why the inspected candidates were retained.
- Validation: `cargo test -p base-consensus-peers --lib` passed (32 passed,
  1 ignored).
- Limitation: workspace searches cannot identify consumers outside this
  repository, so public API removal needs stronger compatibility evidence.
