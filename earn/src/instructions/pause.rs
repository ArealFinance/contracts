//! `pause` / `unpause` — pause-authority-only emergency stop for mint flow.
//!
//! `add_to_basket`, `writedown_capital`, `update_config` are NOT gated by
//! pause (admin must remain able to write down impaired capital or reinvest
//! income even during a pause). Only `mint_rwt` checks `is_paused`.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::EARN_CONFIG_SEED;
use crate::error::EarnError;
use crate::events::EarnPauseToggled;
use crate::state::EarnConfig;

#[derive(Accounts)]
pub struct PauseEarn<'info> {
    #[account(signer)]
    pub pause_authority: &'info AccountView,

    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,
}

#[derive(Accounts)]
pub struct UnpauseEarn<'info> {
    #[account(signer)]
    pub pause_authority: &'info AccountView,

    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,
}

pub fn pause_handler(ctx: Context<PauseEarn>) -> Result<()> {
    let config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

    // Manual check: signer must be pause_authority (not authority — different role).
    if ctx.accounts.pause_authority.address().as_ref() != config.pause_authority.as_ref() {
        return Err(ProgramError::from(EarnError::UnauthorizedPause));
    }

    // Idempotent — double pause succeeds silently.
    config.is_paused = true;

    let clock = Clock::get()?;
    emit!(EarnPauseToggled {
        is_paused: true,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

pub fn unpause_handler(ctx: Context<UnpauseEarn>) -> Result<()> {
    let config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

    if ctx.accounts.pause_authority.address().as_ref() != config.pause_authority.as_ref() {
        return Err(ProgramError::from(EarnError::UnauthorizedPause));
    }

    // Idempotent — double unpause succeeds silently.
    config.is_paused = false;

    let clock = Clock::get()?;
    emit!(EarnPauseToggled {
        is_paused: false,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
