//! Concentrated liquidity math: bin-walk swap and bin distribution.
//!
//! Bins are discrete price buckets. Each bin holds liquidity_a (RWT) and liquidity_b (USDC).
//! Below active_bin: only liquidity_b (bid side). Above: only liquidity_a (ask side).
//! Active bin: both tokens.

use arlex_lang::prelude::*;

use crate::constants::*;
use crate::error::DexError;
use crate::state::BinArray;

/// Bin-walk swap for concentrated pools.
///
/// Walks through bins from active_bin outward, consuming liquidity.
/// Updates active_bin_id as bins are exhausted.
///
/// Rounding: deductions from `remaining` rounded UP per-bin (pool keeps more),
/// `total_out` rounded DOWN (user gets less). Protocol-favored.
///
/// All intermediate math uses u128 to prevent overflow.
/// Returns (amount_out, remaining_input) so callers can track unconsumed input.
pub fn bin_walk_swap(
    bin_array: &mut BinArray,
    bin_step_bps: u16,
    net_input: u64,
    a_to_b: bool,
) -> core::result::Result<(u64, u64), ProgramError> {
    let mut remaining = net_input as u128;
    let mut total_out: u128 = 0;
    let mut current_bin = bin_array.active_bin_id;

    while remaining > 0 {
        // Bounds check BEFORE array indexing
        if current_bin < bin_array.lower_bin_id {
            break;
        }
        let upper_limit = bin_array.lower_bin_id + MAX_BINS as i32 - 1;
        if current_bin > upper_limit {
            break;
        }

        let bin_idx = (current_bin - bin_array.lower_bin_id) as usize;
        let bin = &mut bin_array.bins[bin_idx];

        let price = arlex_lang::math::pow_bps(bin_step_bps, current_bin)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;

        // Guard against price == 0 (extreme negative exponents)
        if price == 0 {
            break;
        }

        if a_to_b {
            // Selling RWT for USDC: consume liquidity_b (USDC)
            let available = bin.liquidity_b as u128;
            if available == 0 {
                current_bin -= 1;
                continue;
            }
            // consumable_usdc = min(remaining_rwt * price / SCALE, available_usdc)
            let consumable = core::cmp::min(
                arlex_lang::math::checked_mul_div_u128(remaining, price, CONCENTRATED_SCALE)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?,
                available,
            );
            if consumable == 0 {
                break;
            }
            // rwt_consumed = ceil(consumable_usdc * SCALE / price) — pool keeps more (M-8)
            let rwt_consumed = arlex_lang::math::checked_mul_div_u128_round_up(consumable, CONCENTRATED_SCALE, price)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            let rwt_consumed = core::cmp::min(rwt_consumed, remaining);

            // Safe u64 conversion (consumable <= available which is u64, rwt_consumed <= remaining which started as u64)
            let consumable_u64 = u64::try_from(consumable).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
            let rwt_consumed_u64 = u64::try_from(rwt_consumed).map_err(|_| ProgramError::from(DexError::MathOverflow))?;

            total_out += consumable;
            remaining -= rwt_consumed;
            bin.liquidity_b = bin.liquidity_b.checked_sub(consumable_u64)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            bin.liquidity_a = bin.liquidity_a.checked_add(rwt_consumed_u64)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;

            if bin.liquidity_b == 0 {
                current_bin -= 1;
            }
        } else {
            // Buying RWT with USDC: consume liquidity_a (RWT)
            let available = bin.liquidity_a as u128;
            if available == 0 {
                current_bin += 1;
                continue;
            }
            // consumable_rwt = min(remaining_usdc * SCALE / price, available_rwt)
            let consumable = core::cmp::min(
                arlex_lang::math::checked_mul_div_u128(remaining, CONCENTRATED_SCALE, price)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?,
                available,
            );
            if consumable == 0 {
                break;
            }
            // usdc_cost = ceil(consumable_rwt * price / SCALE) — pool keeps more (M-8)
            let usdc_cost = arlex_lang::math::checked_mul_div_u128_round_up(consumable, price, CONCENTRATED_SCALE)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            let usdc_cost = core::cmp::min(usdc_cost, remaining);

            let consumable_u64 = u64::try_from(consumable).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
            let usdc_cost_u64 = u64::try_from(usdc_cost).map_err(|_| ProgramError::from(DexError::MathOverflow))?;

            total_out += consumable;
            remaining -= usdc_cost;
            bin.liquidity_a = bin.liquidity_a.checked_sub(consumable_u64)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            bin.liquidity_b = bin.liquidity_b.checked_add(usdc_cost_u64)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;

            if bin.liquidity_a == 0 {
                current_bin += 1;
            }
        }
    }

    bin_array.active_bin_id = current_bin;

    if total_out == 0 {
        return Err(ProgramError::from(DexError::InsufficientBinLiquidity));
    }

    let out = u64::try_from(total_out).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
    let rem = u64::try_from(remaining).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
    Ok((out, rem))
}

