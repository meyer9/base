# Batcher Admin Simplification Review

## Finding

No safe code simplification was identified in `crates/batcher/admin`. The
public JSON-RPC methods in `src/api.rs` are distinct protocol entry points;
their implementations delegate to matching `AdminHandle` operations and
convert core failures into protocol error codes. The mappings for channel
closure, unsupported operations, and stopped-state errors are covered by the
crate's unit tests.

Removing `setLogLevel` is not a cleanup-only change: it is part of the public
JSON-RPC trait, logs requests, and returns the explicitly documented
`NotSupported` result from `AdminHandle`. The server's retained
`ServerHandle` controls the running server lifetime, and `BatcherService`
stores the `AdminServer` while it is active. Neither is redundant ownership.

## Change and Validation

No production behavior or code changed. Before and after this review, the
crate retains the same eight JSON-RPC methods, error mappings, and server
lifecycle ownership; zero code concepts were removed. The only addition is
this report, documenting why deleting or combining existing responsibilities
would alter the protocol or lifecycle contract.

Validation: `cargo test -p base-batcher-admin` passed (3 unit tests; doc tests
passed with none defined).

## Retained Risk

Repository searches cannot reveal clients maintained outside this repository.
Treat the JSON-RPC methods and their error codes as externally consumed until
compatibility requirements establish otherwise.
