//! authority_transfer — two-step rotation of the DistributionConfig authority.
//!
//! Mirrors the pattern used by rwt-engine:
//!   * propose: current authority stores new candidate in `pending_authority`
//!   * accept:  candidate signs, authority swaps in, pending cleared

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::error::YdError;
use crate::events::{AuthorityTransferAccepted, AuthorityTransferProposed};
use crate::state::DistributionConfig;

// --- Propose ---

#[derive(Accounts)]
pub struct ProposeAuthorityTransfer<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "DistributionConfig",
        seeds = [b"dist_config"], bump
    )]
    pub config: &'info AccountView,
}

pub fn propose_handler(
    ctx: Context<ProposeAuthorityTransfer>,
    new_authority: [u8; 32],
) -> Result<()> {
    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut config = DistributionConfig::load_mut(ctx.accounts.config, ctx.program_id)?;

    if new_authority == [0u8; 32] {
        return Err(ProgramError::from(YdError::ZeroDestination));
    }
    if new_authority == config.authority {
        return Err(ProgramError::from(YdError::SelfTransfer));
    }

    let current_authority = config.authority;

    // Overwrite any existing pending transfer.
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

    // NOTE: account_type requires has_one — but we validate pending manually.
    #[account(mut, seeds = [b"dist_config"], bump)]
    pub config: &'info AccountView,
}

pub fn accept_handler(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut config = DistributionConfig::load_mut(ctx.accounts.config, ctx.program_id)?;

    if !config.has_pending {
        return Err(ProgramError::from(YdError::NoPendingAuthority));
    }
    if ctx.accounts.new_authority.address().as_ref() != config.pending_authority.as_ref() {
        return Err(ProgramError::from(YdError::InvalidPendingAuthority));
    }

    let old_authority = config.authority;
    config
        .authority
        .copy_from_slice(ctx.accounts.new_authority.address().as_ref());
    let new_authority = config.authority;
    config.pending_authority = [0u8; 32];
    config.has_pending = false;

    let clock = Clock::get()?;
    emit!(AuthorityTransferAccepted {
        old_authority,
        new_authority,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
