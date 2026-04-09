use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::OtError;
use crate::events::TreasurySpent;
use crate::state::*;

#[derive(Accounts)]
pub struct SpendTreasury<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    // OT mint — for PDA seed derivation, validated as SPL Mint
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    // OtGovernance PDA — validated via seeds + has_one
    #[account(
        has_one = authority, account_type = "OtGovernance",
        seeds = [b"ot_governance", ot_mint.address().as_ref()], bump
    )]
    pub ot_governance: &'info AccountView,

    // OtTreasury PDA — validated via seeds
    #[account(seeds = [b"ot_treasury", ot_mint.address().as_ref()], bump)]
    pub ot_treasury: &'info AccountView,

    // Source: Treasury's token account (classic SPL Token only — no Token-2022)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub treasury_token_account: &'info AccountView,

    // Destination token account (must exist before calling — no init_if_needed)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub destination_token_account: &'info AccountView,

    // Token mint — for validation
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_mint: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(ctx: Context<SpendTreasury>, amount: u64) -> Result<()> {
    if amount == 0 {
        return Err(ProgramError::from(OtError::ZeroAmount));
    }

    // Check is_active (governance PDA already validated via seeds)
    let governance = OtGovernance::load(ctx.accounts.ot_governance, ctx.program_id)?;
    if !governance.is_active {
        return Err(ProgramError::from(OtError::GovernanceInactive));
    }

    // Load treasury (PDA already validated via seeds)
    let treasury = OtTreasury::load(ctx.accounts.ot_treasury, ctx.program_id)?;

    // Validate treasury_token_account: mint (0..32) and owner (32..64)
    let tta_data = unsafe {
        core::slice::from_raw_parts(
            ctx.accounts.treasury_token_account.data_ptr(),
            ctx.accounts.treasury_token_account.data_len(),
        )
    };
    if tta_data.len() < 64 {
        return Err(ProgramError::InvalidAccountData);
    }
    if &tta_data[0..32] != ctx.accounts.token_mint.address().as_ref() {
        return Err(ProgramError::from(OtError::TokenMintMismatch));
    }
    if &tta_data[32..64] != ctx.accounts.ot_treasury.address().as_ref() {
        return Err(ProgramError::from(OtError::InvalidTokenAccountOwner));
    }

    // Validate destination_token_account mint matches token_mint
    let dta_data = unsafe {
        core::slice::from_raw_parts(
            ctx.accounts.destination_token_account.data_ptr(),
            ctx.accounts.destination_token_account.data_len(),
        )
    };
    if dta_data.len() < 32 {
        return Err(ProgramError::InvalidAccountData);
    }
    if &dta_data[0..32] != ctx.accounts.token_mint.address().as_ref() {
        return Err(ProgramError::from(OtError::TokenMintMismatch));
    }

    // Transfer via OtTreasury PDA signer
    let bump_bytes = [treasury.bump];
    let seeds = [
        Seed::from(b"ot_treasury" as &[u8]),
        Seed::from(treasury.ot_mint.as_ref()),
        Seed::from(bump_bytes.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.treasury_token_account,
        to: ctx.accounts.destination_token_account,
        authority: ctx.accounts.ot_treasury,
        amount,
    }.invoke_signed(&[signer])?;

    // Emit event
    let clock = Clock::get()?;
    emit!(TreasurySpent {
        ot_mint: treasury.ot_mint,
        token_mint: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.token_mint.address().as_ref());
            arr
        },
        amount,
        destination: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.destination_token_account.address().as_ref());
            arr
        },
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Treasury spent");
    Ok(())
}
