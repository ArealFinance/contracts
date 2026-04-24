//! execute_proposal — permissionless CPI to OT contract.
//!
//! SECURITY: Executor passes OT accounts — Futarchy MUST validate them
//! against proposal fields before CPI to prevent destination hijacking.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};
use sha2::{Sha256, Digest};

use crate::constants::*;
use crate::error::FutarchyError;
use crate::events::ProposalExecuted;
use crate::state::*;

#[derive(Accounts)]
pub struct ExecuteProposal<'info> {
    #[account(mut, signer)]
    pub executor: &'info AccountView,

    pub config: &'info AccountView,

    #[account(mut)]
    pub proposal: &'info AccountView,

    pub ot_program: &'info AccountView,
    // Remaining accounts vary by proposal_type
}

pub fn handler(ctx: Context<ExecuteProposal>) -> Result<()> {
    // Validate OT program
    if ctx.accounts.ot_program.address().as_ref() != OT_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(FutarchyError::InvalidOtProgram));
    }

    // Manual discriminator validation (no has_one, permissionless)
    let config = FutarchyConfig::load(ctx.accounts.config, ctx.program_id)?;
    let proposal = Proposal::load_mut(ctx.accounts.proposal, ctx.program_id)?;

    // SECURITY (M-3): Validate config PDA derives from ["futarchy_config", ot_mint]
    let (expected_config, _) = arlex_lang::find_program_address(
        &[b"futarchy_config", config.ot_mint.as_ref()],
        ctx.program_id,
    );
    if ctx.accounts.config.address() != &expected_config {
        return Err(ProgramError::from(FutarchyError::InvalidFutarchyConfig));
    }

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

    // SECURITY: Validate proposal belongs to this config (prevents cross-config replay)
    if proposal.ot_mint != config.ot_mint {
        return Err(ProgramError::from(FutarchyError::ProposalConfigMismatch));
    }

    // State machine: only Approved → Executed
    if proposal.status == STATUS_EXECUTED {
        return Err(ProgramError::from(FutarchyError::AlreadyExecuted));
    }
    if proposal.status != STATUS_APPROVED {
        return Err(ProgramError::from(FutarchyError::ProposalNotApproved));
    }

    // NOTE: is_active NOT checked — approved proposals are always executable

    // Defense-in-depth: validate ot_governance PDA
    if ctx.remaining_accounts.is_empty() {
        return Err(ProgramError::NotEnoughAccountKeys);
    }
    let ot_governance_account = &ctx.remaining_accounts[0];
    let (expected_gov, _) = arlex_lang::find_program_address(
        &[b"ot_governance", config.ot_mint.as_ref()],
        &Address::new_from_array(OT_PROGRAM_ID),
    );
    if ot_governance_account.address() != &expected_gov {
        return Err(ProgramError::from(FutarchyError::InvalidOtGovernance));
    }

    // SECURITY (H-2): Checks-effects-interactions — mark executed BEFORE CPI
    let clock = Clock::get()?;
    proposal.status = STATUS_EXECUTED;
    proposal.executed_ts = clock.unix_timestamp;

    match proposal.proposal_type {
        PROPOSAL_TYPE_MINT_OT => execute_mint_ot(&ctx, &config, &proposal)?,
        PROPOSAL_TYPE_SPEND_TREASURY => execute_spend_treasury(&ctx, &config, &proposal)?,
        PROPOSAL_TYPE_UPDATE_DESTINATIONS => execute_update_destinations(&ctx, &config, &proposal)?,
        _ => return Err(ProgramError::from(FutarchyError::InvalidProposalType)),
    }

    let mut executor_bytes = [0u8; 32];
    executor_bytes.copy_from_slice(ctx.accounts.executor.address().as_ref());

    emit!(ProposalExecuted {
        proposal_id: proposal.proposal_id,
        proposal_type: proposal.proposal_type,
        executor: executor_bytes,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Proposal executed");
    Ok(())
}

/// MintOt CPI path
///
/// remaining_accounts layout:
/// [0] ot_governance
/// [1] ot_config (mut)
/// [2] ot_mint (mut)
/// [3] recipient_token_account (mut)
/// [4] recipient — MUST match proposal.destination
/// [5] token_program
/// [6] system_program
/// [7] ata_program
fn execute_mint_ot(
    ctx: &Context<ExecuteProposal>,
    config: &FutarchyConfig,
    proposal: &Proposal,
) -> ProgramResult {
    if ctx.remaining_accounts.len() < 8 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let ot_governance = &ctx.remaining_accounts[0];
    let ot_config = &ctx.remaining_accounts[1];
    let ot_mint = &ctx.remaining_accounts[2];
    let recipient_ata = &ctx.remaining_accounts[3];
    let recipient = &ctx.remaining_accounts[4];
    let token_program = &ctx.remaining_accounts[5];
    let system_program = &ctx.remaining_accounts[6];
    let ata_program = &ctx.remaining_accounts[7];

    // SECURITY: Validate ot_mint matches config.ot_mint
    if ot_mint.address().as_ref() != config.ot_mint.as_ref() {
        return Err(ProgramError::from(FutarchyError::OtMintMismatch));
    }

    // SECURITY: Validate recipient matches proposal.destination
    if recipient.address().as_ref() != proposal.destination.as_ref() {
        return Err(ProgramError::from(FutarchyError::DestinationMismatch));
    }

    crate::cpi::cpi_mint_ot(
        config,
        ctx.accounts.config,
        ot_governance,
        ot_config,
        ot_mint,
        recipient_ata,
        recipient,
        ctx.accounts.executor, // payer
        token_program,
        system_program,
        ata_program,
        ctx.accounts.ot_program,
        proposal.amount,
    )
}

/// SpendTreasury CPI path
///
/// remaining_accounts layout:
/// [0] ot_governance
/// [1] ot_mint
/// [2] ot_treasury
/// [3] treasury_token_account (mut)
/// [4] destination_token_account (mut)
/// [5] token_mint — MUST match proposal.token_mint
/// [6] token_program
fn execute_spend_treasury(
    ctx: &Context<ExecuteProposal>,
    config: &FutarchyConfig,
    proposal: &Proposal,
) -> ProgramResult {
    if ctx.remaining_accounts.len() < 7 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let ot_governance = &ctx.remaining_accounts[0];
    let ot_mint = &ctx.remaining_accounts[1];
    let ot_treasury = &ctx.remaining_accounts[2];
    let treasury_ata = &ctx.remaining_accounts[3];
    let destination_ata = &ctx.remaining_accounts[4];
    let token_mint = &ctx.remaining_accounts[5];
    let token_program = &ctx.remaining_accounts[6];

    // SECURITY: Validate ot_mint matches config.ot_mint
    if ot_mint.address().as_ref() != config.ot_mint.as_ref() {
        return Err(ProgramError::from(FutarchyError::OtMintMismatch));
    }

    // SECURITY: Validate token_mint matches proposal.token_mint
    if token_mint.address().as_ref() != proposal.token_mint.as_ref() {
        return Err(ProgramError::from(FutarchyError::TokenMintMismatch));
    }

    // SECURITY: Validate destination_ata owner matches proposal.destination
    let dest_data = unsafe {
        core::slice::from_raw_parts(destination_ata.data_ptr(), destination_ata.data_len())
    };
    if dest_data.len() < 64 {
        return Err(ProgramError::InvalidAccountData);
    }
    if &dest_data[32..64] != proposal.destination.as_ref() {
        return Err(ProgramError::from(FutarchyError::DestinationMismatch));
    }

    crate::cpi::cpi_spend_treasury(
        config,
        ctx.accounts.config,
        ot_mint,
        ot_governance,
        ot_treasury,
        treasury_ata,
        destination_ata,
        token_mint,
        token_program,
        ctx.accounts.ot_program,
        proposal.amount,
    )
}

/// UpdateDestinations CPI path
///
/// remaining_accounts layout:
/// [0] ot_governance
/// [1] ot_mint
/// [2] revenue_config (mut)
/// [3] destinations_data — account with serialized destinations
fn execute_update_destinations(
    ctx: &Context<ExecuteProposal>,
    config: &FutarchyConfig,
    proposal: &Proposal,
) -> ProgramResult {
    if ctx.remaining_accounts.len() < 4 {
        return Err(ProgramError::NotEnoughAccountKeys);
    }

    let ot_governance = &ctx.remaining_accounts[0];
    let ot_mint = &ctx.remaining_accounts[1];
    let revenue_config = &ctx.remaining_accounts[2];

    // SECURITY: Validate ot_mint matches config.ot_mint
    if ot_mint.address().as_ref() != config.ot_mint.as_ref() {
        return Err(ProgramError::from(FutarchyError::OtMintMismatch));
    }
    let destinations_data_account = &ctx.remaining_accounts[3];

    // SECURITY (M-4): defense-in-depth — require that the destinations payload
    // lives in a plain system-owned data account. The SHA256 check below is the
    // primary defence; this owner check removes the degenerate case where an
    // attacker points us at a program-controlled account whose data could
    // conceivably mutate between hash and read.
    if !destinations_data_account.owned_by(&Address::new_from_array(SYSTEM_PROGRAM)) {
        return Err(ProgramError::IllegalOwner);
    }

    // SECURITY: SHA256 hash verification
    let data = unsafe {
        core::slice::from_raw_parts(
            destinations_data_account.data_ptr(),
            destinations_data_account.data_len(),
        )
    };

    let mut hasher = Sha256::new();
    hasher.update(data);
    let hash = hasher.finalize();

    if hash.as_slice() != proposal.params_hash.as_ref() {
        return Err(ProgramError::from(FutarchyError::ParamsHashMismatch));
    }

    crate::cpi::cpi_batch_update_destinations(
        config,
        ctx.accounts.config,
        ot_mint,
        ot_governance,
        revenue_config,
        ctx.accounts.ot_program,
        data,
    )
}
