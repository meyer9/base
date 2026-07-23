# x402 Batch Settlement: Gas and Batch-Size Report

Date: July 22, 2026

## Summary

The execution-only ceiling depends strongly on receiver locality. The load tester intentionally assigns a distinct receiver to every channel, while a single merchant can have many payer channels that all update one shared receiver balance.

| State profile | Block allocation | Direct `claim` | `claimWithSignature` |
| --- | ---: | ---: | ---: |
| First claim, distinct receivers | 100% | **4,592 rows/s** | **4,321.5 rows/s** |
| First claim, distinct receivers | 30% | **1,378 rows/s** | **1,296 rows/s** |
| Steady state, distinct receivers | 100% | **7,564 rows/s** | **6,853 rows/s** |
| Steady state, distinct receivers | 30% | **2,262 rows/s** | **2,055 rows/s** |
| Steady state, one shared receiver | 100% | **9,230 rows/s** | **8,192 rows/s** |
| Steady state, one shared receiver | 30% | **2,760 rows/s** | **2,457 rows/s** |

The checked-in workload uses distinct receivers and advances cumulative claims through a ladder. Its first rung follows the conservative profile; later rungs follow the steady-state distinct-receiver profile. The shared-receiver figures are the maximum gas-only throughput measured.

These are settled voucher rows per second, not off-chain payment TPS; one cumulative row can represent many payments.

The practical default remains **100 rows per transaction**. Depending on receiver locality and call path, it is within 0.5–1.2% of the full-block optimum. Tuning the exact batch size mainly reduces unused gas at the end of a block.

## What was tested

The tests deployed `x402/contracts/evm/src/x402BatchSettlement.sol` directly from the x402 repository at revision `0a604079aca7b5a45a2e1620ba444e13982646c8`.

The deployment used the repository's normal `contracts/evm/foundry.toml` settings:

- Solidity 0.8.28
- Cancun EVM
- optimizer enabled
- 200 optimizer runs
- `via_ir = false`
- CBOR metadata disabled
- bytecode hash disabled

This is the same bytecode configuration available to a permissionless deployer following the x402 repository setup. No load-test fixture copy was used for the gas measurements.

The conservative sweep used:

- a distinct funded channel
- a distinct receiver
- an EOA payer authorizer
- an EOA receiver authorizer
- a valid payer-signed cumulative voucher
- the first state-changing claim on that channel

Two steady-state sweeps first claimed every channel in a setup transaction, then explicitly marked settlement storage cold before measuring the next cumulative claim. One kept receivers distinct, matching the load tester. The peak case reused one receiver and token, representative of one merchant claiming from many payer channels.

Measurements used pre-encoded calldata and a low-level call under Foundry's `--isolate` mode. `vm.lastCallGas().gasTotalUsed` therefore includes transaction intrinsic gas without charging the test contract's caller-side ABI encoding. Channel addresses, salts, and signatures used representative non-zero values.

The method was checked against real Anvil transaction receipts:

| Call | Rows | Isolated measurement | Receipt | Difference |
| --- | ---: | ---: | ---: | ---: |
| `claim` | 1 | 65,805 | 65,805 | 0 |
| `claim` | 100 | 4,361,144 | 4,361,228 | -84 |
| `claimWithSignature` | 1 | 77,724 | 77,748 | -24 |
| `claimWithSignature` | 100 | 4,629,550 | 4,629,646 | -96 |

The largest difference was 0.003%. Reported gas includes contract execution, the 21,000 transaction base cost, and calldata gas. It does not include Base's separate L1 data fee.

Every integer batch size through the transaction-cap boundary was measured. Whole transactions were then packed into a 400M-gas block and a 120M-gas (30%) allocation.

Exact packing optima are fixture-specific by a small amount because calldata gas depends on zero bytes in deployed addresses, salts, and signatures. The per-row trend and 100-row results are stable; a different deployment can move a gas-threshold optimum by one transaction.

## Claim batch-size results

