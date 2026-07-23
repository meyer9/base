# b20-state-populate

Offline tool for seeding a Reth MDBX database with large token balance state and the
corresponding trie nodes. Supports both:

- B20 precompile token storage (derived from `--creator` + `--salt`), and
- arbitrary EVM contract storage mapping slots (`--contract`, `--mapping-slot`).

## Subcommands

| Command | Description |
|---------|-------------|
| `populate` | Write balance slots, hashed storage, and trie nodes for a token contract |
| `verify` | Read-back a sample of slots and report row counts |

## Usage

```bash
# Seed 700 million balances into the B20 precompile token derived from creator+salt
b20-state-populate populate \
  --datadir /home/meyer9/snapshots/base-mainnet-b20-bench \
  --creator 0xABCDEF0000000000000000000000000000000000 \
  --salt    0x0000000000000000000000000000000000000000000000000000000000000000 \
  --count   700000000 \
  --balance 1000000000000000000

# Seed 10 million balances into an existing EVM contract mapping at slot 9
b20-state-populate populate \
  --datadir /home/meyer9/snapshots/base-mainnet-b20-bench \
  --contract 0xD5409af0AA7Ee0cB7fF1375FCecECDdbf75febA3 \
  --mapping-slot 9 \
  --count 10000000 \
  --balance 1000000000000000000

# Verify the written data
b20-state-populate verify \
  --datadir /home/meyer9/snapshots/base-mainnet-b20-bench \
  --creator 0xABCDEF0000000000000000000000000000000000 \
  --salt    0x0000000000000000000000000000000000000000000000000000000000000000 \
  --count   700000000

# Verify the generic EVM contract mapping population
b20-state-populate verify \
  --datadir /home/meyer9/snapshots/base-mainnet-b20-bench \
  --contract 0xD5409af0AA7Ee0cB7fF1375FCecECDdbf75febA3 \
  --mapping-slot 9 \
  --count 10000000
```

## Argument behavior

- `--contract` is optional. If provided, the tool targets that contract directly.
- If `--contract` is omitted, both `--creator` and `--salt` are required and the tool
  uses the B20 precompile derivation path.
- `--mapping-slot` defaults to `0` (standard ERC-20 `_balances` layout) and applies to
  `--contract` mode.
- In `--contract` mode, precompile metadata slots and bytecode deployment are skipped.

## Design

- All writes go to `PlainStorageState`, `HashedStorages`, `PlainAccountState`, and
  `HashedAccounts`.
- Storage trie nodes are computed via `StorageRoot::from_tx_hashed` and written to
  `StoragesTrie` (one pass over `HashedStorages` for the token address).
- The account trie is updated via `StateRoot::overlay_root_with_updates` (modifies only
  the single path for the token address in `AccountsTrie`).
- Writes are batched in chunks of 1 M to keep transactions manageable.
