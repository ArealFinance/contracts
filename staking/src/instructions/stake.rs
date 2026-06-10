//! `stake(rwt_amount, min_strwt_out)` — deposit RWT, mint stRWT at the
//! current rate.
//!
//! staking.mdx §"stake":
//!   strwt_out = rwt_amount × (strwt_supply + V_SHARES) / (active + V_ASSETS)
//!   (u128 intermediate, floor — rounds toward the pool / against the user)
//!
//! Reverts: StakingPaused, BelowMinStake, ZeroStrwtOutput, SlippageExceeded.
//! Effects (before CPI): `total_rwt_active += rwt_amount`.
//! CPI: Transfer user → pool_vault; mint_to user (stRWT, signed by config PDA).

use arlex_lang::prelude::*;
use pinocchio::cpi::{Seed, Signer};
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::StakingError;
use crate::events::Staked;
use crate::rate::{rate_snapshot, strwt_out_for_stake};
use crate::state::StakingConfig;
use crate::token::{read_mint_supply, read_token_account_mint};

#[derive(Accounts)]
pub struct Stake<'info> {
    #[account(signer)]
    pub user: &'info AccountView,

    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,

    /// stRWT mint (mutated by mint_to). Authority = config PDA.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub strwt_mint: &'info AccountView,

    /// User's RWT source ATA.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_rwt_ata: &'info AccountView,

    /// User's stRWT destination ATA (must already exist).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_strwt_ata: &'info AccountView,

    /// RWT pool vault (config-PDA-owned RWT ATA).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub pool_vault: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(ctx: Context<Stake>, rwt_amount: u64, min_strwt_out: u64) -> Result<()> {
    // CHECKS + EFFECTS in a scope that drops the StakingConfig borrow guard
    // before the MintTo CPI below. The staking_config PDA is passed into MintTo
    // as the mint_authority signer, which the checked invoke rejects while the
    // account is still mutably borrowed. Copy out the bump and computed amounts;
    // the guard's Drop releases the borrow flag at the end of this block.
    let (config_bump, active_after, strwt_supply_after, rate_after, strwt_out);
    {
        let mut config = StakingConfig::load_mut(ctx.accounts.staking_config, ctx.program_id)?;

        // --- Checks ---
        if config.is_paused {
            return Err(ProgramError::from(StakingError::StakingPaused));
        }
        if rwt_amount < config.min_stake_amount {
            return Err(ProgramError::from(StakingError::BelowMinStake));
        }

        // Validate accounts against config.
        if ctx.accounts.strwt_mint.address().as_ref() != config.strwt_mint.as_ref() {
            return Err(ProgramError::from(StakingError::InvalidTokenAccount));
        }
        if ctx.accounts.pool_vault.address().as_ref() != config.pool_vault.as_ref() {
            return Err(ProgramError::from(StakingError::InvalidTokenAccount));
        }
        // user_rwt_ata must hold RWT, user_strwt_ata must hold stRWT.
        if read_token_account_mint(ctx.accounts.user_rwt_ata)? != config.rwt_mint {
            return Err(ProgramError::from(StakingError::InvalidTokenAccount));
        }
        if read_token_account_mint(ctx.accounts.user_strwt_ata)? != config.strwt_mint {
            return Err(ProgramError::from(StakingError::InvalidTokenAccount));
        }

        // --- Rate math (read live stRWT supply from the mint) ---
        let strwt_supply = read_mint_supply(ctx.accounts.strwt_mint)?;
        let active = config.total_rwt_active;

        strwt_out = strwt_out_for_stake(rwt_amount, strwt_supply, active)
            .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;

        if strwt_out == 0 {
            return Err(ProgramError::from(StakingError::ZeroStrwtOutput));
        }
        if strwt_out < min_strwt_out {
            return Err(ProgramError::from(StakingError::SlippageExceeded));
        }

        // --- Effects: bump active counter BEFORE any CPI ---
        active_after = active
            .checked_add(rwt_amount)
            .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;
        config.total_rwt_active = active_after;

        config_bump = config.bump;
        strwt_supply_after = strwt_supply
            .checked_add(strwt_out)
            .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;
        rate_after = rate_snapshot(active_after, strwt_supply_after)
            .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;
    } // StakingConfig guard dropped here — borrow flag released before the CPIs.

    // --- Interactions ---
    // 1. User transfers RWT → pool_vault (user is signer/authority).
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.user_rwt_ata,
        to: ctx.accounts.pool_vault,
        authority: ctx.accounts.user,
        amount: rwt_amount,
    }
    .invoke()?;

    // 2. Config PDA mints stRWT to the user.
    // The StakingConfig borrow guard was dropped at the end of the CHECKS+EFFECTS
    // block above, so passing staking_config as the mint_authority signer here is
    // accepted by the checked invoke (no live AccountRefMut on this account).
    let bump_arr = [config_bump];
    let seeds = [
        Seed::from(STAKING_CONFIG_SEED),
        Seed::from(bump_arr.as_ref()),
    ];
    let signer = Signer::from(&seeds);
    arlex_lang::token::instructions::MintTo {
        mint: ctx.accounts.strwt_mint,
        account: ctx.accounts.user_strwt_ata,
        mint_authority: ctx.accounts.staking_config,
        amount: strwt_out,
    }
    .invoke_signed(&[signer])?;

    // --- Emit ---
    let clock = Clock::get()?;
    let mut user_arr = [0u8; 32];
    user_arr.copy_from_slice(ctx.accounts.user.address().as_ref());
    emit!(Staked {
        user: user_arr,
        rwt_in: rwt_amount,
        strwt_out,
        rate_after,
        active_after,
        strwt_supply_after,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
