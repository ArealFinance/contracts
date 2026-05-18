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
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    let nav_u64 = u64::try_from(nav).map_err(|_| ProgramError::from(RwtError::MathOverflow))?;
    // SECURITY: clamp NAV to minimum 1 when supply > 0
    // Prevents NAV=0 from integer truncation at extreme capital/supply ratios
    // (e.g., capital=1, supply=1_000_001 → raw NAV=0)
    Ok(core::cmp::max(nav_u64, 1))
}

#[cfg(test)]
mod tests {
    //! NAV characterization tests (tester recommendation N-3).
    //!
    //! `calculate_nav` is load-bearing for `mint_rwt`, `claim_yield`,
    //! `adjust_capital`, and `admin_mint_rwt`. These tests pin both the
    //! happy-path arithmetic and the security guards (zero-supply default,
    //! min-clamp, overflow rejection).
    use super::*;

    /// Spec: supply == 0 short-circuits to INITIAL_NAV regardless of capital.
    /// Avoids 0/0 and protects the bootstrap (first mint sees $1.00).
    #[test]
    fn nav_zero_supply_returns_initial_nav() {
        assert_eq!(calculate_nav(0, 0).unwrap(), INITIAL_NAV);
        assert_eq!(calculate_nav(123_456_789, 0).unwrap(), INITIAL_NAV);
        assert_eq!(calculate_nav(u128::MAX, 0).unwrap(), INITIAL_NAV);
    }

    /// Identity: capital == NAV_SCALE && supply == NAV_SCALE → NAV == NAV_SCALE.
    /// Confirms the fixed-point base point is unchanged across the formula.
    #[test]
    fn nav_unit_supply_returns_capital() {
        let nav = calculate_nav(NAV_SCALE as u128, NAV_SCALE).unwrap();
        assert_eq!(nav, NAV_SCALE);
        assert_eq!(nav, INITIAL_NAV);
    }

    /// Doubling capital with constant supply doubles NAV. Mirrors the
    /// `claim_yield` flow where book_value_share grows capital → NAV
    /// mechanically lifts on constant supply.
    #[test]
    fn nav_doubling_capital_doubles_nav() {
        let supply = NAV_SCALE; // 1.0 RWT in base units
        let nav_1x = calculate_nav(NAV_SCALE as u128, supply).unwrap();
        let nav_2x = calculate_nav(2 * NAV_SCALE as u128, supply).unwrap();
        assert_eq!(nav_1x, NAV_SCALE);
        assert_eq!(nav_2x, 2 * NAV_SCALE);
    }

    /// Halving supply with constant capital doubles NAV. Sanity check on
    /// the inverse-supply relation (matters for `adjust_capital` which can
    /// shift either side of the ratio).
    #[test]
    fn nav_halving_supply_doubles_nav() {
        let capital = NAV_SCALE as u128; // $1.00
        let nav_full = calculate_nav(capital, NAV_SCALE).unwrap();
        let nav_half = calculate_nav(capital, NAV_SCALE / 2).unwrap();
        assert_eq!(nav_full, NAV_SCALE);
        assert_eq!(nav_half, 2 * NAV_SCALE);
    }

    /// SECURITY: clamp NAV to 1 when integer truncation would yield 0
    /// (extreme capital/supply ratio). Without the clamp, downstream pricing
    /// math (e.g. `mint_rwt`) would divide by zero or give free RWT.
    #[test]
    fn nav_clamp_at_minimum_when_truncation_yields_zero() {
        // capital=1, supply=NAV_SCALE+1 → raw nav = (1 * NAV_SCALE) / (NAV_SCALE+1) = 0
        let nav = calculate_nav(1u128, NAV_SCALE + 1).unwrap();
        assert_eq!(nav, 1, "NAV must clamp to 1, not 0, on truncation");
    }

    /// Overflow rejection: a NAV that exceeds u64::MAX is refused with
    /// `MathOverflow`, not silently truncated. Protects `mint_rwt` /
    /// `claim_yield` capital math from wrap-around.
    #[test]
    fn nav_overflow_rejected_with_math_overflow() {
        // capital = u128::MAX, supply = 1 → mul_div would yield ~u128::MAX * NAV_SCALE / 1
        // which overflows u64 (and the wide arithmetic still returns a u128
        // > u64::MAX, so try_from fails).
        let result = calculate_nav(u128::MAX, 1);
        assert!(result.is_err(), "expected MathOverflow, got {:?}", result);
        let expected: ProgramError = RwtError::MathOverflow.into();
        match result {
            Err(e) => assert_eq!(format!("{:?}", e), format!("{:?}", expected)),
            Ok(_) => panic!("overflow path must error"),
        }
    }


    /// CU-hotfix regression (2026-05-18). Eagerly-evaluated
    /// `Option::ok_or(ProgramError::from(E))` calls invoke the
    /// arlex-derive `From<E>` impl on the success path, which calls
    /// `arlex_lang::log(msg)` — burning ~100 CUs per call site and
    /// emitting a spurious "Arithmetic overflow" log line on every
    /// instruction. See `rwt-engine/src/instructions/mint_rwt.rs`
    /// (`mint_rwt_has_no_eager_ok_or_program_error`) for the full
    /// background and the smoke-3 trace that first exposed this.
    ///
    /// The detection key is reassembled from two halves so this
    /// test's own definition of it does not match.
    #[test]
    fn no_eager_ok_or_program_error() {
        const SRC: &str = include_str!("nav.rs");
        const HALF_1: &str = ".ok_or(ProgramError";
        const HALF_2: &str = "::from(";
        let bad_needle = alloc::format!("{HALF_1}{HALF_2}");
        let mut hits = 0usize;
        for raw_line in SRC.lines() {
            let line = match raw_line.find("//") {
                Some(idx) => &raw_line[..idx],
                None => raw_line,
            };
            if let Some(needle_pos) = line.find(&bad_needle) {
                if line[..needle_pos].contains('"') {
                    continue;
                }
                hits += 1;
            }
        }
        assert_eq!(
            hits, 0,
            "found {hits} eager .ok_or(ProgramError-from(...)) calls — \
             use .ok_or_else(|| ...) closure form to keep the error \
             construction (and its arlex_lang::log syscall) off the \
             success path (CU-hotfix 2026-05-18)",
        );
    }
}
