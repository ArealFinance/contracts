//! `update_config(reward_depositor, min_stake_amount, cooldown_seconds)` —
//! authority-only tuning (staking.mdx §Instructions / update_config).

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::STAKING_CONFIG_SEED;
use crate::error::StakingError;
use crate::events::StakingConfigUpdated;
use crate::state::StakingConfig;

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,
}

pub fn handler(
    ctx: Context<UpdateConfig>,
    reward_depositor: [u8; 32],
    min_stake_amount: u64,
    cooldown_seconds: i64,
) -> Result<()> {
    let config = StakingConfig::load_mut(ctx.accounts.staking_config, ctx.program_id)?;

    // Authority gate.
    if ctx.accounts.authority.address().as_ref() != config.authority.as_ref() {
        return Err(ProgramError::from(StakingError::Unauthorized));
    }

    // Validate inputs.
    if reward_depositor == [0u8; 32] {
        return Err(ProgramError::from(StakingError::ZeroAddress));
    }
    if cooldown_seconds < 0 {
        return Err(ProgramError::from(StakingError::MathOverflow));
    }

    config.reward_depositor = reward_depositor;
    config.min_stake_amount = min_stake_amount;
    config.cooldown_seconds = cooldown_seconds;

    let clock = Clock::get()?;
    emit!(StakingConfigUpdated {
        reward_depositor,
        min_stake_amount,
        cooldown_seconds,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
