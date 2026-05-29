//! 2-step authority rotation, mirrors `rwt-engine` (staking.mdx §propose/accept).
//!
//! - `propose`: signer == authority; sets pending_authority, has_pending=true.
//!   Reverts SelfTransfer if new == current. Overwrites any prior pending.
//! - `accept`: signer == pending_authority (InvalidPendingAuthority;
//!   NoPendingAuthority if none). Promotes pending → authority, clears pending.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::STAKING_CONFIG_SEED;
use crate::error::StakingError;
use crate::events::{AuthorityTransferAccepted, AuthorityTransferProposed};
use crate::state::StakingConfig;

// --- Propose ---

#[derive(Accounts)]
pub struct ProposeAuthorityTransfer<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,
}

pub fn propose_handler(
    ctx: Context<ProposeAuthorityTransfer>,
    new_authority: [u8; 32],
) -> Result<()> {
    let config = StakingConfig::load_mut(ctx.accounts.staking_config, ctx.program_id)?;

    if ctx.accounts.authority.address().as_ref() != config.authority.as_ref() {
        return Err(ProgramError::from(StakingError::Unauthorized));
    }
    if new_authority == [0u8; 32] {
        return Err(ProgramError::from(StakingError::ZeroAddress));
    }
    if new_authority == config.authority {
        return Err(ProgramError::from(StakingError::SelfTransfer));
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

    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,
}

pub fn accept_handler(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
    let config = StakingConfig::load_mut(ctx.accounts.staking_config, ctx.program_id)?;

    if !config.has_pending {
        return Err(ProgramError::from(StakingError::NoPendingAuthority));
    }
    if ctx.accounts.new_authority.address().as_ref() != config.pending_authority.as_ref() {
        return Err(ProgramError::from(StakingError::InvalidPendingAuthority));
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
