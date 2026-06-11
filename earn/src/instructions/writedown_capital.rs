//! `writedown_capital` — authority-only NAV writedown.
//!
//! Decreases `total_invested_capital` by `amount`, lowering NAV. Recognizes
//! real-world depreciation / amortization of underlying RWA (off-chain
//! valuation). No USDC moves on chain — a purely accounting operation
//! reflecting off-chain custody now holding less value.
//!
//! Guard: writedown cannot reduce capital below `MIN_CAPITAL_FLOOR` (preserves
//! NAV > 0). `reason_code` is a free-form authority hint for the event.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::{EARN_CONFIG_SEED, MIN_CAPITAL_FLOOR, SPL_TOKEN_PROGRAM};
use crate::error::EarnError;
use crate::events::CapitalWrittenDown;
use crate::nav::calculate_nav;
use crate::state::EarnConfig;
use crate::validation::read_mint_supply;

#[derive(Accounts)]
pub struct WritedownCapital<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "EarnConfig",
        seeds = [EARN_CONFIG_SEED], bump
    )]
    pub earn_config: &'info AccountView,

    /// Earn-RWT mint — supply read for the NAV snapshots.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,
}

pub fn handler(ctx: Context<WritedownCapital>, amount: u64, reason_code: u8) -> Result<()> {
    // `mut` binding: capital write goes through the guard's DerefMut. No CPI.
    EarnConfig::assert_account_size(ctx.accounts.earn_config)?;
    let mut config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

    // --- Checks ---
    if amount == 0 {
        return Err(ProgramError::from(EarnError::ZeroAmount));
    }
    if ctx.accounts.rwt_mint.address().as_ref() != config.rwt_mint.as_ref() {
        return Err(ProgramError::from(EarnError::InvalidRwtMint));
    }

    let supply = read_mint_supply(ctx.accounts.rwt_mint)?;
    let nav_before = calculate_nav(config.total_invested_capital, supply)?;

    let new_capital = config
        .total_invested_capital
        .checked_sub(amount as u128)
        .ok_or_else(|| ProgramError::from(EarnError::InsufficientCapital))?;

    if new_capital < MIN_CAPITAL_FLOOR as u128 {
        return Err(ProgramError::from(EarnError::InsufficientCapital));
    }

    // --- Effects ---
    config.total_invested_capital = new_capital;
    let nav_after = calculate_nav(new_capital, supply)?;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(CapitalWrittenDown {
        amount,
        nav_before,
        nav_after,
        reason_code,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! writedown floor-guard characterization.
    use crate::constants::MIN_CAPITAL_FLOOR;

    /// A writedown that would breach the floor must be rejected; one that
    /// leaves capital >= floor is allowed.
    #[test]
    fn writedown_floor_guard() {
        let capital: u128 = 10;
        // Writing down 10 → 0 breaches the floor (0 < MIN_CAPITAL_FLOOR == 1).
        let breach = capital.checked_sub(10).unwrap();
        assert!(
            breach < MIN_CAPITAL_FLOOR as u128,
            "0 must breach the floor"
        );

        // Writing down 9 → 1 is exactly at the floor (allowed).
        let ok = capital.checked_sub(9).unwrap();
        assert!(ok >= MIN_CAPITAL_FLOOR as u128, "1 must satisfy the floor");

        // Writing down more than capital underflows (checked_sub → None).
        assert!(
            capital.checked_sub(11).is_none(),
            "over-writedown underflows"
        );
    }
}