/// Sync fee_lp into the active bin so sum(bins) matches reserves.
///
/// After bin_walk_swap, fee_lp stays in the vault but isn't tracked in any bin.
/// This adds fee_lp to the RWT-side of the active bin to maintain bin/reserve parity.
pub fn sync_fee_lp_to_bin(
    bin_array: &mut BinArray,
    fee_lp: u64,
    token_a_is_rwt: bool,
) -> core::result::Result<(), ProgramError> {
    if fee_lp == 0 {
        return Ok(());
    }
    let active = bin_array.active_bin_id;
    let lower = bin_array.lower_bin_id;
    if active < lower || active > lower + MAX_BINS as i32 - 1 {
        // Active bin out of range — fee_lp tracked in reserves but not in bins.
        // This is a minor discrepancy that shift_liquidity will resolve.
        return Ok(());
    }
    let idx = (active - lower) as usize;
    if token_a_is_rwt {
        bin_array.bins[idx].liquidity_a = bin_array.bins[idx].liquidity_a
            .checked_add(fee_lp).ok_or(ProgramError::from(DexError::MathOverflow))?;
    } else {
        bin_array.bins[idx].liquidity_b = bin_array.bins[idx].liquidity_b
            .checked_add(fee_lp).ok_or(ProgramError::from(DexError::MathOverflow))?;
    }
    Ok(())
}

/// Sync unconsumed swap input into active bin so bins match reserves.
///
/// On partial fills (bin liquidity exhausted), `remaining` input tokens sit in vault
/// but aren't tracked in bins. Adding them to active bin maintains bin/reserve parity.
/// The input side depends on swap direction: a_to_b means input is token_a.
pub fn sync_remaining_to_bin(
    bin_array: &mut BinArray,
    remaining: u64,
    a_to_b: bool,
) -> core::result::Result<(), ProgramError> {
    if remaining == 0 {
        return Ok(());
    }
    let active = bin_array.active_bin_id;
    let lower = bin_array.lower_bin_id;
    if active < lower || active > lower + MAX_BINS as i32 - 1 {
        return Ok(());
    }
    let idx = (active - lower) as usize;
    if a_to_b {
        // Input is token_a (RWT side for a_to_b when token_a is RWT)
        bin_array.bins[idx].liquidity_a = bin_array.bins[idx].liquidity_a
            .checked_add(remaining).ok_or(ProgramError::from(DexError::MathOverflow))?;
    } else {
        // Input is token_b
        bin_array.bins[idx].liquidity_b = bin_array.bins[idx].liquidity_b
            .checked_add(remaining).ok_or(ProgramError::from(DexError::MathOverflow))?;
    }
    Ok(())
}

