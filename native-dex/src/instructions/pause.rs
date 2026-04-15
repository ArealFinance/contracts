use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::error::DexError;
use crate::events::{PoolPaused, PoolUnpaused};
use crate::state::{DexConfig, PoolState};
use crate::validation::pubkey_bytes;

#[derive(Accounts)]
pub struct PausePool<'info> {
    #[account(signer)]
    pub pause_authority: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,
}

#[derive(Accounts)]
pub struct UnpausePool<'info> {
    #[account(signer)]
    pub pause_authority: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,
}

pub fn pause_handler(ctx: Context<PausePool>) -> Result<()> {
    let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;

    // Only pause_authority can pause (not regular authority)
    if ctx.accounts.pause_authority.address().as_ref() != config.pause_authority.as_ref() {
        return Err(ProgramError::from(DexError::UnauthorizedPause));
    }

    let pool = PoolState::load_mut(ctx.accounts.pool_state, ctx.program_id)?;

    // Idempotent
    pool.is_active = false;

    let clock = Clock::get()?;
    emit!(PoolPaused {
        pool: pubkey_bytes(ctx.accounts.pool_state),
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

pub fn unpause_handler(ctx: Context<UnpausePool>) -> Result<()> {
    let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;

    if ctx.accounts.pause_authority.address().as_ref() != config.pause_authority.as_ref() {
        return Err(ProgramError::from(DexError::UnauthorizedPause));
    }

    let pool = PoolState::load_mut(ctx.accounts.pool_state, ctx.program_id)?;

    // Idempotent
    pool.is_active = true;

    let clock = Clock::get()?;
    emit!(PoolUnpaused {
        pool: pubkey_bytes(ctx.accounts.pool_state),
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
