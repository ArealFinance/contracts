//! `deposit_rewards(rwt_amount)` — reward_depositor adds RWT to the pool,
//! raising the rate. NO stRWT minted; NOT gated by pause.
//!
//! staking.mdx §"deposit_rewards":
//!   - Signer MUST equal `reward_depositor` (UnauthorizedRewardDepositor).
//!   - Effects: `total_rwt_active += rwt_amount`.
//!   - CPI: Transfer reward_depositor → pool_vault.
//!   - stRWT supply unchanged → rate rises. Emits RewardsDeposited.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::StakingError;
use crate::events::RewardsDeposited;
use crate::rate::rate_snapshot;
use crate::state::StakingConfig;
use crate::token::{read_mint_supply, read_token_account_mint};

#[derive(Accounts)]
pub struct DepositRewards<'info> {
    #[account(signer)]
    pub depositor: &'info AccountView,

    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,

    /// stRWT mint (read-only) — for the rate snapshot in the event.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub strwt_mint: &'info AccountView,

    /// Depositor's RWT source ATA.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub depositor_rwt_ata: &'info AccountView,

    /// RWT pool vault (config-PDA-owned RWT ATA).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub pool_vault: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(ctx: Context<DepositRewards>, rwt_amount: u64) -> Result<()> {
    // `mut` binding: counter write goes through the guard's DerefMut. The
    // staking_config PDA is NOT passed into the Transfer CPI below (the depositor
    // signs), so the guard may stay live across the transfer.
    let mut config = StakingConfig::load_mut(ctx.accounts.staking_config, ctx.program_id)?;

    // --- Whitelist: only the configured reward_depositor (NOT pause-gated) ---
    if ctx.accounts.depositor.address().as_ref() != config.reward_depositor.as_ref() {
        return Err(ProgramError::from(StakingError::UnauthorizedRewardDepositor));
    }
    if rwt_amount == 0 {
        return Err(ProgramError::from(StakingError::ZeroRwtOutput));
    }

    // Validate accounts.
    if ctx.accounts.pool_vault.address().as_ref() != config.pool_vault.as_ref() {
        return Err(ProgramError::from(StakingError::InvalidTokenAccount));
    }
    if ctx.accounts.strwt_mint.address().as_ref() != config.strwt_mint.as_ref() {
        return Err(ProgramError::from(StakingError::InvalidTokenAccount));
    }
    if read_token_account_mint(ctx.accounts.depositor_rwt_ata)? != config.rwt_mint {
        return Err(ProgramError::from(StakingError::InvalidTokenAccount));
    }

    // --- Effects: bump active BEFORE CPI (supply unchanged → rate rises) ---
    let active_after = config
        .total_rwt_active
        .checked_add(rwt_amount)
        .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;
    config.total_rwt_active = active_after;

    let strwt_supply = read_mint_supply(ctx.accounts.strwt_mint)?;
    let rate_after = rate_snapshot(active_after, strwt_supply)
        .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;

    // --- Interaction: depositor transfers RWT → pool_vault ---
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.depositor_rwt_ata,
        to: ctx.accounts.pool_vault,
        authority: ctx.accounts.depositor,
        amount: rwt_amount,
    }
    .invoke()?;

    // --- Emit ---
    let clock = Clock::get()?;
    let mut depositor_arr = [0u8; 32];
    depositor_arr.copy_from_slice(ctx.accounts.depositor.address().as_ref());
    emit!(RewardsDeposited {
        depositor: depositor_arr,
        rwt_in: rwt_amount,
        rate_after,
        active_after,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