/// Proportionally reduce all bin liquidity for remove_liquidity.
///
/// When LPs withdraw, reserves decrease. Bins must decrease proportionally
/// so subsequent swaps don't try to consume more than vault holds.
pub fn proportional_bin_remove(
    bin_array: &mut BinArray,
    fraction_numerator: u128,
    fraction_denominator: u128,
) -> core::result::Result<(), ProgramError> {
    if fraction_denominator == 0 {
        return Err(ProgramError::from(DexError::MathOverflow));
    }
    // remaining_fraction = (denominator - numerator) / denominator
    let remaining_num = fraction_denominator.checked_sub(fraction_numerator)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    // Capture pre-removal totals for dust reconciliation
    let mut pre_total_a: u128 = 0;
    let mut pre_total_b: u128 = 0;
    for i in 0..MAX_BINS {
        pre_total_a += bin_array.bins[i].liquidity_a as u128;
        pre_total_b += bin_array.bins[i].liquidity_b as u128;
    }

    // Expected post-removal totals (what reserves will show)
    let expected_a = arlex_lang::math::checked_mul_div_u128(pre_total_a, remaining_num, fraction_denominator)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    let expected_b = arlex_lang::math::checked_mul_div_u128(pre_total_b, remaining_num, fraction_denominator)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    // Reduce each bin proportionally (floor rounding per bin)
    for i in 0..MAX_BINS {
        let new_a = arlex_lang::math::checked_mul_div_u128(
            bin_array.bins[i].liquidity_a as u128,
            remaining_num,
            fraction_denominator,
        ).ok_or(ProgramError::from(DexError::MathOverflow))?;
        let new_b = arlex_lang::math::checked_mul_div_u128(
            bin_array.bins[i].liquidity_b as u128,
            remaining_num,
            fraction_denominator,
        ).ok_or(ProgramError::from(DexError::MathOverflow))?;
        bin_array.bins[i].liquidity_a = u64::try_from(new_a)
            .map_err(|_| ProgramError::from(DexError::MathOverflow))?;
        bin_array.bins[i].liquidity_b = u64::try_from(new_b)
            .map_err(|_| ProgramError::from(DexError::MathOverflow))?;
    }

    // Reconcile rounding dust: per-bin floor produces sum(bins) <= expected.
    // Add dust to active bin so sum(bins) == expected (which matches reserves).
    let mut actual_a: u128 = 0;
    let mut actual_b: u128 = 0;
    for i in 0..MAX_BINS {
        actual_a += bin_array.bins[i].liquidity_a as u128;
        actual_b += bin_array.bins[i].liquidity_b as u128;
    }
    let dust_a = expected_a.saturating_sub(actual_a);
    let dust_b = expected_b.saturating_sub(actual_b);

    let active = bin_array.active_bin_id;
    let lower = bin_array.lower_bin_id;
    if active >= lower && active <= lower + MAX_BINS as i32 - 1 {
        let idx = (active - lower) as usize;
        if dust_a > 0 {
            let d = u64::try_from(dust_a).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
            bin_array.bins[idx].liquidity_a = bin_array.bins[idx].liquidity_a
                .checked_add(d).ok_or(ProgramError::from(DexError::MathOverflow))?;
        }
        if dust_b > 0 {
            let d = u64::try_from(dust_b).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
            bin_array.bins[idx].liquidity_b = bin_array.bins[idx].liquidity_b
                .checked_add(d).ok_or(ProgramError::from(DexError::MathOverflow))?;
        }
    }

    Ok(())
}

