//! `update_config` — authority-only admin tuning.
//!
//! Lets the authority retune the mint fee (bps), the minimum mint amount, and
//! the DAO fee destination. The `basket_vault`, `rwt_mint`, `usdc_mint`, and
//! `pause_authority` are IMMUTABLE — set once at `initialize`.
//!
//! `mint_fee_bps` is capped at BPS_DENOMINATOR (100%); `dao_fee_destination`
//! cannot be the zero address.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::{BPS_DENOMINATOR, EARN_CONFIG_SEED};
use crate::error::EarnError;
use crate::events::EarnConfigUpdated;
use crate::state::EarnConfig;

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "EarnConfig",
        seeds = [EARN_CONFIG_SEED], bump
    )]
    pub earn_config: &'info AccountView,
}

pub fn handler(
    ctx: Context<UpdateConfig>,
    mint_fee_bps: u16,
    min_mint_amount: u64,
    dao_fee_destination: [u8; 32],
) -> Result<()> {
    let config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

    // --- Checks ---
    if mint_fee_bps as u64 > BPS_DENOMINATOR {
        return Err(ProgramError::from(EarnError::InvalidTokenAccount));
    }
    if dao_fee_destination == [0u8; 32] {
        return Err(ProgramError::from(EarnError::InvalidFeeDestination));
    }

    // --- Effects ---
    config.mint_fee_bps = mint_fee_bps;
    config.min_mint_amount = min_mint_amount;
    config.dao_fee_destination = dao_fee_destination;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(EarnConfigUpdated {
        mint_fee_bps,
        min_mint_amount,
        dao_fee_destination,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
