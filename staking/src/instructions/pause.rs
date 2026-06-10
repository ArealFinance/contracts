//! `pause` / `unpause` — pause-authority-only emergency stop on stake /
//! unstake. `deposit_rewards` is NEVER gated by pause (staking.mdx §Instructions).

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::STAKING_CONFIG_SEED;
use crate::error::StakingError;
use crate::events::StakingPauseToggled;
use crate::state::StakingConfig;

#[derive(Accounts)]
pub struct PauseStaking<'info> {
    #[account(signer)]
    pub pause_authority: &'info AccountView,

    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,
}

#[derive(Accounts)]
pub struct UnpauseStaking<'info> {
    #[account(signer)]
    pub pause_authority: &'info AccountView,

    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,
}

fn set_paused(
    config_account: &AccountView,
    signer: &AccountView,
    program_id: &Address,
    paused: bool,
) -> Result<()> {
    // `mut` binding: is_paused write goes through the guard's DerefMut. No CPI.
    let mut config = StakingConfig::load_mut(config_account, program_id)?;
    if signer.address().as_ref() != config.pause_authority.as_ref() {
        return Err(ProgramError::from(StakingError::UnauthorizedPause));
    }
    config.is_paused = paused;

    let clock = Clock::get()?;
    emit!(StakingPauseToggled {
        is_paused: paused,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}

pub fn pause_handler(ctx: Context<PauseStaking>) -> Result<()> {
    set_paused(
        ctx.accounts.staking_config,
        ctx.accounts.pause_authority,
        ctx.program_id,
        true,
    )
}

pub fn unpause_handler(ctx: Context<UnpauseStaking>) -> Result<()> {
    set_paused(
        ctx.accounts.staking_config,
        ctx.accounts.pause_authority,
        ctx.program_id,
        false,
    )
}