| Rows | `claim` gas | Gas/row | `claimWithSignature` gas | Gas/row |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 65,805 | 65,805 | 77,724 | 77,724 |
| 10 | 455,988 | 45,599 | 490,120 | 49,012 |
| 50 | 2,190,797 | 43,816 | 2,326,307 | 46,526 |
| 100 | 4,361,144 | 43,611 | 4,629,550 | 46,296 |
| 150 | 6,533,501 | 43,557 | 6,941,690 | 46,278 |
| 200 | 8,708,264 | 43,541 | 9,263,088 | 46,315 |
| 250 | 10,884,941 | 43,540 | 11,593,298 | 46,373 |
| 300 | 13,063,556 | 43,545 | 13,932,297 | 46,441 |
| 350 | 15,244,121 | 43,555 | 16,280,133 | 46,515 |
| 400 | 17,426,469 | 43,566 | 18,636,639 | 46,592 |

These are first-claim, distinct-receiver costs. Most fixed-cost amortization is complete by 50 rows; gas per row is nearly flat thereafter.

At 100 rows, the state profiles compare as follows:

| State profile | `claim` gas | `claimWithSignature` gas |
| --- | ---: | ---: |
| First claim, distinct receivers | 4,361,204 | 4,629,610 |
| Steady state, distinct receivers | 2,651,120 | 2,919,526 |
| Steady state, one shared receiver | 2,176,148 | 2,444,554 |

### First claim versus channel reuse

The 1.71M-gas drop from the first to second 100-row distinct-receiver claim is 17,100 gas per row. That gap comes from receiver accounting, not from making the channel slot non-zero:

- `deposit` has already written `ChannelState.balance`. Because `balance` and `totalClaimed` share one packed storage slot, the first claim and later cumulative claims both update an already non-zero channel slot.
- The first claim writes `receivers[receiver][token].totalClaimed` from zero to non-zero.
- Reusing the channel changes that receiver slot from non-zero to non-zero, saving 17,100 gas for each distinct receiver row.
- If rows share a receiver and token, rows after the first also reuse the same warm receiver slot within the transaction. At 100 rows this saves another 474,972 gas relative to steady-state distinct receivers.

This distinction explains the cap change: first-claim batches stop at 385 direct or 360 signed rows, while already-seeded distinct-receiver channels reach 632 or 563 rows. A shared receiver reaches 769 or 667 rows. The current fixed-group load tester must still stay below the first-claim cap because it cannot reach a cheaper later rung if its initial batch is not includable.

`claimWithSignature` has two extra costs:

1. it hashes every row into the receiver-authorizer batch digest;
2. it verifies the receiver-authorizer signature.

The second cost is fixed, but the first scales with the number of rows. That is why the gap between the two call paths grows with batch size.

## Base transaction limit

Base currently caps ordinary transactions at 16,777,216 gas (`2^24`).

| State profile | Call | Largest batch below cap | Gas | Next batch | Gas |
| --- | --- | ---: | ---: | ---: | ---: |
| First claim, distinct | `claim` | 385 | 16,772,763 | 386 | 16,815,252 |
| First claim, distinct | `claimWithSignature` | 360 | 16,750,729 | 361 | 16,798,874 |
| Steady state, distinct | `claim` | 632 | 16,770,797 | 633 | 16,799,446 |
| Steady state, distinct | `claimWithSignature` | 563 | 16,754,630 | 564 | 16,783,652 |
| Steady state, shared | `claim` | 769 | 16,758,717 | 770 | 16,778,460 |
| Steady state, shared | `claimWithSignature` | 667 | 16,769,625 | 668 | 16,793,912 |

The load tester caps its submitted claim gas limit at `2^24`, so an oversized batch fails at the same boundary rather than requesting a transaction gas limit that Base will reject during validation.

The steady-state boundaries require seeding receiver balances in smaller first-claim transactions. Because the current load tester uses one fixed group size for every ladder rung, its configured batch must remain at or below the first-claim boundary.

For production use:

