//! shift_liquidity — Rebalancer-only instruction.
//!
//! Redistributes bin liquidity using pyramid formula to track NAV price.
//! No tokens enter or leave vaults — internal redistribution only.
//! Conservation invariant: sum(bins) == original reserves per token.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::LiquidityShifted;
use crate::state::*;
use crate::validation::*;

#[derive(Accounts)]
pub struct ShiftLiquidity<'info> {
    #[account(signer)]
    pub rebalancer: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,

    #[account(mut)]
    pub bin_array: &'info AccountView,
}

pub fn handler(
    ctx: Context<ShiftLiquidity>,
    nav_bin: i32,
    target_bin_count: u16,
) -> Result<()> {
    let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;
    let pool = PoolState::load_mut(ctx.accounts.pool_state, ctx.program_id)?;

    // --- Authorization: only rebalancer ---
    let rebalancer_key = pubkey_bytes(ctx.accounts.rebalancer);
    if rebalancer_key != config.rebalancer {
        return Err(ProgramError::from(DexError::Unauthorized));
    }

    // --- Pool must be concentrated ---
    if pool.pool_type != POOL_TYPE_CONCENTRATED {
        return Err(ProgramError::from(DexError::InvalidPoolType));
    }

    // --- Validate target_bin_count ---
    if target_bin_count == 0 || target_bin_count as usize > MAX_BINS {
        return Err(ProgramError::from(DexError::InvalidBinRange));
    }

    // --- Load BinArray ---
    let bin_array = BinArray::load_mut(ctx.accounts.bin_array, ctx.program_id)?;

    // Validate BinArray belongs to this pool
    let pool_key = pubkey_bytes(ctx.accounts.pool_state);
    if bin_array.pool != pool_key {
        return Err(ProgramError::from(DexError::InvalidVault));
    }

    // --- MAX_SHIFT_DISTANCE check ---
    let shift_distance = (nav_bin - bin_array.active_bin_id).abs();
    if shift_distance > MAX_SHIFT_DISTANCE {
        return Err(ProgramError::from(DexError::ShiftTooLarge));
    }

    // --- Check no-op: compute new range and compare ---
    let half = target_bin_count as i32 / 2;
    let new_lower = nav_bin - half;
    let new_upper = nav_bin + (target_bin_count as i32 - 1 - half);

    // Find current effective range (first/last non-empty bin)
    let array_lower = bin_array.lower_bin_id;
    let array_upper = array_lower + MAX_BINS as i32 - 1;

    // Check new range fits within BinArray
    if new_lower < array_lower || new_upper > array_upper {
        return Err(ProgramError::from(DexError::InvalidBinRange));
    }

    // Save old range for event
    let old_lower = array_lower;
    let old_upper = array_upper;

    // --- Verify BinArray PDA derivation ---
    let (expected_bin_pda, _) = arlex_lang::find_program_address(
        &[b"bins", pool_key.as_ref()],
        ctx.program_id,
    );
    if ctx.accounts.bin_array.address().as_ref() != expected_bin_pda.as_ref() {
        return Err(ProgramError::InvalidSeeds);
    }

    // --- ShiftNoOp check: reject if nav_bin unchanged ---
    if nav_bin == bin_array.active_bin_id {
        return Err(ProgramError::from(DexError::ShiftNoOp));
    }

    // --- Execute pyramid redistribution ---
    crate::concentrated::shift_pyramid(bin_array, nav_bin, target_bin_count)?;

    // --- Sync active_bin_id: shift does NOT move active_bin, but BinArray
    // tracks where swaps last moved it. After pyramid redistribution, swaps
    // should start from nav_bin where liquidity is now centered. ---
    bin_array.active_bin_id = nav_bin;
    pool.active_bin_id = nav_bin;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(LiquidityShifted {
        pool: pool_key,
        rebalancer: rebalancer_key,
        old_lower,
        old_upper,
        new_lower,
        new_upper,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Liquidity shifted");
    Ok(())
}
