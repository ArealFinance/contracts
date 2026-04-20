//! initialize_config — create the global DistributionConfig PDA (singleton).
//!
//! Called once per protocol deployment. Stores immutable `areal_fee_destination`
//! (RWT ATA); all other config fields can be rotated by the authority.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::YdError;
use crate::events::ConfigInitialized;
use crate::state::DistributionConfig;

#[derive(Accounts)]
pub struct InitializeConfig<'info> {
    #[account(mut, signer)]
    pub deployer: &'info AccountView,

    #[account(
        init, payer = deployer, space = DistributionConfig::SPACE,
        seeds = [b"dist_config"], bump
    )]
    pub config: &'info AccountView,

    // Areal fee destination — validated as a real SPL Token Account.
    // Its address is stored in config.areal_fee_destination and becomes immutable.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub areal_fee_destination_account: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<InitializeConfig>,
    publish_authority: [u8; 32],
    protocol_fee_bps: u16,
    min_distribution_amount: u64,
) -> Result<()> {
    // --- Validate inputs ---
    if publish_authority == [0u8; 32] {
        return Err(ProgramError::from(YdError::ZeroDestination));
    }
    // Sanity cap on init — runtime update_config keeps the "trust the authority"
    // stance per spec, but blatantly-wrong values at init are refused.
    if protocol_fee_bps as u64 > BPS_DENOMINATOR {
        return Err(ProgramError::from(YdError::InvalidFeeBps));
    }

    let mut areal_fee_destination = [0u8; 32];
    areal_fee_destination.copy_from_slice(
        ctx.accounts
            .areal_fee_destination_account
            .address()
            .as_ref(),
    );
    if areal_fee_destination == [0u8; 32] {
        return Err(ProgramError::from(YdError::ZeroDestination));
    }

    // --- Derive canonical bump ---
    let (_, config_bump) = arlex_lang::find_program_address(&[b"dist_config"], ctx.program_id);

    // --- Initialize config ---
    let config = DistributionConfig::init(ctx.accounts.config, ctx.program_id)?;
    config
        .authority
        .copy_from_slice(ctx.accounts.deployer.address().as_ref());
    config.pending_authority = [0u8; 32];
    config.has_pending = false;
    config.publish_authority = publish_authority;
    config.protocol_fee_bps = protocol_fee_bps;
    config.min_distribution_amount = min_distribution_amount;
    config.areal_fee_destination = areal_fee_destination;
    config.is_active = true;
    config.bump = config_bump;

    // --- Emit ---
    let clock = Clock::get()?;
    let authority = {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(ctx.accounts.deployer.address().as_ref());
        arr
    };
    emit!(ConfigInitialized {
        authority,
        publish_authority,
        protocol_fee_bps,
        areal_fee_destination,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("YD config initialized");
    Ok(())
}
