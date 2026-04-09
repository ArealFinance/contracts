use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::OtError;
use crate::events::DestinationConfigUpdated;
use crate::state::*;

#[derive(Accounts)]
pub struct BatchUpdateDestinations<'info> {
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

    // RevenueConfig PDA — validated via seeds
    #[account(
        mut,
        seeds = [b"revenue_config", ot_mint.address().as_ref()], bump
    )]
    pub revenue_config: &'info AccountView,
}

pub fn handler(
    ctx: Context<BatchUpdateDestinations>,
    destinations: alloc::vec::Vec<BatchDestination>,
) -> Result<()> {
    // Check is_active (governance PDA already validated via seeds)
    let governance = OtGovernance::load(ctx.accounts.ot_governance, ctx.program_id)?;
    if !governance.is_active {
        return Err(ProgramError::from(OtError::GovernanceInactive));
    }

    // Validate destination count
    if destinations.is_empty() {
        return Err(ProgramError::from(OtError::EmptyDestinationList));
    }
    if destinations.len() > MAX_DESTINATIONS {
        return Err(ProgramError::from(OtError::TooManyDestinations));
    }

    // Load RevenueConfig (PDA already validated via seeds)
    let config = RevenueConfig::load_mut(ctx.accounts.revenue_config, ctx.program_id)?;

    let zero_address = [0u8; 32];

    // Validate each destination + compute total BPS
    let mut bps_total: u64 = 0;
    for i in 0..destinations.len() {
        let dest = &destinations[i];

        if dest.address == zero_address {
            return Err(ProgramError::from(OtError::ZeroDestinationAddress));
        }
        if dest.allocation_bps == 0 || dest.allocation_bps > BPS_DENOMINATOR as u16 {
            return Err(ProgramError::from(OtError::InvalidAllocationBps));
        }
        if dest.address == config.areal_fee_destination {
            return Err(ProgramError::from(OtError::FeeDestinationCollision));
        }
        for j in 0..i {
            if dest.address == destinations[j].address {
                return Err(ProgramError::from(OtError::DuplicateDestination));
            }
        }
        bps_total += dest.allocation_bps as u64;
    }

    if bps_total != BPS_DENOMINATOR {
        return Err(ProgramError::from(OtError::InvalidBpsTotal));
    }

    // Clear all 10 slots
    for i in 0..MAX_DESTINATIONS {
        config.destinations[i] = RevenueDestination::zeroed();
    }

    // Write new destinations
    for (i, dest) in destinations.iter().enumerate() {
        config.destinations[i] = RevenueDestination {
            address: dest.address,
            allocation_bps: dest.allocation_bps,
            label: dest.label,
        };
    }

    config.active_count = destinations.len() as u8;
    config.config_version = config.config_version
        .checked_add(1)
        .ok_or(ProgramError::from(OtError::MathOverflow))?;

    // Emit event
    let clock = Clock::get()?;
    emit!(DestinationConfigUpdated {
        ot_mint: config.ot_mint,
        config_version: config.config_version,
        active_count: config.active_count,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Destinations updated");
    Ok(())
}
