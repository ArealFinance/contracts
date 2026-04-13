use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::BPS_DENOMINATOR;
use crate::error::RwtError;
use crate::events::DistributionConfigUpdated;
use crate::state::{RwtVault, RwtDistributionConfig};

#[derive(Accounts)]
pub struct UpdateDistributionConfig<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        has_one = authority, account_type = "RwtVault",
        seeds = [b"rwt_vault"], bump
    )]
    pub rwt_vault: &'info AccountView,

    #[account(mut, seeds = [b"dist_config_rwt"], bump)]
    pub dist_config: &'info AccountView,
}

pub fn handler(
    ctx: Context<UpdateDistributionConfig>,
    book_value_bps: u16,
    liquidity_bps: u16,
    protocol_revenue_bps: u16,
    liquidity_destination: [u8; 32],
    protocol_revenue_destination: [u8; 32],
) -> Result<()> {
    // --- Validate destinations ---
    if liquidity_destination == [0u8; 32] || protocol_revenue_destination == [0u8; 32] {
        return Err(ProgramError::from(RwtError::ZeroDestination));
    }

    // --- Validate BPS sum ---
    let sum = (book_value_bps as u64)
        .checked_add(liquidity_bps as u64)
        .and_then(|s| s.checked_add(protocol_revenue_bps as u64))
        .ok_or(ProgramError::from(RwtError::MathOverflow))?;
    if sum != BPS_DENOMINATOR {
        return Err(ProgramError::from(RwtError::InvalidDistributionRatios));
    }

    // --- Effects ---
    let config = RwtDistributionConfig::load_mut(ctx.accounts.dist_config, ctx.program_id)?;
    config.book_value_bps = book_value_bps;
    config.liquidity_bps = liquidity_bps;
    config.protocol_revenue_bps = protocol_revenue_bps;
    config.liquidity_destination = liquidity_destination;
    config.protocol_revenue_destination = protocol_revenue_destination;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(DistributionConfigUpdated {
        book_value_bps,
        liquidity_bps,
        protocol_revenue_bps,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
