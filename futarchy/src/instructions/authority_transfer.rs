use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::error::FutarchyError;
use crate::events::{AuthorityTransferProposed, AuthorityTransferAccepted};
use crate::state::*;

// =============================================================================
// Propose Authority Transfer
// =============================================================================

#[derive(Accounts)]
pub struct ProposeAuthorityTransfer<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut,
        has_one = authority, account_type = "FutarchyConfig",
    )]
    pub config: &'info AccountView,
}

pub fn propose_handler(
    ctx: Context<ProposeAuthorityTransfer>,
    new_authority: [u8; 32],
) -> Result<()> {
    // NOTE: is_active NOT checked — authority must be able to transfer to unfreeze
    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut config = FutarchyConfig::load_mut(ctx.accounts.config, ctx.program_id)?;

    // SECURITY (L-2): reject zero-address as new authority
    if new_authority == [0u8; 32] {
        return Err(ProgramError::from(FutarchyError::ZeroAuthority));
    }

    if new_authority == config.authority {
        return Err(ProgramError::from(FutarchyError::SelfTransfer));
    }

    let current = config.authority;
    config.pending_authority = new_authority;
    config.has_pending = true;

    let clock = Clock::get()?;
    emit!(AuthorityTransferProposed {
        current_authority: current,
        pending_authority: new_authority,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Authority transfer proposed");
    Ok(())
}

// =============================================================================
// Accept Authority Transfer
// =============================================================================

#[derive(Accounts)]
pub struct AcceptAuthorityTransfer<'info> {
    #[account(signer)]
    pub new_authority: &'info AccountView,

    #[account(mut)]
    pub config: &'info AccountView,
}

pub fn accept_handler(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
    // NOTE: is_active NOT checked — same design decision as propose
    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut config = FutarchyConfig::load_mut(ctx.accounts.config, ctx.program_id)?;

    if !config.has_pending {
        return Err(ProgramError::from(FutarchyError::NoPendingAuthority));
    }
    if ctx.accounts.new_authority.address().as_ref() != config.pending_authority.as_ref() {
        return Err(ProgramError::from(FutarchyError::InvalidPendingAuthority));
    }

    let old = config.authority;
    config.authority.copy_from_slice(ctx.accounts.new_authority.address().as_ref());
    config.pending_authority = [0u8; 32];
    config.has_pending = false;

    let clock = Clock::get()?;
    emit!(AuthorityTransferAccepted {
        old_authority: old,
        new_authority: config.authority,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Authority transfer accepted");
    Ok(())
}
