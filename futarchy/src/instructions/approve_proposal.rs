use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::error::FutarchyError;
use crate::events::ProposalApproved;
use crate::state::*;

#[derive(Accounts)]
pub struct ApproveProposal<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(has_one = authority, account_type = "FutarchyConfig")]
    pub config: &'info AccountView,

    #[account(mut)]
    pub proposal: &'info AccountView,
}

pub fn handler(ctx: Context<ApproveProposal>) -> Result<()> {
    let config = FutarchyConfig::load(ctx.accounts.config, ctx.program_id)?;

    if !config.is_active {
        return Err(ProgramError::from(FutarchyError::GovernancePaused));
    }

    // `mut` binding: status write goes through the guard's DerefMut. No CPI.
    let mut proposal = Proposal::load_mut(ctx.accounts.proposal, ctx.program_id)?;

    // SECURITY (H-3): Validate proposal PDA derives from ["proposal", config, proposal_id]
    let (expected_proposal, _) = arlex_lang::find_program_address(
        &[
            b"proposal",
            ctx.accounts.config.address().as_ref(),
            &proposal.proposal_id.to_le_bytes(),
        ],
        ctx.program_id,
    );
    if ctx.accounts.proposal.address() != &expected_proposal {
        return Err(ProgramError::from(FutarchyError::InvalidProposal));
    }

    // SECURITY: Validate proposal belongs to this config (prevents cross-config action)
    if proposal.ot_mint != config.ot_mint {
        return Err(ProgramError::from(FutarchyError::ProposalConfigMismatch));
    }

    if proposal.status != STATUS_ACTIVE {
        return Err(ProgramError::from(FutarchyError::ProposalNotActive));
    }

    proposal.status = STATUS_APPROVED;

    let mut approver = [0u8; 32];
    approver.copy_from_slice(ctx.accounts.authority.address().as_ref());

    let clock = Clock::get()?;
    emit!(ProposalApproved {
        proposal_id: proposal.proposal_id,
        approver,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Proposal approved");
    Ok(())
}
