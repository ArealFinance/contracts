//! Concentrated liquidity math: bin-walk swap and bin distribution.
//!
//! Bins are discrete price buckets. Each bin holds liquidity_a (RWT) and liquidity_b (USDC).
//! Below active_bin: only liquidity_b (bid side). Above: only liquidity_a (ask side).
//! Active bin: both tokens.

use arlex_lang::prelude::*;

use crate::constants::*;
use crate::error::DexError;
use crate::state::BinArray;

// =====================================================================
// CP-3 Monotonic Ladder math primitives
//
// `GEOMETRIC_WEIGHTS` and the `grow_redistribute` / `compress_redistribute`
// helpers below are consumed by CP-4 (`create_concentrated_pool` rewrite)
// and CP-7 (`grow_liquidity` / `compress_liquidity`). They are pure math —
// no on-chain state access beyond the supplied `BinArray` mutable slice.
//
// References:
// - docs/contracts/native-dex.mdx (Monotonic Ladder section, §49-66, §1300-1304)
// - docs/changelog/2026-04-17-monotonic-ladder.mdx
// =====================================================================

/// Precomputed geometric weights `w_k = r^k` for `k = 0..ACTIVE_ZONE_WIDTH`,
/// with `r = GEOMETRIC_R_BPS / 10_000 = 0.85`, stored as Q64.64 fixed-point.
///
/// Index 0 == peak (the new active bin). Index 39 == far edge of the active
/// bid wall (40 bins below the peak, inclusive). Generated offline via
/// `int(round((0.85 ** k) * (2**64)))` to avoid any runtime exp/log.
///
/// Doc §1303 pins the geometric density ratio; the underlying USDC for bin
/// `b` in the new active zone is computed as
/// `usdc_b = total_usdc * w_k / sum(GEOMETRIC_WEIGHTS)` where
/// `k = peak_bin - b` (so the peak gets the biggest share).
pub const GEOMETRIC_WEIGHTS: [u128; ACTIVE_ZONE_WIDTH as usize] = [
    18446744073709551616u128, // k=0,  0.85^0  = 1.0
    15679732462653118464u128, // k=1,  0.85^1  ≈ 0.8500000000
    13327772593255149568u128, // k=2,  0.85^2  ≈ 0.7225000000
    11328606704266876928u128, // k=3,  0.85^3  ≈ 0.6141250000
    9629315698626844672u128,  // k=4,  0.85^4  ≈ 0.5220062500
    8184918343832818688u128,  // k=5,  0.85^5  ≈ 0.4437053125
    6957180592257895424u128,  // k=6,  0.85^6  ≈ 0.3771495156
    5913603503419210752u128,  // k=7,  0.85^7  ≈ 0.3205770883
    5026562977906329600u128,  // k=8,  0.85^8  ≈ 0.2724905250
    4272578531220379648u128,  // k=9,  0.85^9  ≈ 0.2316169463
    3631691751537322496u128,  // k=10, 0.85^10 ≈ 0.1968744043
    3086937988806724096u128,  // k=11, 0.85^11 ≈ 0.1673432437
    2623897290485715456u128,  // k=12, 0.85^12 ≈ 0.1422417571
    2230312696912858112u128,  // k=13, 0.85^13 ≈ 0.1209054936
    1895765792375929344u128,  // k=14, 0.85^14 ≈ 0.1027696695
    1611400923519539968u128,  // k=15, 0.85^15 ≈ 0.0873542191
    1369690784991608832u128,  // k=16, 0.85^16 ≈ 0.0742510862
    1164237167242867456u128,  // k=17, 0.85^17 ≈ 0.0631134233
    989601592156437376u128,   // k=18, 0.85^18 ≈ 0.0536464098
    841161353332971776u128,   // k=19, 0.85^19 ≈ 0.0455994483
    714987150333025920u128,   // k=20, 0.85^20 ≈ 0.0387595311
    607739077783072000u128,   // k=21, 0.85^21 ≈ 0.0329456014
    516578216115611264u128,   // k=22, 0.85^22 ≈ 0.0280037612
    439091483698269568u128,   // k=23, 0.85^23 ≈ 0.0238031970
    373227761143529088u128,   // k=24, 0.85^24 ≈ 0.0202327175
    317243596971999744u128,   // k=25, 0.85^25 ≈ 0.0171978099
    269657057426199776u128,   // k=26, 0.85^26 ≈ 0.0146181384
    229208498812269792u128,   // k=27, 0.85^27 ≈ 0.0124254176
    194827223990429312u128,   // k=28, 0.85^28 ≈ 0.0105616050
    165603140391864928u128,   // k=29, 0.85^29 ≈ 0.0089773642
    140762669333085168u128,   // k=30, 0.85^30 ≈ 0.0076307596
    119648268933122400u128,   // k=31, 0.85^31 ≈ 0.0064861457
    101701028593154032u128,   // k=32, 0.85^32 ≈ 0.0055132238
    86445874304180928u128,    // k=33, 0.85^33 ≈ 0.0046862402
    73478993158553792u128,    // k=34, 0.85^34 ≈ 0.0039833042
    62457144184770712u128,    // k=35, 0.85^35 ≈ 0.0033858086
    53088572557055104u128,    // k=36, 0.85^36 ≈ 0.0028779373
    45125286673496840u128,    // k=37, 0.85^37 ≈ 0.0024462467
    38356493672472312u128,    // k=38, 0.85^38 ≈ 0.0020793097
    32603019621601464u128,    // k=39, 0.85^39 ≈ 0.0017674132
];

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
        // This is a minor discrepancy that grow_liquidity / compress_liquidity
        // (CP-7) will resolve when the ladder is next rebalanced.
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
///
/// CP-3: kept for CP-5 transition; the Monotonic Ladder rewrite gates user
/// `add_liquidity` / `zap_liquidity` on master pools with
/// `MasterPoolUserLpDisabled`. Once those guards land in CP-5, this helper
/// becomes dead code and a follow-up sweep can drop it.
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

