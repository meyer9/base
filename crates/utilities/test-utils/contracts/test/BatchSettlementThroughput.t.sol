// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {Test} from "forge-std/Test.sol";

/// @notice Locks the block-packing arithmetic used by the batch-settlement report.
/// @dev Gas inputs come from the exhaustive canonical x402 Foundry sweep documented in REPORT.md.
contract BatchSettlementThroughputTest is Test {
    uint256 internal constant FULL_BLOCK_GAS = 400_000_000;
    uint256 internal constant THIRTY_PERCENT_BLOCK_GAS = 120_000_000;
    uint256 internal constant TRANSACTION_GAS_LIMIT = 16_777_216;

    function test_first_claim_distinct_receiver_direct_throughput() public pure {
        uint256 fullBlockGas = 7_142_176;
        uint256 thirtyPercentGas = 9_230_446;

        assertEq(_transactionsPerBlock(FULL_BLOCK_GAS, fullBlockGas), 56);
        assertEq(_rowsPerBlock(FULL_BLOCK_GAS, fullBlockGas, 164), 9_184);

        assertEq(_transactionsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas), 13);
        assertEq(_rowsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas, 212), 2_756);
    }

    function test_first_claim_distinct_receiver_signed_throughput() public pure {
        uint256 fullBlockGas = 5_969_772;
        uint256 thirtyPercentGas = 4_998_864;

        assertEq(_transactionsPerBlock(FULL_BLOCK_GAS, fullBlockGas), 67);
        assertEq(_rowsPerBlock(FULL_BLOCK_GAS, fullBlockGas, 129), 8_643);

        assertEq(_transactionsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas), 24);
        assertEq(_rowsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas, 108), 2_592);
    }

    function test_steady_distinct_receiver_direct_throughput() public pure {
        uint256 fullBlockGas = 6_451_394;
        uint256 thirtyPercentGas = 3_072_454;

        assertEq(_transactionsPerBlock(FULL_BLOCK_GAS, fullBlockGas), 62);
        assertEq(_rowsPerBlock(FULL_BLOCK_GAS, fullBlockGas, 244), 15_128);

        assertEq(_transactionsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas), 39);
        assertEq(_rowsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas, 116), 4_524);
    }

    function test_steady_distinct_receiver_signed_throughput() public pure {
        uint256 fullBlockGas = 4_493_772;
        uint256 thirtyPercentGas = 3_997_346;

        assertEq(_transactionsPerBlock(FULL_BLOCK_GAS, fullBlockGas), 89);
        assertEq(_rowsPerBlock(FULL_BLOCK_GAS, fullBlockGas, 154), 13_706);

        assertEq(_transactionsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas), 30);
        assertEq(_rowsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas, 137), 4_110);
    }

    function test_steady_shared_receiver_direct_throughput() public pure {
        uint256 fullBlockGas = 5_631_652;
        uint256 thirtyPercentGas = 2_499_472;

        assertEq(_transactionsPerBlock(FULL_BLOCK_GAS, fullBlockGas), 71);
        assertEq(_rowsPerBlock(FULL_BLOCK_GAS, fullBlockGas, 260), 18_460);

        assertEq(_transactionsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas), 48);
        assertEq(_rowsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas, 115), 5_520);
    }

    function test_steady_shared_receiver_signed_throughput() public pure {
        uint256 fullBlockGas = 3_124_991;
        uint256 thirtyPercentGas = 3_076_304;

        assertEq(_transactionsPerBlock(FULL_BLOCK_GAS, fullBlockGas), 128);
        assertEq(_rowsPerBlock(FULL_BLOCK_GAS, fullBlockGas, 128), 16_384);

        assertEq(_transactionsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas), 39);
        assertEq(_rowsPerBlock(THIRTY_PERCENT_BLOCK_GAS, thirtyPercentGas, 126), 4_914);
    }

    function test_measured_transaction_cap_boundaries() public pure {
        assertLe(16_772_763, TRANSACTION_GAS_LIMIT); // distinct direct, 385 rows
        assertGt(16_815_252, TRANSACTION_GAS_LIMIT); // distinct direct, 386 rows
        assertLe(16_750_729, TRANSACTION_GAS_LIMIT); // distinct signed, 360 rows
        assertGt(16_798_874, TRANSACTION_GAS_LIMIT); // distinct signed, 361 rows
        assertLe(16_770_797, TRANSACTION_GAS_LIMIT); // steady distinct direct, 632 rows
        assertGt(16_799_446, TRANSACTION_GAS_LIMIT); // steady distinct direct, 633 rows
        assertLe(16_754_630, TRANSACTION_GAS_LIMIT); // steady distinct signed, 563 rows
        assertGt(16_783_652, TRANSACTION_GAS_LIMIT); // steady distinct signed, 564 rows
        assertLe(16_758_717, TRANSACTION_GAS_LIMIT); // steady shared direct, 769 rows
        assertGt(16_778_460, TRANSACTION_GAS_LIMIT); // steady shared direct, 770 rows
        assertLe(16_769_625, TRANSACTION_GAS_LIMIT); // steady shared signed, 667 rows
        assertGt(16_793_912, TRANSACTION_GAS_LIMIT); // steady shared signed, 668 rows
    }

    function _transactionsPerBlock(uint256 blockGas, uint256 transactionGas) internal pure returns (uint256) {
        return blockGas / transactionGas;
    }

    function _rowsPerBlock(uint256 blockGas, uint256 transactionGas, uint256 batchSize)
        internal
        pure
        returns (uint256)
    {
        return _transactionsPerBlock(blockGas, transactionGas) * batchSize;
    }
}
