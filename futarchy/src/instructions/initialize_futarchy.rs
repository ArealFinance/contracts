use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::FutarchyError;
use crate::events::FutarchyInitialized;
use crate::state::*;

#[derive(Accounts)]
pub struct InitializeFutarchy<'info> {
    #[account(mut, signer)]
    pub deployer: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    // OT governance PDA — proves OT is initialized and lets us verify deployer is the authority
    pub ot_governance: &'info AccountView,

    #[account(
        init, payer = deployer, space = 115,
        seeds = [b"futarchy_config", ot_mint.address().as_ref()], bump
    )]
    pub config: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(ctx: Context<InitializeFutarchy>) -> Result<()> {
    let ot_mint_ref = ctx.accounts.ot_mint.address().as_ref();

    // Validate ot_governance PDA derivation (must be real OT governance for this mint)
    let (expected_gov, _) = arlex_lang::find_program_address(
        &[b"ot_governance", ot_mint_ref],
        &Address::new_from_array(OT_PROGRAM_ID),
    );
    if ctx.accounts.ot_governance.address() != &expected_gov {
        return Err(ProgramError::from(FutarchyError::InvalidOtGovernance));
    }

    // Read OT governance authority and verify deployer is the current OT authority.
    // OtGovernance layout (after 8-byte discriminator):
    //   [8..40]   ot_mint       (32)
    //   [40..72]  authority     (32)
    let gov_data = unsafe {
        core::slice::from_raw_parts(
            ctx.accounts.ot_governance.data_ptr(),
            ctx.accounts.ot_governance.data_len(),
        )
    };
    if gov_data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    let ot_authority = &gov_data[40..72];
    if ot_authority != ctx.accounts.deployer.address().as_ref() {
        return Err(ProgramError::from(FutarchyError::Unauthorized));
    }

    let (_, bump) = arlex_lang::find_program_address(
        &[b"futarchy_config", ot_mint_ref], ctx.program_id,
    );

    let config = FutarchyConfig::init(ctx.accounts.config, ctx.program_id)?;

    let mut ot_mint_bytes = [0u8; 32];
    ot_mint_bytes.copy_from_slice(ot_mint_ref);

    let mut authority_bytes = [0u8; 32];
    authority_bytes.copy_from_slice(ctx.accounts.deployer.address().as_ref());

    config.ot_mint = ot_mint_bytes;
    config.authority = authority_bytes;
    config.pending_authority = [0u8; 32];
    config.has_pending = false;
    config.next_proposal_id = 0;
    config.is_active = true;
    config.bump = bump;

    let clock = Clock::get()?;
    emit!(FutarchyInitialized {
        ot_mint: ot_mint_bytes,
        authority: authority_bytes,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Futarchy initialized");
    Ok(())
}