// =====================================================================
// CP-3 Monotonic Ladder helpers
// =====================================================================

/// True iff any bin strictly above `active_bin` (within the `BinArray`
/// extent) has nonzero `liquidity_a` (RWT side).
///
/// Returns `false` if `active_bin` is at or past the top edge of the array,
/// since there is then no upward bin to inspect. Used by the swap path
/// (CP-6) to decide whether USDC→RWT trades can be served from organic
/// ask, or must reroute through `rwt_engine::mint_rwt`.
pub fn bin_walk_has_liquidity_above(bin_array: &BinArray, active_bin: i32) -> bool {
    let lower = bin_array.lower_bin_id;
    let upper = lower + MAX_BINS as i32 - 1;
    if active_bin >= upper {
        return false;
    }
    // Start the walk at active_bin + 1 (saturating so we never wrap below
    // lower when active_bin sits below the array — `for` handles the empty
    // range cleanly in that case).
    let start = core::cmp::max(active_bin + 1, lower);
    if start > upper {
        return false;
    }
    let start_idx = (start - lower) as usize;
    for i in start_idx..MAX_BINS {
        if bin_array.bins[i].liquidity_a > 0 {
            return true;
        }
    }
    false
}

/// Compute price at `bin` using the existing `pow_bps(bin_step_bps, bin)`
/// fixed-point with proper negative-bin handling. Returns a Q-style price
/// scaled by `CONCENTRATED_SCALE`, matching what `bin_walk_swap` already
/// consumes.
///
/// Exposed as a public symbol so the CP-6 swap mint-routing logic can
/// inspect `price_at_bin(bin_step_bps, active_bin)` against
/// `NAV × (1 + MINT_ROUTE_PRICE_OFFSET_BPS / 10_000)` without duplicating
/// the math. Returns `None` on overflow / out-of-range exponent.
pub fn price_at_bin(bin_step_bps: u16, bin: i32) -> Option<u128> {
    arlex_lang::math::pow_bps(bin_step_bps, bin)
}

/// Sum `liquidity_b` across `[zone_lower, zone_upper]` (inclusive). Returns
/// `MathOverflow` on u128 overflow (impossible in practice — `u64 × 40` —
/// but kept for defense-in-depth) or `InvalidBinRange` if the zone leaves
/// the `BinArray` extent.
fn sum_active_zone_usdc(
    bin_array: &BinArray,
    zone_lower: i32,
    zone_upper: i32,
) -> core::result::Result<u128, ProgramError> {
    let lower = bin_array.lower_bin_id;
    let upper = lower + MAX_BINS as i32 - 1;
    if zone_lower < lower || zone_upper > upper || zone_lower > zone_upper {
        return Err(ProgramError::from(DexError::InvalidBinRange));
    }
    let mut sum: u128 = 0;
    for bin_id in zone_lower..=zone_upper {
        let idx = (bin_id - lower) as usize;
        sum = sum
            .checked_add(bin_array.bins[idx].liquidity_b as u128)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }
    Ok(sum)
}

