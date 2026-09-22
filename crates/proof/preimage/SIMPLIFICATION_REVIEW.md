# Simplification review

**Date:** September 22, 2026

No code change is justified within `crates/proof/preimage` without either removing
useful public API or expanding the assigned scope.

- `CommsClient` is a blanket composition of preimage reads and hint writes, and
  multiple proof clients and providers use it as a concise generic bound. Replacing
  it with repeated bounds would add complexity; removing it would break those
  consumers.
- `BidirectionalChannel::new` wraps infallible `async_channel::unbounded` calls in
  `std::io::Result`, making it a plausible simplification candidate. However,
  callers in `crates/proof/host` and `crates/proof/zk/witness` currently propagate or
  map that result. Changing this public signature coherently requires edits outside
  the assigned crate, so it is deferred rather than partially changing the API.
- The oracle and hint paths retain distinct framing and acknowledgement behavior;
  the colocated tests cover successful exchange and hint error handling. No protocol
  logic was removed or changed.

The public surface and runtime behavior are unchanged. The only artifact is this
record of the bounded review; no behavior-regression risk is introduced.

Validation: `cargo test -p base-proof-preimage --features std` (11 passed; one
README doctest ignored).
