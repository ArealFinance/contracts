//! close_distributor — retire a distributor and sweep remaining RWT.
//!
//! Exempt from `config.is_active`: the authority may close even while the system
//! is globally paused (emergency exit). Sets `distributor.is_active = false`.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::YdError;
use crate::events::DistributorClosed;
#[allow(unused_imports)]
use crate::state::DistributionConfig;
use crate::state::MerkleDistributor;
use crate::validation::{read_token_account_amount, read_token_account_mint};

#[derive(Accounts)]
pub struct CloseDistributor<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    // Config loaded for authority validation only. `is_active` is intentionally NOT
    // checked here — close_distributor is an emergency-exit path that must work even
    // when the protocol is paused.
    #[account(
        has_one = authority, account_type = "DistributionConfig",
        seeds = [b"dist_config"], bump
    )]
    pub config: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    // NOTE: account_type requires has_one (Arlex constraint). Discriminator checked by load_mut.
    #[account(
        mut,
        seeds = [b"merkle_dist", ot_mint.address().as_ref()], bump
    )]
    pub distributor: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub reward_vault: &'info AccountView,

    // Typically ARL Treasury RWT ATA.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub unclaimed_destination: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(ctx: Context<CloseDistributor>) -> Result<()> {
    // CHECKS + EFFECTS in a scope that drops the MerkleDistributor guard before
    // the Transfer CPI below (the distributor PDA is the Transfer authority).
    // Copy out the seed material and the swept amount; the guard's Drop releases
    // the borrow flag at the end of this block. (Pattern C/D.)
    let (remaining, ot_mint_bytes, dist_bump);
    {
        let mut dist = MerkleDistributor::load_mut(ctx.accounts.distributor, ctx.program_id)?;
        if !dist.is_active {
            return Err(ProgramError::from(YdError::DistributorNotActive));
        }
        if dist.ot_mint != ctx.accounts.ot_mint.address().as_ref() {
            return Err(ProgramError::from(YdError::InvalidOtMint));
        }
        if ctx.accounts.reward_vault.address().as_ref() != dist.reward_vault.as_ref() {
            return Err(ProgramError::from(YdError::InvalidRewardVault));
        }

        // Both token accounts must hold RWT.
        let vault_mint = read_token_account_mint(ctx.accounts.reward_vault)?;
        let dest_mint = read_token_account_mint(ctx.accounts.unclaimed_destination)?;
        if vault_mint != RWT_MINT || dest_mint != RWT_MINT {
            return Err(ProgramError::from(YdError::InvalidTokenAccount));
        }

        remaining = read_token_account_amount(ctx.accounts.reward_vault)?;
        ot_mint_bytes = dist.ot_mint;
        dist_bump = dist.bump;

        // Mark inactive BEFORE CPI (CEI).
        dist.is_active = false;
    } // MerkleDistributor guard dropped — flag released before the Transfer CPI.

    if remaining > 0 {
        let bump_arr = [dist_bump];
        let seeds = [
            Seed::from(b"merkle_dist" as &[u8]),
            Seed::from(ot_mint_bytes.as_ref()),
            Seed::from(bump_arr.as_ref()),
        ];
        let signer = Signer::from(&seeds);

        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.reward_vault,
            to: ctx.accounts.unclaimed_destination,
            authority: ctx.accounts.distributor,
            amount: remaining,
        }
        .invoke_signed(&[signer])?;
    }

    let clock = Clock::get()?;
    emit!(DistributorClosed {
        ot_mint: ot_mint_bytes,
        unclaimed_swept: remaining,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