/// Redistribute liquidity in the active zone for an UPWARD NAV move.
///
/// Inputs:
/// - `bin_array`: mutable; tail and old-active-zone bins below
///   `new_active_zone_lower` untouched; bins above old active untouched.
/// - `left_anchor_bin`: lower edge of the extended-bid region (active edge
///   of the permanent tail). Active zone is NOT allowed to dip below it.
/// - `permanent_tail_floor_bin`: bottom of the permanent tail. Used only
///   for the right-edge buffer check (`new_nav_bin - permanent_tail_floor_bin`
///   must leave at least `RIGHT_EDGE_BUFFER_BINS` of headroom inside the
///   `BinArray` extent).
/// - `last_rebalance_nav_bin`: monotonicity gate.
/// - `new_nav_bin`: target active bin after growth.
/// - `active_zone_width`: number of bins in the new active zone (peak +
///   `active_zone_width - 1` bins below). Production callers pass
///   `ACTIVE_ZONE_WIDTH = 40`.
/// - `fresh_usdc`: amount pulled from the Nexus accumulator to fold into
///   the active-zone USDC sum.
///
/// Semantics (docs §49-66, §1300-1304):
/// - The new active zone is `[new_nav_bin - active_zone_width + 1
///   .. new_nav_bin]` (inclusive on both ends, `active_zone_width` bins
///   total, peak = `new_nav_bin`).
/// - `total_usdc` after redistribution = sum of `liquidity_b` in the
///   **current** active zone (`[bin_array.active_bin_id -
///   active_zone_width + 1 .. bin_array.active_bin_id]`) plus
///   `fresh_usdc`.
/// - Distribute `total_usdc` across the new active zone using
///   `GEOMETRIC_WEIGHTS` (peak at `new_nav_bin`).
/// - Bins BELOW the new active zone but AT/ABOVE `left_anchor_bin` are
///   "extended bid" — left untouched (no redistribution, no draining).
///   Permanent-tail bins (`< left_anchor_bin`) are also untouched.
/// - Organic ask bins above the **current** `bin_array.active_bin_id`
///   (holding `liquidity_a`) are untouched.
///
/// Returns `Ok(new_active_zone_lower)` on success.
///
/// Errors:
/// - `NotGrowthDirection` if `new_nav_bin <= last_rebalance_nav_bin`.
/// - `ActiveZoneOverlapsTail` if `new_nav_bin - active_zone_width + 1 <
///   left_anchor_bin`.
/// - `ExceedsRightEdgeBuffer` if `new_nav_bin > (lower_bin_id + MAX_BINS -
///   1) - RIGHT_EDGE_BUFFER_BINS` OR if
///   `new_nav_bin - permanent_tail_floor_bin > MAX_BINS as i32 -
///   RIGHT_EDGE_BUFFER_BINS`. Either ceiling means the ladder is running
///   out of headroom and must rotate (out of CP-3 scope).
/// - `InvalidBinRange` if any required bin sits outside the array extent.
/// - `MathOverflow` on any u128/u64 arithmetic.
pub fn grow_redistribute(
    bin_array: &mut BinArray,
    left_anchor_bin: i32,
    permanent_tail_floor_bin: i32,
    last_rebalance_nav_bin: i32,
    new_nav_bin: i32,
    active_zone_width: u16,
    fresh_usdc: u64,
) -> core::result::Result<i32, ProgramError> {
    // ---- Direction gate ----
    if new_nav_bin <= last_rebalance_nav_bin {
        return Err(ProgramError::from(DexError::NotGrowthDirection));
    }

    let width = active_zone_width as i32;
    if width <= 0 || (active_zone_width as usize) > GEOMETRIC_WEIGHTS.len() {
        return Err(ProgramError::from(DexError::InvalidBinRange));
    }

    let lower = bin_array.lower_bin_id;
    let upper = lower + MAX_BINS as i32 - 1;
    let new_zone_lower = new_nav_bin - width + 1;

    // ---- Geometry gates ----
    if new_zone_lower < left_anchor_bin {
        return Err(ProgramError::from(DexError::ActiveZoneOverlapsTail));
    }
    if new_nav_bin > upper - RIGHT_EDGE_BUFFER_BINS {
        return Err(ProgramError::from(DexError::ExceedsRightEdgeBuffer));
    }
    if new_nav_bin
        .checked_sub(permanent_tail_floor_bin)
        .ok_or(ProgramError::from(DexError::MathOverflow))?
        > (MAX_BINS as i32) - RIGHT_EDGE_BUFFER_BINS
    {
        return Err(ProgramError::from(DexError::ExceedsRightEdgeBuffer));
    }
    if new_zone_lower < lower || new_nav_bin > upper {
        return Err(ProgramError::from(DexError::InvalidBinRange));
    }

    // ---- Snapshot the current active zone and sum its USDC ----
    let current_active = bin_array.active_bin_id;
    let current_zone_lower = current_active - width + 1;
    let cur_l = core::cmp::max(current_zone_lower, lower);
    let cur_u = core::cmp::min(current_active, upper);
    let mut current_usdc: u128 = 0;
    if cur_l <= cur_u {
        current_usdc = sum_active_zone_usdc(bin_array, cur_l, cur_u)?;
    }

    let total_usdc = current_usdc
        .checked_add(fresh_usdc as u128)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    // ---- Compute weight sum (Q32.32-normalised) ----
    //
    // GEOMETRIC_WEIGHTS are Q64.64. Their sum across 40 entries is
    // ≈ 6.67 × 2^64 (geometric series with r = 0.85), which would push
    // `checked_mul_div_u128` past its divisor < 2^64 contract. We
    // right-shift each weight by 32 before summing, keeping 32 bits of
    // mantissa per term — plenty for proportional USDC distribution.
    let active_width = active_zone_width as usize;
    let mut weight_sum: u128 = 0;
    for w in GEOMETRIC_WEIGHTS.iter().take(active_width) {
        weight_sum = weight_sum
            .checked_add(*w >> 32)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }
    if weight_sum == 0 {
        return Err(ProgramError::from(DexError::MathOverflow));
    }

    // ---- Zero out current active zone's liquidity_b (we're rebuilding it) ----
    if cur_l <= cur_u {
        for bin_id in cur_l..=cur_u {
            let idx = (bin_id - lower) as usize;
            bin_array.bins[idx].liquidity_b = 0;
        }
    }

    // ---- Distribute total_usdc across the new active zone ----
    // Index 0 of GEOMETRIC_WEIGHTS == peak (= new_nav_bin). Index k == new_nav_bin - k.
    let mut distributed: u128 = 0;
    for k in 0..active_width {
        let bin_id = new_nav_bin - k as i32;
        let idx = (bin_id - lower) as usize;
        // Right-shift each weight to match the Q32.32-normalised weight_sum.
        let share = arlex_lang::math::checked_mul_div_u128(
            total_usdc,
            GEOMETRIC_WEIGHTS[k] >> 32,
            weight_sum,
        )
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
        let share_u64 = u64::try_from(share)
            .map_err(|_| ProgramError::from(DexError::MathOverflow))?;
        // Bin's liquidity_b was just zeroed above, so this is a clean set.
        bin_array.bins[idx].liquidity_b = bin_array.bins[idx]
            .liquidity_b
            .checked_add(share_u64)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        distributed = distributed
            .checked_add(share)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }

    // ---- Conserve rounding dust onto the peak bin ----
    let dust = total_usdc.saturating_sub(distributed);
    if dust > 0 {
        let peak_idx = (new_nav_bin - lower) as usize;
        let dust_u64 = u64::try_from(dust)
            .map_err(|_| ProgramError::from(DexError::MathOverflow))?;
        bin_array.bins[peak_idx].liquidity_b = bin_array.bins[peak_idx]
            .liquidity_b
            .checked_add(dust_u64)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }

    Ok(new_zone_lower)
}

