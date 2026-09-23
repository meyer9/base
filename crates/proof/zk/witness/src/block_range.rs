use std::{
    cmp::{max, min},
    collections::HashSet,
};

use anyhow::{Result, bail};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::{
    fetcher::{OPSuccinctDataFetcher, RPCMode},
    host::SuccinctHost,
    rpc_types::{OutputResponse, SafeHeadResponse},
};

/// Get the start and end block numbers for a range, with validation.
pub async fn get_validated_block_range(
    host: &SuccinctHost,
    start: Option<u64>,
    end: Option<u64>,
    default_range: u64,
) -> Result<(u64, u64)> {
    // Get the latest finalized block number when end block is not provided.
    // Even though the safeDB is activated, we use the finalized block number as the
    // end block by default to ensure the program doesn't run into L2 Block Validation
    // Failure error.
    // L2 Block Validation Failure error might still occur. See
    // [Troubleshooting](../troubleshooting.md#l2-block-validation-failure) for more details.
    let end_number = host.get_finalized_l2_block_number().await?;

    // If end block not provided, use latest finalized block
    let l2_end_block = match end {
        Some(end) => {
            if end > end_number {
                bail!(
                    "The end block ({end}) is greater than the latest finalized block ({end_number})"
                );
            }
            end
        }
        None => end_number,
    };

    // If start block not provided, use end block - default_range
    let l2_start_block =
        start.unwrap_or_else(|| max(1, l2_end_block.saturating_sub(default_range)));

    if l2_start_block >= l2_end_block {
        bail!("Start block ({l2_start_block}) must be less than end block ({l2_end_block})");
    }

    Ok((l2_start_block, l2_end_block))
}

/// Get a rolling block range whose end aligns with the host's finalized L2 block.
///
/// The returned tuple represents the last `range` blocks that the host considers finalized
/// according to its DA-specific logic, making the range safe to use for proof generation.
pub async fn get_rolling_block_range(host: &SuccinctHost, range: u64) -> Result<(u64, u64)> {
    let l2_end_block = host.get_finalized_l2_block_number().await?;

    Ok((l2_end_block.saturating_sub(range), l2_end_block))
}

/// A contiguous range of L2 blocks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanBatchRange {
    /// First block in the range.
    pub start: u64,
    /// Last block in the range (exclusive).
    pub end: u64,
}

/// Split a range of blocks into a list of span batch ranges.
///
/// This is a simple implementation used when the safeDB is not activated on the L2 Node.
///
/// # Errors
///
/// Returns an error when `max_range_size` is zero. A zero-sized range cannot advance the
/// cursor and would otherwise make proof planning loop forever.
pub fn split_range_basic(start: u64, end: u64, max_range_size: u64) -> Result<Vec<SpanBatchRange>> {
    if max_range_size == 0 {
        bail!("max range size must be greater than zero");
    }

    let mut ranges = Vec::new();
    let mut current_start = start;

    while current_start < end {
        let current_end = current_start.saturating_add(max_range_size).min(end);
        ranges.push(SpanBatchRange { start: current_start, end: current_end });
        current_start = current_end;
    }

    Ok(ranges)
}

/// Split a range of blocks into a list of span batch ranges based on L2 safeHeads.
///
/// 1. Get the L1 block range [L1 origin of `l2_start`, `L1Head`] where `L1Head` is the block from which
///    `l2_end` can be derived
/// 2. Loop over L1 blocks to get safeHead increases (batch posts) which form a step function
/// 3. Split ranges based on safeHead increases and max batch size
///
/// Example: If safeHeads are [27,49,90] and `max_size=30`, ranges will be [(0,27), (27,49), (49,69),
/// (69,90)]
pub async fn split_range_based_on_safe_heads(
    l2_start: u64,
    l2_end: u64,
    max_range_size: u64,
) -> Result<Vec<SpanBatchRange>> {
    if max_range_size == 0 {
        bail!("max range size must be greater than zero");
    }

    let data_fetcher = OPSuccinctDataFetcher::default();

    // Get the L1 origin of l2_start
    let l2_start_hex = format!("0x{l2_start:x}");
    let start_output: OutputResponse = data_fetcher
        .fetch_rpc_data_with_mode(
            RPCMode::L2Node,
            "optimism_outputAtBlock",
            vec![l2_start_hex.into()],
        )
        .await?;
    let l1_start = start_output.block_ref.l1_origin.number;

    // Get the L1Head from which l2_end can be derived
    let (_, l1_head_number) = data_fetcher.get_safe_l1_block_for_l2_block(l2_end).await?;

    // Get all the unique safeHeads between l1_start and l1_head
    let mut ranges = Vec::new();
    let mut current_l2_start = l2_start;
    let safe_heads = futures::stream::iter(l1_start..=l1_head_number)
        .map(|block| async move {
            let l1_block_hex = format!("0x{block:x}");
            let data_fetcher = OPSuccinctDataFetcher::default();
            let result: SafeHeadResponse = data_fetcher
                .fetch_rpc_data_with_mode(
                    RPCMode::L2Node,
                    "optimism_safeHeadAtL1Block",
                    vec![l1_block_hex.into()],
                )
                .await?;
            Ok::<_, anyhow::Error>(result.safe_head.number)
        })
        .buffered(15)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<HashSet<_>>>()?;

    // Collect and sort the safe heads.
    let mut safe_heads: Vec<_> = safe_heads.into_iter().collect();
    safe_heads.sort();

    // Loop over all of the safe heads and create ranges.
    for safe_head in safe_heads {
        if safe_head > current_l2_start && current_l2_start < l2_end {
            let range_end = min(l2_end, safe_head);
            ranges.extend(split_range_basic(current_l2_start, range_end, max_range_size)?);
            current_l2_start = safe_head;
        }
    }

    Ok(ranges)
}

#[cfg(test)]
mod tests {
    use super::{SpanBatchRange, split_range_basic};

    #[test]
    fn split_range_basic_rejects_zero_sized_ranges() {
        let error = split_range_basic(1, 10, 0).expect_err("zero-sized ranges must be rejected");

        assert_eq!(error.to_string(), "max range size must be greater than zero");
    }

    #[test]
    fn split_range_basic_partitions_the_requested_range() {
        let ranges = split_range_basic(10, 20, 4).expect("positive range size must succeed");

        assert_eq!(
            ranges,
            vec![
                SpanBatchRange { start: 10, end: 14 },
                SpanBatchRange { start: 14, end: 18 },
                SpanBatchRange { start: 18, end: 20 },
            ]
        );
    }

    #[test]
    fn split_range_basic_handles_the_upper_block_number_boundary() {
        let ranges = split_range_basic(u64::MAX - 2, u64::MAX, 4).expect("range must not overflow");

        assert_eq!(ranges, vec![SpanBatchRange { start: u64::MAX - 2, end: u64::MAX }]);
    }
}