/// Distribute deposited tokens across active bins.
///
/// First add: uniform distribution. Bid-side bins get per_bin_b (USDC),
/// ask-side bins get per_bin_a (RWT), active bin gets both.
/// Per-bin amounts calculated using per-side counts (not total bin count).
/// Subsequent: proportional to existing bin weights.
/// Remainder always goes to active_bin (conservation).
pub fn distribute_to_bins(
    bin_array: &mut BinArray,
    amount_a: u64,
    amount_b: u64,
    is_first: bool,
) -> core::result::Result<(), ProgramError> {
    let lower = bin_array.lower_bin_id;
    let active = bin_array.active_bin_id;
    let upper = lower + MAX_BINS as i32 - 1;

    // Find active range (bins with liquidity, or full range if first add)
    let (active_lower, active_upper) = if is_first {
        (lower, upper)
    } else {
        let mut al = upper + 1;
        let mut au = lower - 1;
        for bin_id in lower..=upper {
            let idx = (bin_id - lower) as usize;
            let b = &bin_array.bins[idx];
            if b.liquidity_a > 0 || b.liquidity_b > 0 {
                if bin_id < al { al = bin_id; }
                if bin_id > au { au = bin_id; }
            }
        }
        if al > au {
            (lower, upper)
        } else {
            (al, au)
        }
    };

    // Track pre-distribution totals for remainder calculation (checked arithmetic)
    let mut pre_total_a: u64 = 0;
    let mut pre_total_b: u64 = 0;
    for i in 0..MAX_BINS {
        pre_total_a = pre_total_a.checked_add(bin_array.bins[i].liquidity_a)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        pre_total_b = pre_total_b.checked_add(bin_array.bins[i].liquidity_b)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }

    if is_first {
        // Calculate per-side bin counts for proper distribution
        // bid_count: bins below active (USDC only)
        // ask_count: bins above active (RWT only)
        // active bin receives both tokens
        let bid_count = core::cmp::max((active - active_lower) as u64, 0) + 1; // +1 for active bin
        let ask_count = core::cmp::max((active_upper - active) as u64, 0) + 1; // +1 for active bin

        let per_bin_b = amount_b / bid_count;  // USDC per bid-side + active bin
        let per_bin_a = amount_a / ask_count;  // RWT per ask-side + active bin

        for bin_id in active_lower..=active_upper {
            let idx = (bin_id - lower) as usize;
            if bin_id < active {
                bin_array.bins[idx].liquidity_b = bin_array.bins[idx].liquidity_b
                    .checked_add(per_bin_b).ok_or(ProgramError::from(DexError::MathOverflow))?;
            } else if bin_id > active {
                bin_array.bins[idx].liquidity_a = bin_array.bins[idx].liquidity_a
                    .checked_add(per_bin_a).ok_or(ProgramError::from(DexError::MathOverflow))?;
            } else {
                bin_array.bins[idx].liquidity_a = bin_array.bins[idx].liquidity_a
                    .checked_add(per_bin_a).ok_or(ProgramError::from(DexError::MathOverflow))?;
                bin_array.bins[idx].liquidity_b = bin_array.bins[idx].liquidity_b
                    .checked_add(per_bin_b).ok_or(ProgramError::from(DexError::MathOverflow))?;
            }
        }
    } else {
        // Proportional to existing per-side weights.
        // Bid bins (USDC side) weighted by liquidity_b only.
        // Ask bins (RWT side) weighted by liquidity_a only.
        // Active bin weighted by both for both tokens.
        let mut total_weight_b: u128 = 0;
        let mut total_weight_a: u128 = 0;
        for bin_id in active_lower..=active_upper {
            let idx = (bin_id - lower) as usize;
            if bin_id <= active {
                // M-7: checked_add for defense-in-depth (overflow impossible at 10 * u64)
                total_weight_b = total_weight_b
                    .checked_add(bin_array.bins[idx].liquidity_b as u128)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?;
            }
            if bin_id >= active {
                total_weight_a = total_weight_a
                    .checked_add(bin_array.bins[idx].liquidity_a as u128)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?;
            }
        }

        if total_weight_a == 0 && total_weight_b == 0 {
            let bid_count = core::cmp::max((active - active_lower) as u64, 0) + 1;
            let ask_count = core::cmp::max((active_upper - active) as u64, 0) + 1;
            let per_bin_b = amount_b / bid_count;
            let per_bin_a = amount_a / ask_count;
            for bin_id in active_lower..=active_upper {
                let idx = (bin_id - lower) as usize;
                if bin_id < active {
                    bin_array.bins[idx].liquidity_b = bin_array.bins[idx].liquidity_b
                        .checked_add(per_bin_b).ok_or(ProgramError::from(DexError::MathOverflow))?;
                } else if bin_id > active {
                    bin_array.bins[idx].liquidity_a = bin_array.bins[idx].liquidity_a
                        .checked_add(per_bin_a).ok_or(ProgramError::from(DexError::MathOverflow))?;
                } else {
                    bin_array.bins[idx].liquidity_a = bin_array.bins[idx].liquidity_a
                        .checked_add(per_bin_a).ok_or(ProgramError::from(DexError::MathOverflow))?;
                    bin_array.bins[idx].liquidity_b = bin_array.bins[idx].liquidity_b
                        .checked_add(per_bin_b).ok_or(ProgramError::from(DexError::MathOverflow))?;
                }
            }
        } else {
            for bin_id in active_lower..=active_upper {
                let idx = (bin_id - lower) as usize;

                if bin_id < active {
                    if total_weight_b > 0 {
                        let w = bin_array.bins[idx].liquidity_b as u128;
                        let share_b = arlex_lang::math::checked_mul_div_u128(amount_b as u128, w, total_weight_b)
                            .ok_or(ProgramError::from(DexError::MathOverflow))?;
                        let share_b_u64 = u64::try_from(share_b).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
                        bin_array.bins[idx].liquidity_b = bin_array.bins[idx].liquidity_b
                            .checked_add(share_b_u64).ok_or(ProgramError::from(DexError::MathOverflow))?;
                    }
                } else if bin_id > active {
                    if total_weight_a > 0 {
                        let w = bin_array.bins[idx].liquidity_a as u128;
                        let share_a = arlex_lang::math::checked_mul_div_u128(amount_a as u128, w, total_weight_a)
                            .ok_or(ProgramError::from(DexError::MathOverflow))?;
                        let share_a_u64 = u64::try_from(share_a).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
                        bin_array.bins[idx].liquidity_a = bin_array.bins[idx].liquidity_a
                            .checked_add(share_a_u64).ok_or(ProgramError::from(DexError::MathOverflow))?;
                    }
                } else {
                    if total_weight_a > 0 {
                        let w = bin_array.bins[idx].liquidity_a as u128;
                        let share_a = arlex_lang::math::checked_mul_div_u128(amount_a as u128, w, total_weight_a)
                            .ok_or(ProgramError::from(DexError::MathOverflow))?;
                        let share_a_u64 = u64::try_from(share_a).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
                        bin_array.bins[idx].liquidity_a = bin_array.bins[idx].liquidity_a
                            .checked_add(share_a_u64).ok_or(ProgramError::from(DexError::MathOverflow))?;
                    }
                    if total_weight_b > 0 {
                        let w = bin_array.bins[idx].liquidity_b as u128;
                        let share_b = arlex_lang::math::checked_mul_div_u128(amount_b as u128, w, total_weight_b)
                            .ok_or(ProgramError::from(DexError::MathOverflow))?;
                        let share_b_u64 = u64::try_from(share_b).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
                        bin_array.bins[idx].liquidity_b = bin_array.bins[idx].liquidity_b
                            .checked_add(share_b_u64).ok_or(ProgramError::from(DexError::MathOverflow))?;
                    }
                }
            }
        }
    }

    // Assign remainder to active bin (conservation — catches integer division dust)
    let mut post_total_a: u64 = 0;
    let mut post_total_b: u64 = 0;
    for i in 0..MAX_BINS {
        post_total_a = post_total_a.checked_add(bin_array.bins[i].liquidity_a)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        post_total_b = post_total_b.checked_add(bin_array.bins[i].liquidity_b)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }
    let distributed_a = post_total_a.saturating_sub(pre_total_a);
    let distributed_b = post_total_b.saturating_sub(pre_total_b);
    let remainder_a = amount_a.saturating_sub(distributed_a);
    let remainder_b = amount_b.saturating_sub(distributed_b);

    let active_idx = (active - lower) as usize;
    if active_idx < MAX_BINS {
        bin_array.bins[active_idx].liquidity_a = bin_array.bins[active_idx].liquidity_a
            .checked_add(remainder_a).ok_or(ProgramError::from(DexError::MathOverflow))?;
        bin_array.bins[active_idx].liquidity_b = bin_array.bins[active_idx].liquidity_b
            .checked_add(remainder_b).ok_or(ProgramError::from(DexError::MathOverflow))?;
    }

    Ok(())
}

