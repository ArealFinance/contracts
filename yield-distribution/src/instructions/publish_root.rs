//! publish_root — publish authority (server wallet) publishes a new merkle root.
//!
//! Enforces `max_total_claim > 0`, `== total_funded`, `>= total_claimed` to
//! prevent inflation, division-by-zero, or rewinding below already-claimed.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::YdError;
use crate::events::RootPublished;
use crate::state::{DistributionConfig, MerkleDistributor};

#[derive(Accounts)]
pub struct PublishRoot<'info> {
    #[account(signer)]
    pub publish_authority: &'info AccountView,

    #[account(seeds = [b"dist_config"], bump)]
    pub config: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    // NOTE: account_type requires has_one (Arlex constraint). Discriminator checked by load_mut.
    #[account(
        mut,
        seeds = [b"merkle_dist", ot_mint.address().as_ref()], bump
    )]
    pub distributor: &'info AccountView,
}

pub fn handler(
    ctx: Context<PublishRoot>,
    merkle_root: [u8; 32],
    max_total_claim: u64,
) -> Result<()> {
    let config = DistributionConfig::load(ctx.accounts.config, ctx.program_id)?;
    if !config.is_active {
        return Err(ProgramError::from(YdError::SystemPaused));
    }

    // Publish authority check
    if ctx.accounts.publish_authority.address().as_ref() != config.publish_authority.as_ref() {
        return Err(ProgramError::from(YdError::UnauthorizedPublisher));
    }

    let dist = MerkleDistributor::load_mut(ctx.accounts.distributor, ctx.program_id)?;
    if !dist.is_active {
        return Err(ProgramError::from(YdError::DistributorNotActive));
    }
    if dist.ot_mint != ctx.accounts.ot_mint.address().as_ref() {
        return Err(ProgramError::from(YdError::InvalidOtMint));
    }

    // --- Validation order matters ---
    if max_total_claim == 0 {
        return Err(ProgramError::from(YdError::ZeroMaxClaim));
    }
    if max_total_claim != dist.total_funded {
        return Err(ProgramError::from(YdError::InvalidMaxClaim));
    }
    if max_total_claim < dist.total_claimed {
        return Err(ProgramError::from(YdError::MaxClaimBelowClaimed));
    }

    // --- Apply ---
    dist.merkle_root = merkle_root;
    dist.max_total_claim = max_total_claim;
    dist.epoch = dist
        .epoch
        .checked_add(1)
        .ok_or(ProgramError::from(YdError::MathOverflow))?;

    let epoch = dist.epoch;
    let ot_mint_bytes = dist.ot_mint;

    let clock = Clock::get()?;
    emit!(RootPublished {
        ot_mint: ot_mint_bytes,
        epoch,
        merkle_root,
        max_total_claim,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
