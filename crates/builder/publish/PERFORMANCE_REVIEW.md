# Publish Crate Performance Review

## Verdict

No performance change is proposed for `base-builder-publish`.

## Evidence

`WebSocketPublisher::publish` serializes a payload, stores a cloned UTF-8
buffer for replay, and broadcasts it. Its production callers are the
Flashblock payload and service modules in `base-builder-core`. The existing
Criterion benchmark measures that same API with flashblock-like JSON payloads
and 0, 1, or 10 local WebSocket subscribers.

The repository feature map explicitly deprecates the Flashblock builder and
rules out optimizing it. That makes this publisher workload an unsuitable
target for a new optimization, regardless of a local microbenchmark result.
No candidate implementation or supported-path end-to-end workload is
identified in this crate's scope.

## Measurement and validation

- Baseline/candidate: not run; no eligible optimization was identified, so a
  comparison would not support a product-facing performance claim.
- Workload, samples, machine, and latency/resource distributions: not
  collected; no benchmark result is claimed.
- Evidence reviewed: production call sites, `publish` implementation, existing
  Criterion workload, and the current feature-map deprecation rule.
- Code and tests: unchanged; no code test was needed for this review.

Revisit measurement only if this publisher remains on a supported path after
Flashblock retirement or the roadmap explicitly requires a retirement-blocking
performance fix. In that case, benchmark the supported end-to-end workload and
compare a candidate against a recorded baseline, including latency and resource
guardrails.
