//! claim_ot_governance — accept OT authority transfer on behalf of Futarchy config PDA.
//! Permissionless — anyone can trigger once OT has proposed.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::FutarchyError;
use crate::events::OtGovernanceClaimed;
use crate::state::*;

#[derive(Accounts)]
pub struct ClaimOtGovernance<'info> {
    #[account(mut, signer)]
    pub executor: &'info AccountView,

    pub config: &'info AccountView,

    #[account(mut)]
    pub ot_governance: &'info AccountView,

    pub ot_mint: &'info AccountView,

    pub ot_program: &'info AccountView,
}

pub fn handler(ctx: Context<ClaimOtGovernance>) -> Result<()> {
    // Validate OT program
    if ctx.accounts.ot_program.address().as_ref() != OT_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(FutarchyError::InvalidOtProgram));
    }

    // Manual discriminator validation (no has_one, permissionless)
    let config = FutarchyConfig::load(ctx.accounts.config, ctx.program_id)?;

    // Defense-in-depth: validate ot_mint matches config
    if ctx.accounts.ot_mint.address().as_ref() != config.ot_mint.as_ref() {
        return Err(ProgramError::from(FutarchyError::OtMintMismatch));
    }

    // Defense-in-depth: validate ot_governance PDA derivation
    let (expected_gov, _) = arlex_lang::find_program_address(
        &[b"ot_governance", config.ot_mint.as_ref()],
        &Address::new_from_array(OT_PROGRAM_ID),
    );
    if ctx.accounts.ot_governance.address() != &expected_gov {
        return Err(ProgramError::from(FutarchyError::InvalidOtGovernance));
    }

    // Validate pending_authority on OT governance matches this Futarchy config PDA
    // OtGovernance layout (after 8-byte discriminator):
    //   [8..40]   ot_mint       (32)
    //   [40..72]  authority     (32)
    //   [72..104] pending_auth  (32)
    //   [104]     has_pending   (1)
    let gov_data = unsafe {
        core::slice::from_raw_parts(
            ctx.accounts.ot_governance.data_ptr(),
            ctx.accounts.ot_governance.data_len(),
        )
    };
    if gov_data.len() < 105 {
        return Err(ProgramError::InvalidAccountData);
    }
    let has_pending = gov_data[104] != 0;
    if !has_pending {
        return Err(ProgramError::from(FutarchyError::GovernanceClaimMismatch));
    }
    let pending = &gov_data[72..104];
    if pending != ctx.accounts.config.address().as_ref() {
        return Err(ProgramError::from(FutarchyError::GovernanceClaimMismatch));
    }

    // CPI → OT::accept_authority_transfer
    crate::cpi::cpi_accept_authority_transfer(
        &config,
        ctx.accounts.config,
        ctx.accounts.ot_mint,
        ctx.accounts.ot_governance,
        ctx.accounts.ot_program,
    )?;

    let mut config_bytes = [0u8; 32];
    config_bytes.copy_from_slice(ctx.accounts.config.address().as_ref());

    let clock = Clock::get()?;
    emit!(OtGovernanceClaimed {
        ot_mint: config.ot_mint,
        futarchy_config: config_bytes,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("OT governance claimed");
    Ok(())
}
