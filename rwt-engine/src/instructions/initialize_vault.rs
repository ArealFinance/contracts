use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::RwtError;
use crate::events::VaultInitialized;
use crate::state::*;

#[derive(Accounts)]
pub struct InitializeVault<'info> {
    #[account(mut, signer)]
    pub deployer: &'info AccountView,

    // RWT Vault PDA (singleton)
    #[account(
        init, payer = deployer, space = RwtVault::SPACE,
        seeds = [b"rwt_vault"], bump
    )]
    pub rwt_vault: &'info AccountView,

    // Distribution config PDA (singleton)
    #[account(
        init, payer = deployer, space = RwtDistributionConfig::SPACE,
        seeds = [b"dist_config_rwt"], bump
    )]
    pub dist_config: &'info AccountView,

    // RWT SPL Mint — created here. 82 bytes, owned by SPL Token Program.
    // Deployer creates as signer keypair, then InitializeMint sets authority to vault PDA.
    #[account(mut, signer)]
    pub rwt_mint: &'info AccountView,

    // USDC mint — for creating Capital Accumulator ATA
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub usdc_mint: &'info AccountView,

    // Capital Accumulator USDC ATA — created via CPI
    #[account(mut)]
    pub capital_accumulator_ata: &'info AccountView,

    // Areal fee destination — validated as real SPL Token Account
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub areal_fee_destination_account: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,

    #[account(constraint = ata_program.address() == &Address::new_from_array(ASSOCIATED_TOKEN_PROGRAM))]
    pub ata_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<InitializeVault>,
    initial_authority: [u8; 32],
    pause_authority: [u8; 32],
    liquidity_destination: [u8; 32],
    protocol_revenue_destination: [u8; 32],
) -> Result<()> {
    // --- Validate inputs (no zero addresses for any role or destination) ---
    if initial_authority == [0u8; 32] {
        return Err(ProgramError::from(RwtError::ZeroDestination));
    }
    if pause_authority == [0u8; 32] {
        return Err(ProgramError::from(RwtError::InvalidPauseAuthority));
    }
    if liquidity_destination == [0u8; 32] || protocol_revenue_destination == [0u8; 32] {
        return Err(ProgramError::from(RwtError::ZeroDestination));
    }

    let mut areal_fee_destination = [0u8; 32];
    areal_fee_destination.copy_from_slice(
        ctx.accounts.areal_fee_destination_account.address().as_ref(),
    );
    if areal_fee_destination == [0u8; 32] {
        return Err(ProgramError::from(RwtError::InvalidFeeDestination));
    }

    // --- Compute canonical bumps ---
    let (_, vault_bump) = arlex_lang::find_program_address(
        &[b"rwt_vault"], ctx.program_id,
    );
    let (_, dist_config_bump) = arlex_lang::find_program_address(
        &[b"dist_config_rwt"], ctx.program_id,
    );

    // --- Create RWT Mint (82 bytes, SPL Token owner) ---
    // Allocate the account via System Program CPI (rwt_mint is signer keypair)
    let rent = pinocchio::sysvars::rent::Rent::get()?;
    let lamports = rent.try_minimum_balance(82)?;
    arlex_lang::system::instructions::CreateAccount {
        from: ctx.accounts.deployer,
        to: ctx.accounts.rwt_mint,
        lamports,
        space: 82,
        owner: &Address::new_from_array(SPL_TOKEN_PROGRAM),
    }.invoke()?;

    // Initialize the mint: decimals=6, authority=vault PDA, freeze=None
    // Use InitializeMint2 (no rent sysvar needed)
    let vault_address = arlex_lang::find_program_address(
        &[b"rwt_vault"], ctx.program_id,
    ).0;
    arlex_lang::token::instructions::InitializeMint2 {
        mint: ctx.accounts.rwt_mint,
        decimals: RWT_DECIMALS,
        mint_authority: &vault_address,
        freeze_authority: None,
    }.invoke()?;

    // --- Initialize RwtVault ---
    let vault = RwtVault::init(ctx.accounts.rwt_vault, ctx.program_id)?;
    vault.total_invested_capital = 0;
    vault.total_rwt_supply = 0;
    vault.nav_book_value = INITIAL_NAV;
    vault.capital_accumulator_ata = [0u8; 32]; // set after ATA creation
    vault.rwt_mint.copy_from_slice(ctx.accounts.rwt_mint.address().as_ref());
    vault.authority = initial_authority;
    vault.pending_authority = [0u8; 32];
    vault.has_pending = false;
    vault.manager = [0u8; 32]; // zeroed — set via update_vault_manager before Layer 6
    vault.pause_authority = pause_authority;
    vault.mint_paused = false;
    vault.areal_fee_destination = areal_fee_destination;
    vault.bump = vault_bump;

    // --- Create Capital Accumulator USDC ATA (owned by vault PDA) ---
    arlex_lang::associated_token::instructions::Create {
        funding_account: ctx.accounts.deployer,
        account: ctx.accounts.capital_accumulator_ata,
        wallet: ctx.accounts.rwt_vault,
        mint: ctx.accounts.usdc_mint,
        system_program: ctx.accounts.system_program,
        token_program: ctx.accounts.token_program,
    }.invoke()?;

    // Store the ATA address in vault state
    vault.capital_accumulator_ata.copy_from_slice(
        ctx.accounts.capital_accumulator_ata.address().as_ref(),
    );

    // --- Initialize RwtDistributionConfig ---
    let config = RwtDistributionConfig::init(ctx.accounts.dist_config, ctx.program_id)?;
    config.book_value_bps = DEFAULT_BOOK_VALUE_BPS;
    config.liquidity_bps = DEFAULT_LIQUIDITY_BPS;
    config.protocol_revenue_bps = DEFAULT_PROTOCOL_REVENUE_BPS;
    config.liquidity_destination = liquidity_destination;
    config.protocol_revenue_destination = protocol_revenue_destination;
    config.bump = dist_config_bump;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(VaultInitialized {
        authority: initial_authority,
        rwt_mint: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.rwt_mint.address().as_ref());
            arr
        },
        nav: INITIAL_NAV,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("RWT vault initialized");
    Ok(())
}
