# `base-common-chains`

Static configuration for supported Base networks and their execution upgrade schedules.

## Overview

`ChainConfig` provides the chain IDs, genesis data, protocol parameters, contract addresses, and
embedded genesis JSON for Base mainnet, Base Sepolia, Base Zeronet, and the local devnet.
`ChainUpgrades` exposes each network's activation conditions through the `Upgrades` trait.

## Usage

```toml
[dependencies]
base-common-chains = { workspace = true }
```

```rust
use base_common_chains::{ChainConfig, ChainUpgrades, Upgrades};

let mainnet = ChainConfig::mainnet();
assert_eq!(mainnet.chain_id, 8453);
assert!(ChainUpgrades::mainnet().is_canyon_active_at_timestamp(mainnet.canyon_timestamp));
```

## License

Licensed under the [MIT License](https://github.com/base/base/blob/main/LICENSE).
