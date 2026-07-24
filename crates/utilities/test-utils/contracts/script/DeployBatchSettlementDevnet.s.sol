// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import {Script} from "forge-std/Script.sol";
import {console} from "forge-std/console.sol";
import {FiatTokenV2_2} from "../src/FiatTokenV2_2.sol";
import {x402BatchSettlement} from "../src/x402/x402BatchSettlement.sol";
import {ERC3009DepositCollector} from "../src/x402/periphery/ERC3009DepositCollector.sol";

/// @title DeployBatchSettlementDevnet
/// @notice Deploys the x402 batch-settlement load-test devnet harness: a USDC-like ERC-3009 token
///         fixture, the `x402BatchSettlement` contract, and the `ERC3009DepositCollector`.
///
/// @dev All three contracts are deployed via CREATE2 through the Foundry deterministic deployer
///      (0x4e59b44847b379578588920cA78FbF26c0B4956C, pre-deployed on Anvil) with fixed salts so
///      every address is stable across fresh devnet restarts. This lets the load-test setup code
///      check for existing deployments and skip re-deployment, and allows `base-state-populate` to
///      pre-warm the token's `_balances` storage trie with 10M slots before the devnet starts.
///
///      The broadcasting funder becomes the token's `masterMinter` and is configured as a minter,
///      so the load-test setup can later `mint` USDC to each sender. Prints `Token:`,
///      `Settlement:`, and `Collector:` lines parsed by the Justfile target.
///
///      Re-running this script against an already-deployed devnet is a no-op: CREATE2 reverts if
///      the address is already occupied, so Foundry's `--skip-simulation` flag is not needed and
///      partial re-runs are safe.
contract DeployBatchSettlementDevnet is Script {
    /// @dev CREATE2 salt for `FiatTokenV2_2`. Fixed so the token address is stable.
    bytes32 internal constant TOKEN_SALT =
        bytes32(uint256(0x78343032546f6b656e53616c74000000000000000000000000000000000000));

    /// @dev CREATE2 salt for `x402BatchSettlement`. Fixed so the settlement address is stable.
    bytes32 internal constant SETTLEMENT_SALT =
        bytes32(uint256(0x783430324261746368536574746c656d656e7453616c74000000000000000000));

    /// @dev CREATE2 salt for `ERC3009DepositCollector`. Fixed so the collector address is stable.
    bytes32 internal constant COLLECTOR_SALT =
        bytes32(uint256(0x7834303244657036436f6c6c6563746f7253616c740000000000000000000000));

    /// @dev Minter allowance large enough to cover any load-test mint volume.
    uint256 internal constant MINTER_ALLOWANCE = type(uint256).max;

    function run() public {
        vm.startBroadcast();

        // Pass msg.sender explicitly so masterMinter is the EOA broadcaster, not the CREATE2
        // factory (which would be the case if msg.sender were used inside the constructor).
        FiatTokenV2_2 token = new FiatTokenV2_2{salt: TOKEN_SALT}(msg.sender);
        token.configureMinter(msg.sender, MINTER_ALLOWANCE);

        x402BatchSettlement settlement = new x402BatchSettlement{salt: SETTLEMENT_SALT}();

        // Collector salt encodes the settlement address into the CREATE2 call so the collector
        // address is stable given a stable settlement address.
        ERC3009DepositCollector collector =
            new ERC3009DepositCollector{salt: COLLECTOR_SALT}(address(settlement));

        vm.stopBroadcast();

        console.log("Token:", address(token));
        console.log("Settlement:", address(settlement));
        console.log("Collector:", address(collector));
        console.log("Minter:", msg.sender);
    }
}
