use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::LiquidityRemoved;
use crate::state::*;
use crate::amm::calculate_remove_amounts;
use crate::validation::*;

#[derive(Accounts)]
pub struct RemoveLiquidity<'info> {
    #[account(signer)]
    pub provider: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,

    #[account(mut)]
    pub lp_position: &'info AccountView,

    // Provider's token accounts (receive withdrawn tokens)
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
}

pub fn handler(
    ctx: Context<RemoveLiquidity>,
    shares_to_burn: u128,
) -> Result<()> {
    let pool = PoolState::load_mut(ctx.accounts.pool_state, ctx.program_id)?;

    // NOTE: remove_liquidity does NOT check pool.is_active — LPs can always exit (by design)

    if shares_to_burn == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount));
    }

    // SECURITY: Validate vaults match pool state
    validate_vault(ctx.accounts.vault_a, &pool.vault_a)?;
    validate_vault(ctx.accounts.vault_b, &pool.vault_b)?;

    // --- Validate LP position ---
    let lp = LpPosition::load_mut(ctx.accounts.lp_position, ctx.program_id)?;
    let provider_key = pubkey_bytes(ctx.accounts.provider);

    if lp.owner != provider_key {
        return Err(ProgramError::from(DexError::Unauthorized));
    }
    if lp.shares < shares_to_burn {
        return Err(ProgramError::from(DexError::InsufficientShares));
    }

    // --- Calculate proportional withdrawal ---
    let (amount_a, amount_b) = calculate_remove_amounts(
        shares_to_burn, pool.reserve_a, pool.reserve_b, pool.total_lp_shares,
    )?;

    // --- Effects: update state BEFORE CPIs ---
    pool.reserve_a = pool.reserve_a.checked_sub(amount_a)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    pool.reserve_b = pool.reserve_b.checked_sub(amount_b)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    pool.total_lp_shares = pool.total_lp_shares.checked_sub(shares_to_burn)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    lp.shares = lp.shares.checked_sub(shares_to_burn)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    let clock = Clock::get()?;
    lp.last_update_ts = clock.unix_timestamp;

    // If shares == 0, close the LpPosition and return rent to provider
    let close_position = lp.shares == 0;

    // --- Interactions: transfer tokens from vaults to provider ---
    let pool_key = pubkey_bytes(ctx.accounts.pool_state);
    let bump = [pool.bump];

    if amount_a > 0 {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.vault_a,
            to: ctx.accounts.provider_token_a,
            authority: ctx.accounts.pool_state,
            amount: amount_a,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    if amount_b > 0 {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.vault_b,
            to: ctx.accounts.provider_token_b,
            authority: ctx.accounts.pool_state,
            amount: amount_b,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // Close LpPosition if fully withdrawn (rent back to provider)
    if close_position {
        // Transfer lamports from LpPosition to provider
        let lp_lamports = ctx.accounts.lp_position.lamports();
        let provider_lamports = ctx.accounts.provider.lamports();
        ctx.accounts.provider.set_lamports(
            provider_lamports.checked_add(lp_lamports)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
        );
        ctx.accounts.lp_position.set_lamports(0);
        // Zero owner + data_len + lamports to close the account
        ctx.accounts.lp_position.close()?;
    }

    // --- Emit event ---
    emit!(LiquidityRemoved {
        pool: pool_key,
        provider: provider_key,
        amount_a,
        amount_b,
        shares_burned: shares_to_burn,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