- **100 rows:** recommended default
- **244 direct / 154 signed rows:** load-test steady-state full-block optima
- **116 direct / 137 signed rows:** load-test steady-state 30%-allocation optima
- **260 direct / 128 signed rows:** shared-receiver full-block optima
- **115 direct / 126 signed rows:** shared-receiver 30%-allocation optima
- Transaction-cap boundaries are stress-test values only.

## Gas cost by action

Deposit, refund, and settlement measurements used a Base mainnet fork at block `48,939,200`, a freshly deployed canonical settlement contract, Base USDC, and the production Permit2 contract.

| Action | Total gas | Notes |
| --- | ---: | --- |
| ERC-3009 deposit | 166,592 | First deposit on a new channel, real Base USDC authorization |
| Permit2 deposit | 154,844 | First deposit on a new channel, real Permit2 witness transfer and Base USDC |
| `claim[1]` | 65,805 | Direct receiver-side claim, receipt-validated |
| `claimWithSignature[1]` | 77,748 | Relay-friendly claim, receipt-validated |
| `refund` | 63,314 | Direct receiver-side full refund using Base USDC |
| `settle` | 53,340 | One claimed balance transferred as Base USDC |

These are first-use or state-changing paths. Repeating an operation against already-warm state can be cheaper, while a no-op `settle` with nothing pending is not representative and was not included.

## Calldata

The exact encoded calldata lengths are:

```text
claim:              68 + 480 × rows bytes
claimWithSignature: 228 + 480 × rows bytes
```

Examples:

| Rows | `claim` calldata | `claimWithSignature` calldata |
| ---: | ---: | ---: |
| 1 | 548 bytes | 708 bytes |
| 10 | 4,868 bytes | 5,028 bytes |
| 50 | 24,068 bytes | 24,228 bytes |
| 100 | 48,068 bytes | 48,228 bytes |
| 250 | 120,068 bytes | 120,228 bytes |
| 400 | 192,068 bytes | 192,228 bytes |

The 160-byte difference is the outer dynamic signature argument. Both calls otherwise carry the same 480 bytes per row.

Raw calldata is a useful upper-bound proxy for data availability load, but it is not the same as bytes posted to L1. OP Stack batch compression should compress repeated addresses, zero padding, and similar channel fields. A production DA result should therefore include batcher output, not only transaction input size.

## What the result means for throughput

The first claim on fresh receiver balances fits 8,643–9,184 rows in a 400M-gas block. Once those balances are non-zero, the checked-in distinct-receiver workload fits 13,706–15,128 rows. Sharing one receiver raises the range to 16,384–18,460 rows.

That is not yet a full-stack x402 TPS number. A `VoucherClaim` row is the latest cumulative state for one channel. It can replace many earlier off-chain payment vouchers. If each on-chain row aggregates `A` payments, then:

```text
represented payment rate ≈ on-chain claim-row rate × A
```

For one million represented payments per second, the steady-state aggregation requirement is about 109–123 payments per row with a shared receiver, or 132–146 with distinct receivers. That factor must be measured in the resource-server workload; it cannot be inferred from this contract benchmark.

### Exact block-packing results

The following are gas-only ceilings for 400M-gas, two-second blocks. They use whole transactions and enforce Base's 16,777,216 per-transaction limit.

| State profile | Call | Allocation | Rows/tx | Gas/tx | Tx/block | Rows/block | Rows/s |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| First distinct | `claim` | 100% | 164 | 7,142,176 | 56 | 9,184 | **4,592** |
| First distinct | `claim` | 30% | 212 | 9,230,446 | 13 | 2,756 | **1,378** |
| First distinct | `claimWithSignature` | 100% | 129 | 5,969,772 | 67 | 8,643 | **4,321.5** |
| First distinct | `claimWithSignature` | 30% | 108 | 4,998,864 | 24 | 2,592 | **1,296** |
| Steady distinct | `claim` | 100% | 244 | 6,451,394 | 62 | 15,128 | **7,564** |
| Steady distinct | `claim` | 30% | 116 | 3,072,454 | 39 | 4,524 | **2,262** |
| Steady distinct | `claimWithSignature` | 100% | 154 | 4,493,772 | 89 | 13,706 | **6,853** |
| Steady distinct | `claimWithSignature` | 30% | 137 | 3,997,346 | 30 | 4,110 | **2,055** |
| Steady shared | `claim` | 100% | 260 | 5,631,652 | 71 | 18,460 | **9,230** |
| Steady shared | `claim` | 30% | 115 | 2,499,472 | 48 | 5,520 | **2,760** |
| Steady shared | `claimWithSignature` | 100% | 128 | 3,124,991 | 128 | 16,384 | **8,192** |
| Steady shared | `claimWithSignature` | 30% | 126 | 3,076,304 | 39 | 4,914 | **2,457** |

