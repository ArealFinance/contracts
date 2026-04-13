use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::MIN_CAPITAL_FLOOR;
use crate::error::RwtError;
use crate::events::CapitalAdjusted;
use crate::nav::calculate_nav;
use crate::state::RwtVault;

#[derive(Accounts)]
pub struct AdjustCapital<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "RwtVault",
        seeds = [b"rwt_vault"], bump
    )]
    pub rwt_vault: &'info AccountView,
}

pub fn handler(ctx: Context<AdjustCapital>, writedown_amount: u64) -> Result<()> {
    let vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    // --- Checks ---
    if writedown_amount == 0 {
        return Err(ProgramError::from(RwtError::ZeroAmount));
    }

    let old_capital = vault.total_invested_capital;
    let old_nav = vault.nav_book_value;

    let new_capital = vault.total_invested_capital
        .checked_sub(writedown_amount as u128)
        .ok_or(ProgramError::from(RwtError::InsufficientCapital))?;

    if new_capital < MIN_CAPITAL_FLOOR as u128 {
        return Err(ProgramError::from(RwtError::InsufficientCapital));
    }

    // --- Effects ---
    vault.total_invested_capital = new_capital;
    vault.nav_book_value = calculate_nav(new_capital, vault.total_rwt_supply)?;

    let new_nav = vault.nav_book_value;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(CapitalAdjusted {
        old_capital,
        new_capital,
        writedown_amount,
        old_nav,
        new_nav,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
