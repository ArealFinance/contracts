//! `pause` / `unpause` — emergency stop on stake / unstake. `deposit_rewards`
//! is NEVER gated by pause (staking.mdx §Instructions).
//!
//! Asymmetric guardian model:
//!   - **pause**:  any ONE of the (up to 3) non-zero `pause_authorities`
//!     guardian slots can pause. The emergency brake must be fast, so the
//!     pause power is spread across independent guardian keys.
//!   - **unpause**: ONLY `config.authority` can unpause. Unpausing is never
//!     time-critical, and a leaked guardian key must NOT be able to lift a
//!     pause mid-incident.
//! Guardian slots are immutable after `initialize` (change only via program
//! upgrade), deliberately NOT rotatable by the authority.

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
    pub authority: &'info AccountView,

    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,
}

/// Flip `is_paused` and emit the toggle event. The auth gate is enforced by the
/// caller (pause vs unpause differ), so this helper only writes + emits.
fn set_paused(config: &mut StakingConfig, paused: bool) -> Result<()> {
    config.is_paused = paused;

    let clock = Clock::get()?;
    emit!(StakingPauseToggled {
        is_paused: paused,
        timestamp: clock.unix_timestamp,
    });
    Ok(())
}

pub fn pause_handler(ctx: Context<PauseStaking>) -> Result<()> {
    // `mut` binding: is_paused write goes through the guard's DerefMut. No CPI.
    let mut config = StakingConfig::load_mut(ctx.accounts.staking_config, ctx.program_id)?;

    // Copy the packed guardian slots to a local before comparing — no
    // unaligned reference into the repr(C, packed) struct.
    let pause_authorities = config.pause_authorities;
    let signer = ctx.accounts.pause_authority.address().as_ref();

    // Manual check: signer must match ANY non-zero guardian slot. Zeroed slots
    // are skipped explicitly so a zero-address signer can never match an
    // unused slot.
    let mut authorized = false;
    for guardian in pause_authorities.iter() {
        if *guardian != [0u8; 32] && signer == guardian.as_ref() {
            authorized = true;
            break;
        }
    }
    if !authorized {
        return Err(ProgramError::from(StakingError::UnauthorizedPause));
    }

    // Idempotent — double pause succeeds silently.
    set_paused(&mut config, true)
}

pub fn unpause_handler(ctx: Context<UnpauseStaking>) -> Result<()> {
    // `mut` binding: is_paused write goes through the guard's DerefMut. No CPI.
    let mut config = StakingConfig::load_mut(ctx.accounts.staking_config, ctx.program_id)?;

    // Authority-only gate (a leaked guardian key cannot lift a pause). Copy the
    // packed field to a local before comparing.
    let current_authority = config.authority;
    if ctx.accounts.authority.address().as_ref() != current_authority.as_ref() {
        return Err(ProgramError::from(StakingError::Unauthorized));
    }

    // Idempotent — double unpause succeeds silently.
    set_paused(&mut config, false)
}
