use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::LiquidityAdded;
use crate::state::{DexConfig, PoolState, LpPosition};
use crate::amm::calculate_lp_shares;
use crate::validation::*;

#[derive(Accounts)]
pub struct AddLiquidity<'info> {
    #[account(signer)]
    pub provider: &'info AccountView,

    #[account(mut, signer)]
    pub payer: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    // NOTE: pool_state PDA verified via PoolState::load_mut (checks discriminator + owner).
    // Full seed validation is done at create_pool time. Here we rely on program-ownership.
    #[account(mut)]
    pub pool_state: &'info AccountView,

    // LpPosition PDA: ["lp", pool_state, provider]
    #[account(mut)]
    pub lp_position: &'info AccountView,

    // Provider's token accounts
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub provider_token_a: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub provider_token_b: &'info AccountView,

    // Pool vaults
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_a: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_b: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<AddLiquidity>,
    amount_a: u64,
    amount_b: u64,
) -> Result<()> {
    let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;
    let pool = PoolState::load_mut(ctx.accounts.pool_state, ctx.program_id)?;

    // --- Checks ---
    if !config.is_active {
        return Err(ProgramError::from(DexError::DexPaused));
    }
    if !pool.is_active {
        return Err(ProgramError::from(DexError::PoolNotActive));
    }
    if amount_a == 0 || amount_b == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount));
    }

    // SECURITY: Validate vaults match pool state
    validate_vault(ctx.accounts.vault_a, &pool.vault_a)?;
    validate_vault(ctx.accounts.vault_b, &pool.vault_b)?;

    // --- Calculate LP shares ---
    let is_first = pool.total_lp_shares == 0;
    let (deposit_a, deposit_b, shares) = if is_first {
        // First LP: use both amounts directly
        let shares = calculate_lp_shares(amount_a, amount_b, 0, 0, 0, true)?;
        (amount_a, amount_b, shares)
    } else {
        // Subsequent LP: proportional deposit
        // Calculate max balanced deposit based on pool ratio
        // deposit_b_needed = amount_a * reserve_b / reserve_a
        let b_needed = arlex_lang::math::mul_div_u64(amount_a, pool.reserve_b, pool.reserve_a)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;

        let (dep_a, dep_b) = if b_needed <= amount_b {
            // amount_a is the limiting factor
            (amount_a, b_needed)
        } else {
            // amount_b is the limiting factor
            let a_needed = arlex_lang::math::mul_div_u64(amount_b, pool.reserve_a, pool.reserve_b)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            (a_needed, amount_b)
        };

        let shares = calculate_lp_shares(dep_a, dep_b, pool.reserve_a, pool.reserve_b, pool.total_lp_shares, false)?;
        (dep_a, dep_b, shares)
    };

    if shares == 0 {
        return Err(ProgramError::from(DexError::ZeroOutput));
    }

    // --- Effects: update pool state BEFORE CPIs ---
    pool.reserve_a = pool.reserve_a.checked_add(deposit_a)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    pool.reserve_b = pool.reserve_b.checked_add(deposit_b)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    if is_first {
        // Total includes burned MIN_LIQUIDITY shares
        pool.total_lp_shares = shares.checked_add(MIN_LIQUIDITY as u128)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    } else {
        pool.total_lp_shares = pool.total_lp_shares.checked_add(shares)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }

    // --- Initialize or update LpPosition ---
    let provider_key = pubkey_bytes(ctx.accounts.provider);
    let pool_key = pubkey_bytes(ctx.accounts.pool_state);
    let clock = Clock::get()?;

    // Check if LpPosition already exists (data_len > 0 means initialized)
    let lp_data_len = ctx.accounts.lp_position.data_len();
    if lp_data_len == 0 {
        // Create new LpPosition PDA
        let (lp_pda, lp_bump) = arlex_lang::find_program_address(
            &[b"lp", pool_key.as_ref(), provider_key.as_ref()],
            ctx.program_id,
        );
        if ctx.accounts.lp_position.address().as_ref() != lp_pda.as_ref() {
            return Err(ProgramError::InvalidSeeds);
        }

        let rent = pinocchio::sysvars::rent::Rent::get()?;
        let lamports = rent.minimum_balance(LpPosition::SPACE);
        arlex_lang::system::instructions::CreateAccount {
            from: ctx.accounts.payer,
            to: ctx.accounts.lp_position,
            lamports,
            space: LpPosition::SPACE as u64,
            owner: ctx.program_id,
        }.invoke_signed(&[Signer::from(&[
            Seed::from(b"lp" as &[u8]),
            Seed::from(pool_key.as_ref()),
            Seed::from(provider_key.as_ref()),
            Seed::from(&[lp_bump]),
        ])])?;

        let lp = LpPosition::init(ctx.accounts.lp_position, ctx.program_id)?;
        lp.pool = pool_key;
        lp.owner = provider_key;
        lp.shares = shares;
        lp.last_update_ts = clock.unix_timestamp;
        lp.bump = lp_bump;
    } else {
        // Update existing position
        let lp = LpPosition::load_mut(ctx.accounts.lp_position, ctx.program_id)?;
        // SECURITY: Verify LpPosition belongs to this provider
        if lp.owner != provider_key {
            return Err(ProgramError::from(DexError::Unauthorized));
        }
        if lp.pool != pool_key {
            return Err(ProgramError::from(DexError::InvalidVault));
        }
        lp.shares = lp.shares.checked_add(shares)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        lp.last_update_ts = clock.unix_timestamp;
    }

    // --- Interactions: transfer tokens from provider to vaults ---
    if deposit_a > 0 {
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.provider_token_a,
            to: ctx.accounts.vault_a,
            authority: ctx.accounts.provider,
            amount: deposit_a,
        }.invoke()?;
    }

    if deposit_b > 0 {
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.provider_token_b,
            to: ctx.accounts.vault_b,
            authority: ctx.accounts.provider,
            amount: deposit_b,
        }.invoke()?;
    }

    // --- Emit event ---
    emit!(LiquidityAdded {
        pool: pool_key,
        provider: provider_key,
        amount_a: deposit_a,
        amount_b: deposit_b,
        shares_minted: shares,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