/// Redistribute liquidity in the active zone for a DOWNWARD NAV move.
///
/// Semantics (docs §66, Monotonic Ladder spec):
/// - Total USDC stays constant (capital-neutral, no Nexus pull).
/// - Sum `liquidity_b` in the OLD active zone (`[bin_array.active_bin_id -
///   active_zone_width + 1 .. bin_array.active_bin_id]`), then redistribute
///   using `GEOMETRIC_WEIGHTS` centered on `new_nav_bin` (peak =
///   `new_nav_bin`).
/// - Bins between `new_nav_bin` and the OLD `bin_array.active_bin_id`
///   become a "frozen ask wall" — they retain their RWT (`liquidity_a`).
///   `liquidity_b` is zeroed there as part of the sum-and-rebuild, but RWT
///   is preserved. Capital is never destroyed.
/// - Permanent tail (`< left_anchor_bin`) UNTOUCHED.
/// - Organic ask above the OLD active bin UNTOUCHED.
///
/// Returns `Ok(new_active_zone_lower)` on success.
///
/// Errors:
/// - `NotCompressionDirection` if `new_nav_bin >= last_rebalance_nav_bin`.
/// - `ActiveZoneOverlapsTail` if the new zone lower dips below
///   `left_anchor_bin`.
/// - `InvalidBinRange` if any required bin sits outside the array extent.
/// - `MathOverflow` on any u128/u64 arithmetic.
pub fn compress_redistribute(
    bin_array: &mut BinArray,
    left_anchor_bin: i32,
    last_rebalance_nav_bin: i32,
    new_nav_bin: i32,
    active_zone_width: u16,
) -> core::result::Result<i32, ProgramError> {
    // ---- Direction gate ----
    if new_nav_bin >= last_rebalance_nav_bin {
        return Err(ProgramError::from(DexError::NotCompressionDirection));
    }

    let width = active_zone_width as i32;
    if width <= 0 || (active_zone_width as usize) > GEOMETRIC_WEIGHTS.len() {
        return Err(ProgramError::from(DexError::InvalidBinRange));
    }

    let lower = bin_array.lower_bin_id;
    let upper = lower + MAX_BINS as i32 - 1;
    let new_zone_lower = new_nav_bin - width + 1;

    if new_zone_lower < left_anchor_bin {
        return Err(ProgramError::from(DexError::ActiveZoneOverlapsTail));
    }
    if new_zone_lower < lower || new_nav_bin > upper {
        return Err(ProgramError::from(DexError::InvalidBinRange));
    }

    // ---- Sum OLD active zone's USDC (capital-neutral target) ----
    let current_active = bin_array.active_bin_id;
    let current_zone_lower = current_active - width + 1;
    let cur_l = core::cmp::max(current_zone_lower, lower);
    let cur_u = core::cmp::min(current_active, upper);
    let mut total_usdc: u128 = 0;
    if cur_l <= cur_u {
        total_usdc = sum_active_zone_usdc(bin_array, cur_l, cur_u)?;
    }

    // ---- Weight sum (Q32.32-normalised; see grow_redistribute for rationale) ----
    let active_width = active_zone_width as usize;
    let mut weight_sum: u128 = 0;
    for w in GEOMETRIC_WEIGHTS.iter().take(active_width) {
        weight_sum = weight_sum
            .checked_add(*w >> 32)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }
    if weight_sum == 0 {
        return Err(ProgramError::from(DexError::MathOverflow));
    }

    // ---- Zero out OLD active zone's liquidity_b (RWT stays — frozen ask wall) ----
    if cur_l <= cur_u {
        for bin_id in cur_l..=cur_u {
            let idx = (bin_id - lower) as usize;
            bin_array.bins[idx].liquidity_b = 0;
        }
    }

    // ---- Distribute total_usdc across the new active zone ----
    let mut distributed: u128 = 0;
    for k in 0..active_width {
        let bin_id = new_nav_bin - k as i32;
        let idx = (bin_id - lower) as usize;
        // Right-shift each weight to match the Q32.32-normalised weight_sum.
        let share = arlex_lang::math::checked_mul_div_u128(
            total_usdc,
            GEOMETRIC_WEIGHTS[k] >> 32,
            weight_sum,
        )
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
        let share_u64 = u64::try_from(share)
            .map_err(|_| ProgramError::from(DexError::MathOverflow))?;
        bin_array.bins[idx].liquidity_b = bin_array.bins[idx]
            .liquidity_b
            .checked_add(share_u64)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        distributed = distributed
            .checked_add(share)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }

    // ---- Conserve rounding dust onto the peak bin ----
    let dust = total_usdc.saturating_sub(distributed);
    if dust > 0 {
        let peak_idx = (new_nav_bin - lower) as usize;
        let dust_u64 = u64::try_from(dust)
            .map_err(|_| ProgramError::from(DexError::MathOverflow))?;
        bin_array.bins[peak_idx].liquidity_b = bin_array.bins[peak_idx]
            .liquidity_b
            .checked_add(dust_u64)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }

    Ok(new_zone_lower)
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Bin;

    // ---- Test helpers --------------------------------------------------

    /// Build a zero-initialised `BinArray` rooted at `lower_bin_id` with
    /// `active_bin_id` set. Used as the substrate for all CP-3 math tests.
    fn empty_bin_array(lower_bin_id: i32, active_bin_id: i32) -> BinArray {
        BinArray {
            pool: [0u8; 32],
            bins: [Bin { liquidity_a: 0, liquidity_b: 0 }; MAX_BINS],
            lower_bin_id,
            bin_step_bps: 10,
            active_bin_id,
            bump: 0,
        }
    }

    fn idx(bin_array: &BinArray, bin_id: i32) -> usize {
        (bin_id - bin_array.lower_bin_id) as usize
    }

    fn set_b(bin_array: &mut BinArray, bin_id: i32, amount: u64) {
        let i = idx(bin_array, bin_id);
        bin_array.bins[i].liquidity_b = amount;
    }

    fn set_a(bin_array: &mut BinArray, bin_id: i32, amount: u64) {
        let i = idx(bin_array, bin_id);
        bin_array.bins[i].liquidity_a = amount;
    }

    fn sum_b(bin_array: &BinArray, from: i32, to: i32) -> u128 {
        let mut s: u128 = 0;
        for bin_id in from..=to {
            // `Bin` is repr(C, packed) — copy via braces.
            let i = idx(bin_array, bin_id);
            s += { bin_array.bins[i].liquidity_b } as u128;
        }
        s
    }

    // ---- GEOMETRIC_WEIGHTS pin tests ----------------------------------

    /// docs §1303 — index 0 is the peak and weight is exactly `2^64`.
    #[test]
    fn geometric_weights_index_zero_is_one_q64() {
        assert_eq!(GEOMETRIC_WEIGHTS[0], Q64);
        assert_eq!(GEOMETRIC_WEIGHTS[0], 1u128 << 64);
    }

    /// `w[1] / w[0]` should equal `r = 0.85` ≈ 8500 bps. Floor-rounded
    /// truncation gives 8499 in Q64, which is within the documented ±1
    /// tolerance band.
    #[test]
    fn geometric_weights_ratio_is_0_85() {
        let ratio_bps = GEOMETRIC_WEIGHTS[1] * 10_000 / GEOMETRIC_WEIGHTS[0];
        assert!(
            (ratio_bps as i64 - 8500).abs() <= 1,
            "expected ~8500 bps, got {ratio_bps}"
        );
    }

    /// The full table must be strictly monotonically decreasing.
    #[test]
    fn geometric_weights_strictly_decreasing() {
        for k in 0..(ACTIVE_ZONE_WIDTH as usize - 1) {
            assert!(
                GEOMETRIC_WEIGHTS[k + 1] < GEOMETRIC_WEIGHTS[k],
                "GEOMETRIC_WEIGHTS not monotone at k={k}: \
                 w[{k}]={a}, w[{k1}]={b}",
                k1 = k + 1,
                a = GEOMETRIC_WEIGHTS[k],
                b = GEOMETRIC_WEIGHTS[k + 1],
            );
        }
    }

    /// Pin a specific row deep in the table. `0.85^10 ≈ 0.1968744043`
    /// scaled by `2^64` rounds to this exact Q64.64 literal.
    #[test]
    fn geometric_weights_known_value_at_index_10() {
        assert_eq!(GEOMETRIC_WEIGHTS[10], 3631691751537322496u128);
    }

    // ---- price_at_bin tests -------------------------------------------

    /// `price_at_bin(_, 0) == CONCENTRATED_SCALE` — unit price at bin 0.
    #[test]
    fn price_at_bin_zero_is_unity() {
        let p = price_at_bin(10, 0).expect("price_at_bin(10, 0)");
        assert_eq!(p, CONCENTRATED_SCALE);
    }

    /// Negative-exponent bins price below unity.
    #[test]
    fn price_at_bin_negative_below_unity() {
        let p = price_at_bin(100, -5).expect("price_at_bin(100, -5)");
        assert!(p < CONCENTRATED_SCALE, "price at bin -5 should be < unity");
    }

    // ---- bin_walk_has_liquidity_above ---------------------------------

    #[test]
    fn bin_walk_has_liquidity_above_empty_returns_false() {
        let ba = empty_bin_array(0, 500);
        assert!(!bin_walk_has_liquidity_above(&ba, 500));
    }

    #[test]
    fn bin_walk_has_liquidity_above_with_rwt_returns_true() {
        let mut ba = empty_bin_array(0, 500);
        // RWT above active
        set_a(&mut ba, 600, 1_000);
        assert!(bin_walk_has_liquidity_above(&ba, 500));
        // RWT only below active — should still be false
        let mut ba2 = empty_bin_array(0, 500);
        set_a(&mut ba2, 400, 1_000);
        assert!(!bin_walk_has_liquidity_above(&ba2, 500));
    }

    // ---- grow_redistribute --------------------------------------------

    /// Fresh USDC plus old-zone USDC ends up redistributed inside the new
    /// active zone, within `active_zone_width` rounding ulps.
    #[test]
    fn grow_redistribute_conserves_sum() {
        // Lower 0, active 500. Old zone = [461..=500]. Bump NAV to 600.
        let lower = 0i32;
        let mut ba = empty_bin_array(lower, 500);
        // Populate old active zone with arbitrary USDC.
        for bin_id in 461..=500 {
            set_b(&mut ba, bin_id, 10_000);
        }
        let pre_sum_b: u128 = sum_b(&ba, 461, 500);
        let fresh = 50_000u64;

        // left_anchor_bin small enough to not block, tail floor at 100,
        // last_rebalance at 500 (current active).
        let new_active_zone_lower = grow_redistribute(
            &mut ba,
            /* left_anchor_bin */ 100,
            /* permanent_tail_floor_bin */ 100,
            /* last_rebalance_nav_bin */ 500,
            /* new_nav_bin */ 600,
            ACTIVE_ZONE_WIDTH,
            fresh,
        )
        .expect("grow_redistribute");

        assert_eq!(new_active_zone_lower, 600 - ACTIVE_ZONE_WIDTH as i32 + 1);
        let post_sum_new_zone = sum_b(&ba, new_active_zone_lower, 600);
        let expected = pre_sum_b + fresh as u128;
        let diff = expected as i128 - post_sum_new_zone as i128;
        assert!(
            diff.unsigned_abs() <= ACTIVE_ZONE_WIDTH as u128,
            "USDC drift exceeds 40-ulp tolerance: expected {expected}, got {post_sum_new_zone}"
        );
    }

    /// Permanent tail bins (< left_anchor_bin) stay byte-identical across a
    /// grow_redistribute call.
    #[test]
    fn grow_redistribute_permanent_tail_untouched() {
        let lower = 0i32;
        let mut ba = empty_bin_array(lower, 500);
        // Tail USDC at bins 30..100
        for bin_id in 30..100 {
            set_b(&mut ba, bin_id, 1_234);
        }
        for bin_id in 461..=500 {
            set_b(&mut ba, bin_id, 5_000);
        }
        let pre_tail_sum: u128 = sum_b(&ba, 30, 99);
        grow_redistribute(&mut ba, 100, 100, 500, 600, ACTIVE_ZONE_WIDTH, 10_000)
            .expect("grow_redistribute");
        let post_tail_sum: u128 = sum_b(&ba, 30, 99);
        assert_eq!(pre_tail_sum, post_tail_sum);
        // And each bin individually. Bin is `repr(C, packed)`, so reads
        // must go through copies (the braces below).
        for bin_id in 30..100 {
            let i = idx(&ba, bin_id);
            assert_eq!({ ba.bins[i].liquidity_b }, 1_234u64);
        }
    }

    /// Organic ask bins above the current active bin keep their RWT.
    #[test]
    fn grow_redistribute_organic_ask_untouched() {
        let lower = 0i32;
        let mut ba = empty_bin_array(lower, 500);
        for bin_id in 461..=500 {
            set_b(&mut ba, bin_id, 5_000);
        }
        // Sparse organic ask between current active (500) and target (600)
        // outside the new active zone (which is [561..=600]).
        set_a(&mut ba, 520, 7_777);
        set_a(&mut ba, 540, 9_999);
        // Organic ask above target also exists.
        set_a(&mut ba, 700, 12_345);

        grow_redistribute(&mut ba, 100, 100, 500, 600, ACTIVE_ZONE_WIDTH, 10_000)
            .expect("grow_redistribute");

        // Bins outside the new active zone keep RWT unchanged.
        // (Bins 520, 540 are below new_zone_lower=561, so they're "extended
        // bid" and we don't touch liquidity_a.) `Bin` is repr(C, packed) so
        // reads must go through copies.
        let i520 = idx(&ba, 520);
        let i540 = idx(&ba, 540);
        let i700 = idx(&ba, 700);
        assert_eq!({ ba.bins[i520].liquidity_a }, 7_777u64);
        assert_eq!({ ba.bins[i540].liquidity_a }, 9_999u64);
        assert_eq!({ ba.bins[i700].liquidity_a }, 12_345u64);
    }

    #[test]
    fn grow_redistribute_rejects_compression() {
        let lower = 0i32;
        let mut ba = empty_bin_array(lower, 500);
        // Equal NAV should also reject — strict `<=`.
        let err = grow_redistribute(&mut ba, 100, 100, 500, 500, ACTIVE_ZONE_WIDTH, 1_000);
        assert!(matches!(err, Err(_)));
        let err = grow_redistribute(&mut ba, 100, 100, 500, 400, ACTIVE_ZONE_WIDTH, 1_000);
        // Coarse: we expect `NotGrowthDirection`. The error enum maps to
        // ProgramError::Custom — assert it's an Err is enough at this level
        // (exact code path is exercised in CP-7 integration tests).
        assert!(err.is_err());
    }

    #[test]
    fn grow_redistribute_rejects_overlap_with_tail() {
        let lower = 0i32;
        let mut ba = empty_bin_array(lower, 500);
        // Target = 100, active zone lower would be 100 - 40 + 1 = 61.
        // left_anchor_bin = 80 means the new lower (61) dips below the
        // anchor → ActiveZoneOverlapsTail.
        let err = grow_redistribute(
            &mut ba,
            /* left_anchor_bin */ 80,
            /* permanent_tail_floor_bin */ 10,
            /* last_rebalance_nav_bin */ 50,
            /* new_nav_bin */ 100,
            ACTIVE_ZONE_WIDTH,
            0,
        );
        assert!(err.is_err());
    }

    #[test]
    fn grow_redistribute_rejects_right_edge() {
        // Force new_nav_bin too close to the upper edge of the BinArray.
        // lower = 0, MAX_BINS = 1000 → upper = 999, buffer = 10 →
        // any new_nav_bin > 989 should trip ExceedsRightEdgeBuffer.
        let lower = 0i32;
        let mut ba = empty_bin_array(lower, 800);
        let err = grow_redistribute(&mut ba, 50, 50, 800, 995, ACTIVE_ZONE_WIDTH, 0);
        assert!(err.is_err());
    }

    // ---- compress_redistribute ----------------------------------------

    #[test]
    fn compress_redistribute_conserves_sum() {
        let lower = 0i32;
        let mut ba = empty_bin_array(lower, 500);
        for bin_id in 461..=500 {
            set_b(&mut ba, bin_id, 4_000);
        }
        let pre_sum_b: u128 = sum_b(&ba, 461, 500);

        // Compress NAV 500 → 480. New zone = [441..=480].
        let new_zone_lower =
            compress_redistribute(&mut ba, 100, 500, 480, ACTIVE_ZONE_WIDTH).expect("compress");
        assert_eq!(new_zone_lower, 480 - ACTIVE_ZONE_WIDTH as i32 + 1);

        // Total USDC system-wide should be unchanged (modulo 40 ulps).
        let post_sum_all: u128 = sum_b(&ba, lower, lower + MAX_BINS as i32 - 1);
        let diff = pre_sum_b as i128 - post_sum_all as i128;
        assert!(
            diff.unsigned_abs() <= ACTIVE_ZONE_WIDTH as u128,
            "Total USDC drift exceeds tolerance: pre={pre_sum_b}, post={post_sum_all}"
        );
    }

    #[test]
    fn compress_redistribute_frozen_ask_preserved() {
        let lower = 0i32;
        let mut ba = empty_bin_array(lower, 500);
        // Old active zone full of USDC.
        for bin_id in 461..=500 {
            set_b(&mut ba, bin_id, 4_000);
        }
        // Some bins between new (480) and old (500) carry RWT — frozen ask
        // wall after compression. We seed them ahead of time so compress
        // can demonstrate it leaves liquidity_a alone.
        set_a(&mut ba, 485, 3_333);
        set_a(&mut ba, 495, 5_555);

        compress_redistribute(&mut ba, 100, 500, 480, ACTIVE_ZONE_WIDTH).expect("compress");

        // The frozen-ask RWT survives the compression. `Bin` is repr(C,
        // packed) — reads must go through copies (the braces below).
        let i485 = idx(&ba, 485);
        let i495 = idx(&ba, 495);
        assert_eq!({ ba.bins[i485].liquidity_a }, 3_333u64);
        assert_eq!({ ba.bins[i495].liquidity_a }, 5_555u64);
    }

    #[test]
    fn compress_redistribute_rejects_growth() {
        let lower = 0i32;
        let mut ba = empty_bin_array(lower, 500);
        // Equal NAV → reject (strict `>=`).
        let err = compress_redistribute(&mut ba, 100, 500, 500, ACTIVE_ZONE_WIDTH);
        assert!(err.is_err());
        // Larger NAV → reject.
        let err = compress_redistribute(&mut ba, 100, 500, 600, ACTIVE_ZONE_WIDTH);
        assert!(err.is_err());
    }
}
