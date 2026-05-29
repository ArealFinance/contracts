//! `propose_authority_transfer` / `accept_authority_transfer` — 2-step
//! authority rotation (mirrors `rwt-engine`).
//!
//! - propose: signer == authority; sets `pending_authority`, `has_pending`.
//!   `SelfTransfer` if `new_authority == authority`. Overwrites prior pending.
//! - accept:  signer == pending_authority (`InvalidPendingAuthority`;
//!   `NoPendingAuthority` if none). Promotes pending → authority.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::EARN_CONFIG_SEED;
use crate::error::EarnError;
use crate::events::{AuthorityTransferAccepted, AuthorityTransferProposed};
use crate::state::EarnConfig;

// --- Propose ---

#[derive(Accounts)]
pub struct ProposeAuthorityTransfer<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "EarnConfig",
        seeds = [EARN_CONFIG_SEED], bump
    )]
    pub earn_config: &'info AccountView,
}

pub fn propose_handler(
    ctx: Context<ProposeAuthorityTransfer>,
    new_authority: [u8; 32],
) -> Result<()> {
    let config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

    if new_authority == [0u8; 32] {
        return Err(ProgramError::from(EarnError::ZeroDestination));
    }
    if new_authority == config.authority {
        return Err(ProgramError::from(EarnError::SelfTransfer));
    }

    let current_authority = config.authority;

    // Overwrites any existing pending transfer.
    config.pending_authority = new_authority;
    config.has_pending = true;

    let clock = Clock::get()?;
    emit!(AuthorityTransferProposed {
        current_authority,
        pending_authority: new_authority,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

// --- Accept ---

#[derive(Accounts)]
pub struct AcceptAuthorityTransfer<'info> {
    #[account(signer)]
    pub new_authority: &'info AccountView,

    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,
}

pub fn accept_handler(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
    let config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

    if !config.has_pending {
        return Err(ProgramError::from(EarnError::NoPendingAuthority));
    }

    if ctx.accounts.new_authority.address().as_ref() != config.pending_authority.as_ref() {
        return Err(ProgramError::from(EarnError::InvalidPendingAuthority));
    }

    let old_authority = config.authority;

    config.authority.copy_from_slice(ctx.accounts.new_authority.address().as_ref());
    config.pending_authority = [0u8; 32];
    config.has_pending = false;

    let clock = Clock::get()?;
    emit!(AuthorityTransferAccepted {
        old_authority,
        new_authority: config.authority,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
