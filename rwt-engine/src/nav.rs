use arlex_lang::prelude::*;
use crate::constants::{INITIAL_NAV, NAV_SCALE};
use crate::error::RwtError;

/// Calculate NAV (Net Asset Value) per RWT token.
///
/// Formula: NAV = total_invested_capital * NAV_SCALE / total_rwt_supply
///
/// Returns value in USDC lamports per RWT (6 decimals).
/// Guards:
/// - supply == 0 → INITIAL_NAV ($1.00)
/// - NAV clamped to minimum 1 when supply > 0 (prevents NAV=0 from integer truncation)
pub fn calculate_nav(capital: u128, supply: u64) -> core::result::Result<u64, ProgramError> {
    if supply == 0 {
        return Ok(INITIAL_NAV);
    }
    let nav = arlex_lang::math::mul_div_u128_u64(capital, NAV_SCALE, supply)
        .ok_or(ProgramError::from(RwtError::MathOverflow))?;
    let nav_u64 = u64::try_from(nav).map_err(|_| ProgramError::from(RwtError::MathOverflow))?;
    // SECURITY: clamp NAV to minimum 1 when supply > 0
    // Prevents NAV=0 from integer truncation at extreme capital/supply ratios
    // (e.g., capital=1, supply=1_000_001 → raw NAV=0)
    Ok(core::cmp::max(nav_u64, 1))
}
