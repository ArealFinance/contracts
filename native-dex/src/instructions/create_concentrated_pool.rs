//! Create a concentrated liquidity pool with BinArray.
//!
//! Shares ~77% of its flow with `create_pool` — validation (whitelist, RWT
//! pairing, canonical mint order), pool PDA + vault init, and OT treasury
//! detection all live in `pool_creation.rs` (L-7 audit fix). This handler
//! keeps only the concentrated-specific additions:
//! - `bin_step_bps` / `initial_active_bin` argument validation
//! - BinArray PDA creation
//! - `bin_step_bps` and `active_bin_id` fields on PoolState
//! - BinArray zero-init

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
pub struct CreateConcentratedPool<'info> {
    #[account(mut, signer)]
    pub creator: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    #[account(seeds = [b"pool_creators"], bump)]
    pub pool_creators: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,

    // BinArray PDA: ["bins", pool_state]
    #[account(mut)]
    pub bin_array: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_a_mint: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_b_mint: &'info AccountView,

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

pub fn handler(
    ctx: Context<CreateConcentratedPool>,
    bin_step_bps: u16,
    initial_active_bin: i32,
) -> Result<()> {
    let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;
    if !config.is_active {
        return Err(ProgramError::from(DexError::DexPaused));
    }

    // --- Concentrated-specific argument validation ---
    if bin_step_bps == 0 || bin_step_bps > MAX_BIN_STEP_BPS {
        return Err(ProgramError::from(DexError::InvalidBinStep));
    }
    if initial_active_bin.abs() > MAX_INITIAL_ACTIVE_BIN {
        return Err(ProgramError::from(DexError::InvalidBinRange));
    }

    // --- Shared setup (pool_creation.rs — L-7) ---
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

    // --- Create BinArray PDA (concentrated-specific) ---
    let pool_key = pubkey_bytes(ctx.accounts.pool_state);
    let (bin_pda, bin_bump) = arlex_lang::find_program_address(
        &[b"bins", pool_key.as_ref()],
        ctx.program_id,
    );
    if ctx.accounts.bin_array.address().as_ref() != bin_pda.as_ref() {
        return Err(ProgramError::InvalidSeeds);
    }

    let bin_lamports = rent.minimum_balance(BinArray::SPACE);
    arlex_lang::system::instructions::CreateAccount {
        from: ctx.accounts.creator,
        to: ctx.accounts.bin_array,
        lamports: bin_lamports,
        space: BinArray::SPACE as u64,
        owner: ctx.program_id,
    }
    .invoke_signed(&[Signer::from(&[
        Seed::from(b"bins" as &[u8]),
        Seed::from(pool_key.as_ref()),
        Seed::from(&[bin_bump]),
    ])])?;

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

    // --- Concentrated-specific PoolState init ---
    let lower_bin_id = initial_active_bin - (MAX_BINS as i32 / 2);

    let pool = PoolState::init(ctx.accounts.pool_state, ctx.program_id)?;
    pool.pool_type = POOL_TYPE_CONCENTRATED;
    pool.token_a_mint = mint_a;
    pool.token_b_mint = mint_b;
    pool.vault_a = pubkey_bytes(ctx.accounts.vault_a);
    pool.vault_b = pubkey_bytes(ctx.accounts.vault_b);
    pool.reserve_a = 0;
    pool.reserve_b = 0;
    pool.total_lp_shares = 0;
    pool.fee_bps = config.base_fee_bps;
    pool.is_active = true;
    pool.total_fees_accumulated = 0;
    pool.bin_step_bps = bin_step_bps;
    pool.active_bin_id = initial_active_bin;
    pool.ot_treasury_fee_destination = ot_treasury_fee_destination;
    pool.has_ot_treasury = has_ot_treasury;
    pool.bump = pool_bump;

    // --- Initialize BinArray (concentrated-specific) ---
    let bins = BinArray::init(ctx.accounts.bin_array, ctx.program_id)?;
    bins.pool = pool_key;
    for i in 0..MAX_BINS {
        bins.bins[i] = Bin { liquidity_a: 0, liquidity_b: 0 };
    }
    bins.lower_bin_id = lower_bin_id;
    bins.bin_step_bps = bin_step_bps;
    bins.active_bin_id = initial_active_bin;
    bins.bump = bin_bump;

    let clock = Clock::get()?;
    emit!(PoolCreated {
        pool: pool_key,
        token_a_mint: mint_a,
        token_b_mint: mint_b,
        pool_type: POOL_TYPE_CONCENTRATED,
        creator: creator_key,
        ot_treasury_fee_destination,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Concentrated pool created");
    Ok(())
}
