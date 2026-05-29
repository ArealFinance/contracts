//! `initialize` — one-time bootstrap of the `staking` program.
//!
//! Creates the singleton `StakingConfig` PDA at seed `["staking_config"]`,
//! initializes the stRWT mint (mint authority = StakingConfig PDA), and
//! creates the RWT pool vault (an RWT ATA owned by the StakingConfig PDA).
//!
//! Per staking.mdx §"initialize constraints":
//!   - `rwt_mint` MUST be the canonical earn-RWT mint. This crate is
//!     deliberately decoupled from `earn` (no CPI), so V1 validates the mint
//!     is a real, non-zero SPL mint and pins it into config. A cross-program
//!     read against EarnConfig.rwt_mint is a TODO once the earn program ID /
//!     PDA is finalized (Phase 4 vanity grind) — see TODO(earn-pin) below.
//!   - `strwt_mint` created with mint authority = StakingConfig PDA.
//!   - `pool_vault` created as an RWT ATA owned by the StakingConfig PDA.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, rent::Rent, Sysvar};

use crate::constants::*;
use crate::error::StakingError;
use crate::events::StakingInitialized;
use crate::state::StakingConfig;

#[derive(Accounts)]
pub struct Initialize<'info> {
    /// Deployer / authority. Pays rent for the StakingConfig PDA + strwt_mint.
    #[account(mut, signer)]
    pub authority: &'info AccountView,

    /// StakingConfig PDA — created here (singleton).
    #[account(
        init, payer = authority, space = StakingConfig::SPACE,
        seeds = [STAKING_CONFIG_SEED], bump
    )]
    pub staking_config: &'info AccountView,

    /// Staked token: the earn-RWT mint. Read-only; pinned into config.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    /// stRWT share mint — created here as a deployer-signed keypair, then
    /// InitializeMint2 sets the mint authority to the StakingConfig PDA.
    #[account(mut, signer)]
    pub strwt_mint: &'info AccountView,

    /// RWT pool vault — RWT ATA owned by the StakingConfig PDA. Created via
    /// the Associated Token Program CPI below.
    #[account(mut)]
    pub pool_vault: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,

    #[account(constraint = ata_program.address() == &Address::new_from_array(ASSOCIATED_TOKEN_PROGRAM))]
    pub ata_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<Initialize>,
    pause_authority: [u8; 32],
    reward_depositor: [u8; 32],
) -> Result<()> {
    // --- Validate inputs (no zero addresses for any role) ---
    if pause_authority == [0u8; 32] || reward_depositor == [0u8; 32] {
        return Err(ProgramError::from(StakingError::ZeroAddress));
    }

    // rwt_mint must be a real, non-zero SPL mint. TODO(earn-pin): once the
    // earn program ID is final, additionally CPI-read EarnConfig.rwt_mint and
    // require equality (InvalidRwtMint otherwise).
    let mut rwt_mint = [0u8; 32];
    rwt_mint.copy_from_slice(ctx.accounts.rwt_mint.address().as_ref());
    if rwt_mint == [0u8; 32] {
        return Err(ProgramError::from(StakingError::InvalidRwtMint));
    }

    // --- Canonical config bump ---
    let (config_address, config_bump) =
        arlex_lang::find_program_address(&[STAKING_CONFIG_SEED], ctx.program_id);

    // --- Create the stRWT mint (82 bytes, SPL Token owner) ---
    let rent = Rent::get()?;
    let mint_lamports = rent.try_minimum_balance(82)?;
    arlex_lang::system::instructions::CreateAccount {
        from: ctx.accounts.authority,
        to: ctx.accounts.strwt_mint,
        lamports: mint_lamports,
        space: 82,
        owner: &Address::new_from_array(SPL_TOKEN_PROGRAM),
    }
    .invoke()?;

    // Mint authority = StakingConfig PDA; no freeze authority.
    arlex_lang::token::instructions::InitializeMint2 {
        mint: ctx.accounts.strwt_mint,
        decimals: STRWT_DECIMALS,
        mint_authority: &config_address,
        freeze_authority: None,
    }
    .invoke()?;

    // --- Create the RWT pool vault ATA (owned by StakingConfig PDA) ---
    arlex_lang::associated_token::instructions::Create {
        funding_account: ctx.accounts.authority,
        account: ctx.accounts.pool_vault,
        wallet: ctx.accounts.staking_config,
        mint: ctx.accounts.rwt_mint,
        system_program: ctx.accounts.system_program,
        token_program: ctx.accounts.token_program,
    }
    .invoke()?;

    // --- Initialize StakingConfig ---
    let config = StakingConfig::init(ctx.accounts.staking_config, ctx.program_id)?;
    config.authority.copy_from_slice(ctx.accounts.authority.address().as_ref());
    config.pending_authority = [0u8; 32];
    config.has_pending = false;
    config.pause_authority = pause_authority;
    config.is_paused = false;
    config.rwt_mint = rwt_mint;
    config.strwt_mint.copy_from_slice(ctx.accounts.strwt_mint.address().as_ref());
    config.reward_depositor = reward_depositor;
    config.pool_vault.copy_from_slice(ctx.accounts.pool_vault.address().as_ref());
    config.total_rwt_active = 0;
    config.total_rwt_reserved = 0;
    config.cooldown_seconds = COOLDOWN_SECONDS;
    config.min_stake_amount = MIN_STAKE_AMOUNT;
    config.bump = config_bump;

    // --- Emit event ---
    let clock = Clock::get()?;
    let mut authority_arr = [0u8; 32];
    authority_arr.copy_from_slice(ctx.accounts.authority.address().as_ref());
    let mut strwt_mint_arr = [0u8; 32];
    strwt_mint_arr.copy_from_slice(ctx.accounts.strwt_mint.address().as_ref());
    let mut pool_vault_arr = [0u8; 32];
    pool_vault_arr.copy_from_slice(ctx.accounts.pool_vault.address().as_ref());

    emit!(StakingInitialized {
        authority: authority_arr,
        rwt_mint,
        strwt_mint: strwt_mint_arr,
        pool_vault: pool_vault_arr,
        cooldown_seconds: config.cooldown_seconds,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Staking initialized");
    Ok(())
}
