//! initialize_liquidity_holding — create the singleton LiquidityHolding PDA
//! and its RWT ATA (per D11.1).
//!
//! Permissionless, idempotent-ish: re-running after a successful init reverts
//! with `LiquidityHoldingAlreadyInitialized`. The ATA creation is also
//! idempotent at the SPL ATA program level (Create returns Ok if the account
//! already exists with the correct mint/owner).
//!
//! Layer 8 §2.1 + decisions D4 / D11.1. The PDA is the SPL ATA owner; only
//! `withdraw_liquidity_holding` (Layer 9 Nexus) can move funds out — anti-
//! honeypot guarantee. `claim_yield` in RWT Engine validates that
//! `liquidity_dest == dist_config.liquidity_destination` (singleton equality)
//! so this ATA address must be pinned in `RwtDistributionConfig` after init
//! via RWT Engine's `update_distribution_config`.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::YdError;
use crate::events::LiquidityHoldingInitialized;
use crate::state::LiquidityHolding;

#[derive(Accounts)]
pub struct InitializeLiquidityHolding<'info> {
    /// Permissionless payer — covers PDA + ATA rent. Anyone may call.
    #[account(mut, signer)]
    pub payer: &'info AccountView,

    #[account(
        init, payer = payer, space = LiquidityHolding::SPACE,
        seeds = [b"liq_holding"], bump
    )]
    pub liquidity_holding: &'info AccountView,

    // RWT mint pinned (deployment-time). Same constant used by all YD ix that
    // touch RWT (create_distributor, close_distributor).
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    // RWT ATA owned by `liquidity_holding` PDA. Created via the SPL ATA program
    // CPI below (idempotent at the ATA-program level).
    #[account(mut)]
    pub liquidity_holding_ata: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,

    #[account(constraint = ata_program.address() == &Address::new_from_array(ASSOCIATED_TOKEN_PROGRAM))]
    pub ata_program: &'info AccountView,
}

pub fn handler(ctx: Context<InitializeLiquidityHolding>) -> Result<()> {
    // --- Mint pinning (refuse foreign-mint deployments) ---
    if ctx.accounts.rwt_mint.address().as_ref() != RWT_MINT.as_ref() {
        return Err(ProgramError::from(YdError::InvalidTokenAccount));
    }

    // --- Derive canonical bump ---
    let (_, holding_bump) =
        arlex_lang::find_program_address(&[b"liq_holding"], ctx.program_id);

    // --- Initialize PDA ---
    //
    // `init` Arlex constraint above guarantees this account is freshly
    // allocated; the `initialized` flag below is a defense-in-depth guard
    // so that any future replay path is caught explicitly.
    let holding = LiquidityHolding::init(ctx.accounts.liquidity_holding, ctx.program_id)?;
    if holding.initialized {
        return Err(ProgramError::from(YdError::LiquidityHoldingAlreadyInitialized));
    }
    holding.bump = holding_bump;
    holding.initialized = true;
    holding.total_received = 0;
    holding.total_withdrawn = 0;
    holding.last_funded_slot = 0;
    // Layer 9 R20 — zero-init the per-drain tracking slots carved out of the
    // Layer 8 32-byte `_reserved` block. SPACE / data layout unchanged.
    holding.last_withdrawn_slot = 0;
    holding.last_withdrawn_amount = 0;
    holding._reserved = [0u8; 16];

    // --- Create RWT ATA (wallet = liquidity_holding PDA) ---
    //
    // ATA program rejects creation when the account already exists with a
    // different mint/owner; matching configurations are no-ops. Safe to call
    // unconditionally.
    arlex_lang::associated_token::instructions::Create {
        funding_account: ctx.accounts.payer,
        account: ctx.accounts.liquidity_holding_ata,
        wallet: ctx.accounts.liquidity_holding,
        mint: ctx.accounts.rwt_mint,
        system_program: ctx.accounts.system_program,
        token_program: ctx.accounts.token_program,
    }
    .invoke()?;

    // --- Emit ---
    let now = Clock::get()?.unix_timestamp;
    let liquidity_holding_addr = {
        let mut a = [0u8; 32];
        a.copy_from_slice(ctx.accounts.liquidity_holding.address().as_ref());
        a
    };
    let liquidity_holding_ata_addr = {
        let mut a = [0u8; 32];
        a.copy_from_slice(ctx.accounts.liquidity_holding_ata.address().as_ref());
        a
    };
    let payer_addr = {
        let mut a = [0u8; 32];
        a.copy_from_slice(ctx.accounts.payer.address().as_ref());
        a
    };
    emit!(LiquidityHoldingInitialized {
        liquidity_holding: liquidity_holding_addr,
        liquidity_holding_ata: liquidity_holding_ata_addr,
        payer: payer_addr,
        timestamp: now,
    });

    arlex_lang::log("YD liquidity_holding initialized");
    Ok(())
}
