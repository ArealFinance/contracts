use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::error::RwtError;
use crate::events::{AuthorityTransferProposed, AuthorityTransferAccepted};
use crate::state::RwtVault;

// --- Propose ---

#[derive(Accounts)]
pub struct ProposeAuthorityTransfer<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "RwtVault",
        seeds = [b"rwt_vault"], bump
    )]
    pub rwt_vault: &'info AccountView,
}

pub fn propose_handler(
    ctx: Context<ProposeAuthorityTransfer>,
    new_authority: [u8; 32],
) -> Result<()> {
    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    if new_authority == [0u8; 32] {
        return Err(ProgramError::from(RwtError::ZeroDestination));
    }
    if new_authority == vault.authority {
        return Err(ProgramError::from(RwtError::SelfTransfer));
    }

    let current_authority = vault.authority;

    // Overwrites any existing pending transfer
    vault.pending_authority = new_authority;
    vault.has_pending = true;

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

    // NOTE: account_type requires has_one (Arlex constraint). Discriminator checked by load_mut.
    #[account(mut, seeds = [b"rwt_vault"], bump)]
    pub rwt_vault: &'info AccountView,
}

pub fn accept_handler(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    if !vault.has_pending {
        return Err(ProgramError::from(RwtError::NoPendingAuthority));
    }

    if ctx.accounts.new_authority.address().as_ref() != vault.pending_authority.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidPendingAuthority));
    }

    let old_authority = vault.authority;

    vault.authority.copy_from_slice(ctx.accounts.new_authority.address().as_ref());
    vault.pending_authority = [0u8; 32];
    vault.has_pending = false;

    let clock = Clock::get()?;
    emit!(AuthorityTransferAccepted {
        old_authority,
        new_authority: vault.authority,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
