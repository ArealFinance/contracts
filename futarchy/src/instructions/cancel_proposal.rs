use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::error::FutarchyError;
use crate::events::ProposalCancelled;
use crate::state::*;

#[derive(Accounts)]
pub struct CancelProposal<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(has_one = authority, account_type = "FutarchyConfig")]
    pub config: &'info AccountView,

    #[account(mut)]
    pub proposal: &'info AccountView,
}

pub fn handler(ctx: Context<CancelProposal>) -> Result<()> {
    let config = FutarchyConfig::load(ctx.accounts.config, ctx.program_id)?;

    if !config.is_active {
        return Err(ProgramError::from(FutarchyError::GovernancePaused));
    }

    let proposal = Proposal::load_mut(ctx.accounts.proposal, ctx.program_id)?;

    // SECURITY: Validate proposal belongs to this config (prevents cross-config action)
    if proposal.ot_mint != config.ot_mint {
        return Err(ProgramError::from(FutarchyError::ProposalConfigMismatch));
    }

    if proposal.status != STATUS_ACTIVE {
        return Err(ProgramError::from(FutarchyError::ProposalNotActive));
    }

    proposal.status = STATUS_CANCELLED;

    let mut authority_bytes = [0u8; 32];
    authority_bytes.copy_from_slice(ctx.accounts.authority.address().as_ref());

    let clock = Clock::get()?;
    emit!(ProposalCancelled {
        proposal_id: proposal.proposal_id,
        authority: authority_bytes,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Proposal cancelled");
    Ok(())
}