At 100 rows per transaction, full-block throughput is 7,500 direct or 6,850 signed rows/s for the steady distinct-receiver workload, and 9,150 direct or 8,150 signed rows/s with a shared receiver. The simpler 100-row default is close to every steady-state optimum.

Raw calldata at the full-block optima is approximately 6.6–7.3 MB for the steady distinct-receiver workload and 7.9–8.9 MB for the shared-receiver peak before OP Stack compression. Execution gas is not automatically the final bottleneck; DA and state-root processing still need independent measurements.

The final system limit is the minimum of:

- off-chain voucher creation and validation;
- facilitator `/verify` capacity;
- transaction construction and submission;
- sequencer execution;
- state-root processing;
- compressed DA throughput.

## Mixed-workload devnet result

The checked-in devnet workload was rerun with 10 senders, eight rows per signed claim, a 20M gas/s target, and the 90/5/4/1 transaction mix. The 30-second generation window produced 1,780 transactions:

| Action | Transactions | Claim rows |
| --- | ---: | ---: |
| `claimWithSignature` | 1,595 | 12,760 |
| ERC-3009 deposit | 101 | — |
| `settle` | 71 | — |
| `refund` | 13 | — |

All 1,780 transactions confirmed, with no submission failures or reverts. The observed aggregate rate was 37.08 tx/s and 9.04M gas/s. Applying the exact claim share to that observed rate gives 33.23 signed claim transactions/s, or **265.8 confirmed claim rows/s**. The transactions carried 6,558,480 bytes of calldata in total, averaging 3,684.5 bytes per transaction.

| Inclusion metric | Minimum | p50 | Mean | p95 | p99 | Maximum |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Block | 670.9 ms | 2.9 s | 6.6 s | 28.3 s | 37.5 s | 40.1 s |
| Flashblock | 95.4 ms | 1.0 s | 5.2 s | 26.8 s | 35.4 s | 36.9 s |

This run validates transaction construction, ordering, execution, and receipt accounting. It is not a production capacity result: the recovered HA devnet's follower remained at genesis, so the healthy builder was used for both submission and queries. The high tail latency reflects that local environment and should not be used as a Base mainnet latency estimate.

## Measurements still required for a full-stack TPS claim

The devnet batcher exposed input, compressed-output, and submitted-DA counters, but it detected a chain reorganization during the measurement and reset with 1,275 pending blocks and 317 ready channels. That invalidates a before/after compression delta. No compressed-DA ceiling is reported here; it needs a clean, stable run with counter snapshots bracketing only the measured workload.

Facilitator `/verify` was not exercised by this on-chain workload. The conservative path performs signature validation and reads current channel state over RPC. An optimistic server can validate vouchers locally and periodically resynchronize. Those modes need a separate HTTP benchmark with valid voucher payloads, controlled RPC latency, and an explicit resynchronization interval.

The workload also does not model how many off-chain payments are represented by one cumulative voucher row. Until a resource-server test records that aggregation factor, settlement rows/s must not be presented as payment TPS.

## Sources

- x402 source: `contracts/evm/src/x402BatchSettlement.sol`
- x402 build settings: `contracts/evm/foundry.toml`
- Base, [Throughput and Limits](https://docs.base.org/base-chain/network-information/throughput-and-limits)
- Base, [Transaction Ordering](https://docs.base.org/base-chain/network-information/block-building)
- Base Azul, [Execution Engine Changes](https://docs.base.org/base-chain/specs/upgrades/azul/exec-engine)
