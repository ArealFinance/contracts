use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::PoolCreated;
use crate::pool_creation::{
    create_pool_account, detect_ot_treasury, init_vault_pair, require_valid_mint_pair,
    require_whitelisted_creator,
};
use crate::state::*;
use crate::validation::pubkey_bytes;

#[derive(Accounts)]
pub struct CreatePool<'info> {
    #[account(mut, signer)]
    pub creator: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    #[account(seeds = [b"pool_creators"], bump)]
    pub pool_creators: &'info AccountView,

    // Pool PDA: ["pool", token_a_mint, token_b_mint]
    #[account(mut)]
    pub pool_state: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_a_mint: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_b_mint: &'info AccountView,

    // Vault keypair accounts (created via CPI)
    #[account(mut, signer)]
    pub vault_a: &'info AccountView,

    #[account(mut, signer)]
    pub vault_b: &'info AccountView,

    // OT treasury PDA + RWT ATA for OT pairs — passed as remaining_accounts[0..2]

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(ctx: Context<CreatePool>) -> Result<()> {
    let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;
    if !config.is_active {
        return Err(ProgramError::from(DexError::DexPaused));
    }

    // Shared setup (see pool_creation.rs — L-7)
    let creator_key = require_whitelisted_creator(
        ctx.accounts.pool_creators,
        ctx.accounts.creator,
        ctx.program_id,
    )?;
    let (mint_a, mint_b) =
        require_valid_mint_pair(ctx.accounts.token_a_mint, ctx.accounts.token_b_mint)?;

    let rent = pinocchio::sysvars::rent::Rent::get()?;
    let (pool_pda, pool_bump) = create_pool_account(
        ctx.accounts.creator,
        ctx.accounts.pool_state,
        &mint_a,
        &mint_b,
        ctx.program_id,
        &rent,
    )?;

    init_vault_pair(
        ctx.accounts.creator,
        ctx.accounts.vault_a,
        ctx.accounts.vault_b,
        ctx.accounts.token_a_mint,
        ctx.accounts.token_b_mint,
        &pool_pda,
        &rent,
    )?;

    let (ot_treasury_fee_destination, has_ot_treasury) =
        detect_ot_treasury(ctx.remaining_accounts, &mint_a, &mint_b)?;

    // --- StandardCurve-specific PoolState init ---
    let pool = PoolState::init(ctx.accounts.pool_state, ctx.program_id)?;
    pool.pool_type = POOL_TYPE_STANDARD;
    pool.token_a_mint = mint_a;
    pool.token_b_mint = mint_b;
    pool.vault_a = pubkey_bytes(ctx.accounts.vault_a);
    pool.vault_b = pubkey_bytes(ctx.accounts.vault_b);
    pool.reserve_a = 0;
    pool.reserve_b = 0;
    pool.total_lp_shares = 0;
    pool.fee_bps = config.base_fee_bps; // immutable after creation
    pool.is_active = true;
    pool.total_fees_accumulated = 0;
    pool.bin_step_bps = 0; // StandardCurve
    pool.active_bin_id = 0; // StandardCurve
    pool.ot_treasury_fee_destination = ot_treasury_fee_destination;
    pool.has_ot_treasury = has_ot_treasury;
    pool.bump = pool_bump;

    let clock = Clock::get()?;
    emit!(PoolCreated {
        pool: pubkey_bytes(ctx.accounts.pool_state),
        token_a_mint: mint_a,
        token_b_mint: mint_b,
        pool_type: POOL_TYPE_STANDARD,
        creator: creator_key,
        ot_treasury_fee_destination,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Pool created");
    Ok(())
}
