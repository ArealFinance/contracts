use arlex_lang::prelude::*;

use crate::constants::*;
use crate::error::DexError;

/// Fee breakdown for a swap.
pub struct FeeBreakdown {
    pub fee_total: u64,
    pub fee_lp: u64,
    pub fee_protocol: u64,
    pub ot_treasury_fee: u64,
}

/// Constant product swap: amount_out = reserve_out * net_input / (reserve_in + net_input)
/// Rounds DOWN (user gets slightly less — protocol favored).
pub fn constant_product_output(reserve_in: u64, reserve_out: u64, net_input: u64) -> core::result::Result<u64, ProgramError> {
    if reserve_in == 0 || reserve_out == 0 {
        return Err(ProgramError::from(DexError::EmptyReserves));
    }
    let numerator = (reserve_out as u128).checked_mul(net_input as u128)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    let denominator = (reserve_in as u128).checked_add(net_input as u128)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    if denominator == 0 {
        return Err(ProgramError::from(DexError::MathOverflow));
    }
    let result = numerator / denominator;
    u64::try_from(result).map_err(|_| ProgramError::from(DexError::MathOverflow))
}

/// Calculate fee split. Protocol fee = fee_total - fee_lp (remainder pattern, dust to protocol).
pub fn calculate_fees(
    amount: u64,
    fee_bps: u16,
    lp_fee_share_bps: u16,
    has_ot_treasury: bool,
) -> core::result::Result<FeeBreakdown, ProgramError> {
    let fee_total = arlex_lang::math::mul_div_u64(amount, fee_bps as u64, BPS_DENOMINATOR)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    let fee_lp = arlex_lang::math::mul_div_u64(fee_total, lp_fee_share_bps as u64, BPS_DENOMINATOR)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    // Remainder pattern: protocol gets dust
    let fee_protocol = fee_total.checked_sub(fee_lp)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    let ot_treasury_fee = if has_ot_treasury {
        arlex_lang::math::mul_div_u64(amount, OT_TREASURY_FEE_BPS as u64, BPS_DENOMINATOR)
            .ok_or(ProgramError::from(DexError::MathOverflow))?
    } else {
        0
    };

    Ok(FeeBreakdown { fee_total, fee_lp, fee_protocol, ot_treasury_fee })
}

/// First add: shares = sqrt(amount_a * amount_b), burn MIN_LIQUIDITY.
/// Subsequent: shares = min(amount_a * total / reserve_a, amount_b * total / reserve_b).
pub fn calculate_lp_shares(
    amount_a: u64,
    amount_b: u64,
    reserve_a: u64,
    reserve_b: u64,
    total_lp_shares: u128,
    is_first: bool,
) -> core::result::Result<u128, ProgramError> {
    if is_first {
        let product = (amount_a as u128).checked_mul(amount_b as u128)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        let shares = arlex_lang::math::isqrt(product);
        if shares < MIN_LIQUIDITY as u128 {
            return Err(ProgramError::from(DexError::InitialLiquidityTooSmall));
        }
        // SECURITY: Burn MIN_LIQUIDITY to prevent first-depositor attack
        Ok(shares - MIN_LIQUIDITY as u128)
    } else {
        let shares_a = arlex_lang::math::checked_mul_div_u128(
            total_lp_shares, amount_a as u128, reserve_a as u128,
        ).ok_or(ProgramError::from(DexError::MathOverflow))?;
        let shares_b = arlex_lang::math::checked_mul_div_u128(
            total_lp_shares, amount_b as u128, reserve_b as u128,
        ).ok_or(ProgramError::from(DexError::MathOverflow))?;
        Ok(core::cmp::min(shares_a, shares_b))
    }
}

/// Remove: amount = shares_to_burn * reserve / total_lp_shares (rounds DOWN).
pub fn calculate_remove_amounts(
    shares: u128,
    reserve_a: u64,
    reserve_b: u64,
    total: u128,
) -> core::result::Result<(u64, u64), ProgramError> {
    if total == 0 {
        return Err(ProgramError::from(DexError::InsufficientLiquidity));
    }
    let a = arlex_lang::math::checked_mul_div_u128(shares, reserve_a as u128, total)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    let b = arlex_lang::math::checked_mul_div_u128(shares, reserve_b as u128, total)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    let a_u64 = u64::try_from(a).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
    let b_u64 = u64::try_from(b).map_err(|_| ProgramError::from(DexError::MathOverflow))?;
    Ok((a_u64, b_u64))
}
