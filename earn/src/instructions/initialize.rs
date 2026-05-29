//! `initialize` — bootstrap the `earn` program. One-time, deployer-only.
//!
//! Creates the singleton `EarnConfig` PDA at seed `["earn_config"]`, pins the
//! authority, pause authority, the `basket_vault` USDC ATA (EarnConfig-PDA-
//! owned), the `dao_fee_destination` USDC ATA, and the earn-RWT mint (whose
//! mint authority MUST already be the EarnConfig PDA).
//!
//! Initial NAV is implicit ($1.00 via the `total_rwt_supply == 0` guard) — no
//! separate NAV write needed; `total_invested_capital` starts at 0.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::EarnError;
use crate::events::EarnInitialized;
use crate::state::EarnConfig;
use crate::validation::read_token_account_mint;

#[derive(Accounts)]
pub struct Initialize<'info> {
    /// Deployer. Pays for the EarnConfig PDA rent.
    #[account(mut, signer)]
    pub deployer: &'info AccountView,

    /// EarnConfig PDA — created here (singleton).
    #[account(
        init, payer = deployer, space = EarnConfig::SPACE,
        seeds = [EARN_CONFIG_SEED], bump
    )]
    pub earn_config: &'info AccountView,

    /// Earn-RWT mint. Created off-chain via spl-token before this ix.
    /// Mint authority MUST be set to the EarnConfig PDA before init.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    /// USDC mint (validation only; pinned into config).
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub usdc_mint: &'info AccountView,

    /// USDC vault owned by the EarnConfig PDA — receives mint bodies + income.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub basket_vault: &'info AccountView,

    /// USDC ATA receiving the 1% commission (Areal revenue).
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub dao_fee_destination: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<Initialize>,
    authority: [u8; 32],
    pause_authority: [u8; 32],
) -> Result<()> {
    // --- Validate inputs ---
    if authority == [0u8; 32] {
        return Err(ProgramError::from(EarnError::ZeroDestination));
    }
    if pause_authority == [0u8; 32] {
        return Err(ProgramError::from(EarnError::InvalidPauseAuthority));
    }

    let mut usdc_mint = [0u8; 32];
    usdc_mint.copy_from_slice(ctx.accounts.usdc_mint.address().as_ref());

    // basket_vault and dao_fee_destination must both hold USDC.
    let basket_mint = read_token_account_mint(ctx.accounts.basket_vault)?;
    if basket_mint != usdc_mint {
        return Err(ProgramError::from(EarnError::InvalidTokenAccount));
    }
    let dao_fee_mint = read_token_account_mint(ctx.accounts.dao_fee_destination)?;
    if dao_fee_mint != usdc_mint {
        return Err(ProgramError::from(EarnError::InvalidFeeDestination));
    }

    let mut dao_fee_destination = [0u8; 32];
    dao_fee_destination.copy_from_slice(ctx.accounts.dao_fee_destination.address().as_ref());
    if dao_fee_destination == [0u8; 32] {
        return Err(ProgramError::from(EarnError::InvalidFeeDestination));
    }

    // --- Compute canonical bump ---
    let (_, config_bump) = arlex_lang::find_program_address(
        &[EARN_CONFIG_SEED], ctx.program_id,
    );

    // --- Initialize EarnConfig ---
    let config = EarnConfig::init(ctx.accounts.earn_config, ctx.program_id)?;
    config.total_invested_capital = 0; // supply == 0 → NAV implicitly $1.00
    config.authority = authority;
    config.pending_authority = [0u8; 32];
    config.has_pending = false;
    config.pause_authority = pause_authority;
    config.is_paused = false;
    config.mint_fee_bps = DEFAULT_MINT_FEE_BPS;
    config.basket_vault.copy_from_slice(ctx.accounts.basket_vault.address().as_ref());
    config.dao_fee_destination = dao_fee_destination;
    config.rwt_mint.copy_from_slice(ctx.accounts.rwt_mint.address().as_ref());
    config.usdc_mint = usdc_mint;
    config.min_mint_amount = MIN_MINT_AMOUNT;
    config.bump = config_bump;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(EarnInitialized {
        authority,
        rwt_mint: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.rwt_mint.address().as_ref());
            arr
        },
        basket_vault: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.basket_vault.address().as_ref());
            arr
        },
        dao_fee_destination,
        initial_nav: INITIAL_NAV,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Earn config initialized");
    Ok(())
}
