use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::OtError;
use crate::events::{AuthorityTransferProposed, AuthorityTransferAccepted};
use crate::state::*;

// =============================================================================
// Propose Authority Transfer
// =============================================================================

#[derive(Accounts)]
pub struct ProposeAuthorityTransfer<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    // OT mint — for PDA seed derivation, validated as SPL Mint
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    // OtGovernance PDA — validated via seeds + has_one
    #[account(
        mut,
        has_one = authority, account_type = "OtGovernance",
        seeds = [b"ot_governance", ot_mint.address().as_ref()], bump
    )]
    pub ot_governance: &'info AccountView,
}

pub fn propose_handler(
    ctx: Context<ProposeAuthorityTransfer>,
    new_authority: [u8; 32],
) -> Result<()> {
    // NOTE: is_active is NOT checked — design decision to prevent permanent lockout.

    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut governance = OtGovernance::load_mut(ctx.accounts.ot_governance, ctx.program_id)?;

    // SECURITY (L-1): reject zero-address as new authority
    if new_authority == [0u8; 32] {
        return Err(ProgramError::from(OtError::ZeroAuthority));
    }

    if new_authority == governance.authority {
        return Err(ProgramError::from(OtError::AuthorityTransferToSelf));
    }

    let current_authority = governance.authority;

    governance.pending_authority = new_authority;
    governance.has_pending = true;

    let clock = Clock::get()?;
    emit!(AuthorityTransferProposed {
        ot_mint: governance.ot_mint,
        current_authority,
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

    // OT mint — for PDA seed derivation, validated as SPL Mint
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    // OtGovernance PDA — validated via seeds
    #[account(
        mut,
        seeds = [b"ot_governance", ot_mint.address().as_ref()], bump
    )]
    pub ot_governance: &'info AccountView,
}

pub fn accept_handler(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
    // NOTE: is_active is NOT checked — same design decision as propose.

    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut governance = OtGovernance::load_mut(ctx.accounts.ot_governance, ctx.program_id)?;

    if !governance.has_pending {
        return Err(ProgramError::from(OtError::NoPendingAuthority));
    }
    if ctx.accounts.new_authority.address().as_ref() != governance.pending_authority.as_ref() {
        return Err(ProgramError::from(OtError::InvalidPendingAuthority));
    }

    let old_authority = governance.authority;

    governance.authority.copy_from_slice(ctx.accounts.new_authority.address().as_ref());
    governance.pending_authority = [0u8; 32];
    governance.has_pending = false;

    let clock = Clock::get()?;
    emit!(AuthorityTransferAccepted {
        ot_mint: governance.ot_mint,
        old_authority,
        new_authority: governance.authority,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Authority transfer accepted");
    Ok(())
}