/// Redistribute liquidity using pyramid formula for shift_liquidity.
///
/// Asymmetric 2:1 pyramid: bid side gets 2/3 USDC, ask side gets 1/3 RWT.
/// nav_bin gets the remainder (peak, both tokens).
/// Conservation invariant: sum(bins) == original totals per token.
///
/// All bins are zeroed then refilled. This is intentional — ensures bins
/// outside the target range are properly cleared.
pub fn shift_pyramid(
    bin_array: &mut BinArray,
    nav_bin: i32,
    target_bin_count: u16,
) -> core::result::Result<(), ProgramError> {
    let half = target_bin_count as i32 / 2;
    let new_lower = nav_bin - half;
    let new_upper = nav_bin + (target_bin_count as i32 - 1 - half);

    // Validate range fits within BinArray
    if new_lower < bin_array.lower_bin_id || new_upper > bin_array.lower_bin_id + MAX_BINS as i32 - 1 {
        return Err(ProgramError::from(DexError::InvalidBinRange));
    }

    // Collect total liquidity across ALL bins
    let mut total_usdc: u128 = 0;
    let mut total_rwt: u128 = 0;
    for i in 0..MAX_BINS {
        total_usdc += bin_array.bins[i].liquidity_b as u128;
        total_rwt += bin_array.bins[i].liquidity_a as u128;
    }

    // Zero all bins (intentional: clear bins outside target range)
    for i in 0..MAX_BINS {
        bin_array.bins[i].liquidity_a = 0;
        bin_array.bins[i].liquidity_b = 0;
    }

    // Bid side targets (bins below nav_bin): USDC only, pyramid weight
    let bid_total_usdc = total_usdc * 2 / 3;
    let mut total_bid_weight: u128 = 0;
    for bin_id in new_lower..nav_bin {
        let weight = (bin_id - new_lower + 1) as u128;
        total_bid_weight += weight;
    }

    let mut sum_bid_usdc: u128 = 0;
    if total_bid_weight > 0 {
        for bin_id in new_lower..nav_bin {
            let idx = (bin_id - bin_array.lower_bin_id) as usize;
            let weight = (bin_id - new_lower + 1) as u128;
            let target = arlex_lang::math::checked_mul_div_u128(bid_total_usdc, weight, total_bid_weight)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            bin_array.bins[idx].liquidity_b = u64::try_from(target)
                .map_err(|_| ProgramError::from(DexError::MathOverflow))?;
            sum_bid_usdc += target;
        }
    }

    // Ask side targets (bins above nav_bin): RWT only, pyramid weight
    let ask_total_rwt = total_rwt / 3;
    let mut total_ask_weight: u128 = 0;
    for bin_id in (nav_bin + 1)..=new_upper {
        let weight = (new_upper - bin_id + 1) as u128;
        total_ask_weight += weight;
    }

    let mut sum_ask_rwt: u128 = 0;
    if total_ask_weight > 0 {
        for bin_id in (nav_bin + 1)..=new_upper {
            let idx = (bin_id - bin_array.lower_bin_id) as usize;
            let weight = (new_upper - bin_id + 1) as u128;
            let target = arlex_lang::math::checked_mul_div_u128(ask_total_rwt, weight, total_ask_weight)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            bin_array.bins[idx].liquidity_a = u64::try_from(target)
                .map_err(|_| ProgramError::from(DexError::MathOverflow))?;
            sum_ask_rwt += target;
        }
    }

    // nav_bin gets REMAINDER (peak of pyramid, both tokens)
    let nav_idx = (nav_bin - bin_array.lower_bin_id) as usize;
    let nav_usdc = total_usdc.checked_sub(sum_bid_usdc)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    let nav_rwt = total_rwt.checked_sub(sum_ask_rwt)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    bin_array.bins[nav_idx].liquidity_b = u64::try_from(nav_usdc)
        .map_err(|_| ProgramError::from(DexError::MathOverflow))?;
    bin_array.bins[nav_idx].liquidity_a = u64::try_from(nav_rwt)
        .map_err(|_| ProgramError::from(DexError::MathOverflow))?;

    // Defense-in-depth: verify conservation invariant
    let mut check_usdc: u128 = 0;
    let mut check_rwt: u128 = 0;
    for i in 0..MAX_BINS {
        check_usdc += bin_array.bins[i].liquidity_b as u128;
        check_rwt += bin_array.bins[i].liquidity_a as u128;
    }
    if check_usdc != total_usdc || check_rwt != total_rwt {
        return Err(ProgramError::from(DexError::ConservationViolation));
    }

    Ok(())
}
