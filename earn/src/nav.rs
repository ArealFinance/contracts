use crate::constants::{INITIAL_NAV, NAV_SCALE};
use crate::error::EarnError;
use arlex_lang::prelude::*;

/// Calculate Book NAV Price per RWT token.
///
/// Formula: NAV = total_invested_capital × NAV_SCALE / total_rwt_supply
///
/// Returns value in USDC lamports per RWT (6 decimals).
/// Guards:
/// - supply == 0 → INITIAL_NAV ($1.00)
/// - NAV clamped to minimum 1 when supply > 0 (prevents NAV=0 from integer
///   truncation at extreme capital/supply ratios → would give free RWT).
pub fn calculate_nav(capital: u128, supply: u64) -> core::result::Result<u64, ProgramError> {
    if supply == 0 {
        return Ok(INITIAL_NAV);
    }
    let nav = arlex_lang::math::mul_div_u128_u64(capital, NAV_SCALE, supply)
        .ok_or_else(|| ProgramError::from(EarnError::MathOverflow))?;
    let nav_u64 = u64::try_from(nav).map_err(|_| ProgramError::from(EarnError::MathOverflow))?;
    Ok(core::cmp::max(nav_u64, 1))
}

#[cfg(test)]
mod tests {
    //! NAV characterization tests (mirror rwt-engine/src/nav.rs).
    use super::*;

    /// supply == 0 short-circuits to INITIAL_NAV regardless of capital.
    /// Protects the bootstrap (first mint sees $1.00).
    #[test]
    fn nav_zero_supply_returns_initial_nav() {
        assert_eq!(calculate_nav(0, 0).unwrap(), INITIAL_NAV);
        assert_eq!(calculate_nav(123_456_789, 0).unwrap(), INITIAL_NAV);
        assert_eq!(calculate_nav(u128::MAX, 0).unwrap(), INITIAL_NAV);
    }

    /// Identity: capital == NAV_SCALE && supply == NAV_SCALE → NAV == NAV_SCALE.
    #[test]
    fn nav_unit_supply_returns_capital() {
        let nav = calculate_nav(NAV_SCALE as u128, NAV_SCALE).unwrap();
        assert_eq!(nav, NAV_SCALE);
        assert_eq!(nav, INITIAL_NAV);
    }

    /// add_to_basket flow: doubling capital with constant supply doubles NAV.
    #[test]
    fn nav_doubling_capital_doubles_nav() {
        let supply = NAV_SCALE;
        let nav_1x = calculate_nav(NAV_SCALE as u128, supply).unwrap();
        let nav_2x = calculate_nav(2 * NAV_SCALE as u128, supply).unwrap();
        assert_eq!(nav_1x, NAV_SCALE);
        assert_eq!(nav_2x, 2 * NAV_SCALE);
    }

    /// SECURITY: clamp NAV to 1 when integer truncation would yield 0.
    #[test]
    fn nav_clamp_at_minimum_when_truncation_yields_zero() {
        let nav = calculate_nav(1u128, NAV_SCALE + 1).unwrap();
        assert_eq!(nav, 1, "NAV must clamp to 1, not 0, on truncation");
    }

    /// Overflow rejection: a NAV that exceeds u64::MAX is refused.
    #[test]
    fn nav_overflow_rejected_with_math_overflow() {
        let result = calculate_nav(u128::MAX, 1);
        assert!(result.is_err(), "expected MathOverflow, got {:?}", result);
    }
}
