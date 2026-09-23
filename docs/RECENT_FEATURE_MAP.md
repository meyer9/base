# Product Direction and Feature Map

A short map of what Base owns, where to start, and why the boundaries exist.
Read it before changing product, protocol, builder, proof, Reth integration, or
operator behavior. It is an orientation index, not an API reference.

## How to use this map

1. Start at the system path that owns the observable behavior.
2. Follow the listed flow before changing a boundary.
3. Read the target crate's README, public API, and focused tests before editing.
4. Keep Base policy at the narrowest required boundary; do not add an adapter
   that only forwards an upstream API.

## System paths

### Hierarchy

- **Entry points** — `bin/base`, `bin/node`, and `bin/consensus` start the
  unified node, execution node, and consensus node.
- **Shared protocol** — `crates/common` owns chain/genesis data, transaction
  and EVM/precompile rules, EIP-8130 types, RPC types, signing, and events.
- **Execution** — `crates/execution` owns the Base Reth node, chain spec, EVM,
  txpool, payloads, trie/state, Engine/RPC extensions, and node lifecycle.
- **Consensus** — `crates/consensus` owns L1 following, derivation, origins,
  Engine requests, unsafe gossip, SafeDB, peer discovery, upgrades, and RPC.
- **Batcher** — `crates/batcher` turns safe/finalized L2 blocks into encoded,
  compressed blob/calldata submissions and tracks L1 confirmation.
- **Builder** — `crates/builder` adapts pool/state into Base payload ordering,
  metering, multiplexing, sealing, and publication.
- **Proofs** — `crates/proof` owns witness/preimages, proof backends, workers,
  proposer/challenger flows, submission, disputes, and recovery.
- **Operations** — `crates/infra`, `crates/utilities`, `actions/harness`, and
  `etc/systems` provide CLI, snapshots, health, telemetry, devnet, benchmarks,
  system tests, and operator evidence.

### Principal flows

- **User transaction:** RPC → admission/txpool → builder/payload → EVM →
  state/trie → Engine/RPC result. EIP-8130 must agree across all consumers.
- **Derived block:** L1 source → derivation/origin → Engine payload → execution
  → SafeDB/status. Sequencing adds payload requests and unsafe gossip.
- **Batch:** L2 block source → encode/compress → blob/calldata submission → L1
  confirmation → derivation can reproduce the chain.
- **Proof:** agreed inputs → witness/preimages → backend → artifact → proposer
  or challenger → on-chain resolution.
- **Operator lifecycle:** configuration → service/actor ownership → metrics and
  status → restart, recovery, snapshot, or shutdown behavior.

## Reth integration boundary

Reth supplies generic Ethereum-node facilities. Base owns rollup policy and the
boundary that applies it.

| Reth facility | Base owns |
| --- | --- |
| Node/CLI | chain spec, node types, runtime extensions, binary wiring |
| DB/provider/trie | proof history, retention, custom witness/trie behavior |
| EVM/execution | Base EVM config, precompiles, native assets, L1 fees, EIP-8130 |
| Pool/payload | admission/order, metering, composition, sealing policy |
| RPC/Engine | Base namespaces, rollup Engine handling, trusted-proxy policy |
| P2P/discovery | rollup peer policy, unsafe gossip, telemetry |
| ExEx/metrics | shadow indexing, tracing, Base events, system-test adapters |

Use an upstream capability directly when it has the required contract. Add a
Base adapter only for Base policy, an externally visible Base contract, or an
upstream compatibility boundary.

## Product direction

Improve a user/operator-visible correctness, reliability, security, latency,
throughput, or resource-use outcome; complete a planned vertical slice; retire a
superseded path; or make production behavior reproducibly observable.

Current product areas: upgrade delivery; EIP-8130 and validity transactions;
native assets and policies; proof production/disputes; and node/operator
experience. Favor one canonical owner, deterministic feedback, supported-path
performance evidence, and complete ingress-to-execution-to-operations slices.

### Flashblocks to 200 ms blocks

Base is moving from Flashblocks to 200 ms blocks. The Flashblock builder is
scheduled for removal by **October 31, 2026**, after 200 ms blocks activate.
Sequencing work should prepare the 200 ms path, migrate callers/operators, and
remove replaced Flashblocks surface after migration is proven.

### Transition-system convergence

Retire pre-Holocene compatibility when the replacement is explicit and tested.
For upgrade, derivation, execution, and operator flows, define states, events,
legal/rejected transitions, recovery, and one canonical owner. Expose effective
state and transition/failure reasons through status, metrics, or structured logs.

## Change selection and evidence

Before a PR, state the outcome, preserved contract, roadmap fit, removed or
avoided surface, and focused evidence. A substantial change normally fixes a
reproduced cross-boundary failure, removes a complete obsolete production path,
consolidates a meaningful ownership boundary, or completes a supported feature.

Use the narrowest useful evidence: focused regression plus affected-package or
integration test; system/devnet test for cross-process behavior; and a
representative baseline/candidate workload for performance. Block-production
work also follows `docs/guides/BLOCK_PRODUCTION_REVIEW.md`.
