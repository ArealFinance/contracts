//! `update_config(reward_depositor, min_stake_amount, cooldown_seconds)` —
//! authority-only tuning (staking.mdx §Instructions / update_config).
//!
//! Governance can raise/lower operational values, but not below the contract
//! security floors used at initialization.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::{COOLDOWN_SECONDS, MIN_STAKE_AMOUNT, STAKING_CONFIG_SEED};
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

fn normalize_update_config_inputs(
    reward_depositor: [u8; 32],
    min_stake_amount: u64,
    cooldown_seconds: i64,
) -> Result<(u64, i64)> {
    if reward_depositor == [0u8; 32] {
        return Err(ProgramError::from(StakingError::ZeroAddress));
    }

    Ok((
        min_stake_amount.max(MIN_STAKE_AMOUNT),
        cooldown_seconds.max(COOLDOWN_SECONDS),
    ))
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

    // Validate and normalize inputs.
    let (min_stake_amount, cooldown_seconds) =
        normalize_update_config_inputs(reward_depositor, min_stake_amount, cooldown_seconds)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_code(err: ProgramError) -> u32 {
        match err {
            ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    fn assert_err_code(result: Result<(u64, i64)>, expected: StakingError) {
        assert_eq!(
            custom_code(result.unwrap_err()),
            custom_code(ProgramError::from(expected)),
        );
    }

    #[test]
    fn update_config_rejects_zero_reward_depositor() {
        assert_err_code(
            normalize_update_config_inputs([0u8; 32], MIN_STAKE_AMOUNT, COOLDOWN_SECONDS),
            StakingError::ZeroAddress,
        );
    }

    #[test]
    fn update_config_clamps_min_stake_and_cooldown_to_security_floors() {
        assert_eq!(
            normalize_update_config_inputs([1u8; 32], 0, 0).unwrap(),
            (MIN_STAKE_AMOUNT, COOLDOWN_SECONDS),
        );
        assert_eq!(
            normalize_update_config_inputs([1u8; 32], MIN_STAKE_AMOUNT - 1, -1).unwrap(),
            (MIN_STAKE_AMOUNT, COOLDOWN_SECONDS),
        );
    }

    #[test]
    fn update_config_keeps_values_at_or_above_security_floors() {
        assert_eq!(
            normalize_update_config_inputs([1u8; 32], MIN_STAKE_AMOUNT, COOLDOWN_SECONDS).unwrap(),
            (MIN_STAKE_AMOUNT, COOLDOWN_SECONDS),
        );
        assert_eq!(
            normalize_update_config_inputs([1u8; 32], MIN_STAKE_AMOUNT + 1, COOLDOWN_SECONDS + 1,)
                .unwrap(),
            (MIN_STAKE_AMOUNT + 1, COOLDOWN_SECONDS + 1),
        );
    }
}
