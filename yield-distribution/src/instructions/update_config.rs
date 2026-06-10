//! update_config — authority overwrites mutable config fields.
//!
//! `areal_fee_destination` is IMMUTABLE and is intentionally not a parameter.
//! `protocol_fee_bps` accepts any u16 per spec (authority is trusted).

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::events::ConfigUpdated;
use crate::state::DistributionConfig;

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "DistributionConfig",
        seeds = [b"dist_config"], bump
    )]
    pub config: &'info AccountView,
}

pub fn handler(
    ctx: Context<UpdateConfig>,
    protocol_fee_bps: u16,
    min_distribution_amount: u64,
    is_active: bool,
) -> Result<()> {
    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut config = DistributionConfig::load_mut(ctx.accounts.config, ctx.program_id)?;

    config.protocol_fee_bps = protocol_fee_bps;
    config.min_distribution_amount = min_distribution_amount;
    config.is_active = is_active;

    let clock = Clock::get()?;
    emit!(ConfigUpdated {
        protocol_fee_bps,
        min_distribution_amount,
        is_active,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
