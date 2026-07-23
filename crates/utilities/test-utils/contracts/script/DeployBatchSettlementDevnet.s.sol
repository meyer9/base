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
/// @dev Uses plain `CREATE` (not the vanity CREATE2 addresses of x402's production deploy) so the
///      logged addresses are the actual runtime addresses on a fresh devnet. The broadcasting funder
///      becomes the token's `masterMinter` and is configured as a minter, so the load-test setup can
///      later `mint` USDC to each sender. Prints `Token:`, `Settlement:`, and `Collector:` lines that
///      the `just load-test batch-settlement` target parses to render the run config.
contract DeployBatchSettlementDevnet is Script {
    /// @dev Minter allowance large enough to cover any load-test mint volume.
    uint256 internal constant MINTER_ALLOWANCE = type(uint256).max;

    function run() public {
        vm.startBroadcast();

        FiatTokenV2_2 token = new FiatTokenV2_2();
        token.configureMinter(msg.sender, MINTER_ALLOWANCE);

        x402BatchSettlement settlement = new x402BatchSettlement();
        ERC3009DepositCollector collector = new ERC3009DepositCollector(address(settlement));

        vm.stopBroadcast();

        console.log("Token:", address(token));
        console.log("Settlement:", address(settlement));
        console.log("Collector:", address(collector));
        console.log("Minter:", msg.sender);
    }
}
