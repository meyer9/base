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
/// @dev `FiatTokenV2_2` is deployed via CREATE2 through the Foundry deterministic deployer
///      (0x4e59b44847b379578588920cA78FbF26c0B4956C, pre-deployed on Anvil) with a fixed salt so
///      its address is stable across fresh devnet runs. This lets `base-state-populate` be pointed
///      at the token address before the devnet starts to pre-warm its `_balances` storage trie with
///      10M slots, ensuring benchmark runs hit a realistically deep trie.
///
///      `x402BatchSettlement` and `ERC3009DepositCollector` use plain CREATE; they are not
///      pre-populated and their trie depth grows naturally during the load test.
///
///      The broadcasting funder becomes the token's `masterMinter` and is configured as a minter,
///      so the load-test setup can later `mint` USDC to each sender. Prints `Token:`,
///      `Settlement:`, `Collector:`, and `TokenSalt:` lines parsed by the Justfile target.
contract DeployBatchSettlementDevnet is Script {
    /// @dev CREATE2 salt for `FiatTokenV2_2`. Any non-zero value works; this one is fixed so the
    ///      deployed address never changes across devnet restarts.
    bytes32 internal constant TOKEN_SALT = bytes32(uint256(0x78343032546f6b656e53616c74000000000000000000000000000000000000));

    /// @dev Minter allowance large enough to cover any load-test mint volume.
    uint256 internal constant MINTER_ALLOWANCE = type(uint256).max;

    function run() public {
        vm.startBroadcast();

        FiatTokenV2_2 token = new FiatTokenV2_2{salt: TOKEN_SALT}();
        token.configureMinter(msg.sender, MINTER_ALLOWANCE);

        x402BatchSettlement settlement = new x402BatchSettlement();
        ERC3009DepositCollector collector = new ERC3009DepositCollector(address(settlement));

        vm.stopBroadcast();

        console.log("Token:", address(token));
        console.log("Settlement:", address(settlement));
        console.log("Collector:", address(collector));
        console.log("Minter:", msg.sender);
        console.log("TokenSalt:", uint256(TOKEN_SALT));
    }
}
