use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::DexInitialized;
use crate::state::*;

#[derive(Accounts)]
pub struct InitializeDex<'info> {
    #[account(mut, signer)]
    pub deployer: &'info AccountView,

    #[account(
        init, payer = deployer, space = DexConfig::SPACE,
        seeds = [b"dex_config"], bump
    )]
    pub dex_config: &'info AccountView,

    #[account(
        init, payer = deployer, space = PoolCreators::SPACE,
        seeds = [b"pool_creators"], bump
    )]
    pub pool_creators: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<InitializeDex>,
    areal_fee_destination: [u8; 32],
    pause_authority: [u8; 32],
    rebalancer: [u8; 32],
) -> Result<()> {
    // --- Validate inputs ---
    if pause_authority == [0u8; 32] {
        return Err(ProgramError::from(DexError::InvalidPauseAuthority));
    }
    if areal_fee_destination == [0u8; 32] {
        return Err(ProgramError::from(DexError::InvalidFeeDestination));
    }

    // --- Compute bumps ---
    let (_, config_bump) = arlex_lang::find_program_address(
        &[b"dex_config"], ctx.program_id,
    );
    let (_, creators_bump) = arlex_lang::find_program_address(
        &[b"pool_creators"], ctx.program_id,
    );

    let deployer_key = crate::validation::pubkey_bytes(ctx.accounts.deployer);

    // --- Initialize DexConfig ---
    let config = DexConfig::init(ctx.accounts.dex_config, ctx.program_id)?;
    config.authority = deployer_key;
    config.pending_authority = [0u8; 32];
    config.has_pending = false;
    config.pause_authority = pause_authority;
    config.base_fee_bps = DEFAULT_BASE_FEE_BPS;
    config.lp_fee_share_bps = DEFAULT_LP_FEE_SHARE_BPS;
    config.areal_fee_destination = areal_fee_destination;
    config.rebalancer = rebalancer;
    config.is_active = true;
    config.bump = config_bump;

    // --- Initialize PoolCreators (deployer auto-added as first creator) ---
    let creators = PoolCreators::init(ctx.accounts.pool_creators, ctx.program_id)?;
    creators.authority = deployer_key;
    creators.creators = [[0u8; 32]; 10];
    creators.creators[0] = deployer_key;
    creators.active_count = 1;
    creators.bump = creators_bump;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(DexInitialized {
        authority: deployer_key,
        base_fee_bps: DEFAULT_BASE_FEE_BPS,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("DEX initialized");
    Ok(())
}
