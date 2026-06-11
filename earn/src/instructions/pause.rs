//! `pause` / `unpause` — emergency stop for the mint flow.
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
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "EarnConfig",
        seeds = [EARN_CONFIG_SEED], bump
    )]
    pub earn_config: &'info AccountView,
}

pub fn pause_handler(ctx: Context<PauseEarn>) -> Result<()> {
    // `mut` binding: is_paused write goes through the guard's DerefMut. No CPI.
    let mut config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

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
    // Authority gate is enforced by `has_one = authority` on the Accounts
    // struct: only `config.authority` can unpause (a leaked guardian key
    // cannot lift a pause during an incident).
    //
    // `mut` binding: is_paused write goes through the guard's DerefMut. No CPI.
    let mut config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

    // Idempotent — double unpause succeeds silently.
    config.is_paused = false;

    let clock = Clock::get()?;
    emit!(EarnPauseToggled {
        is_paused: false,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
