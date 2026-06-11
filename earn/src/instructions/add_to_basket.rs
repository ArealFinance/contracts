//! `add_to_basket` — authority / off-chain executor reinvests income.
//!
//! The authority moves USDC into `basket_vault` and the contract records the
//! inflow into `total_invested_capital`. No RWT minted → NAV ↑ for all
//! existing holders. This is the *sole* NAV-growth channel (the off-chain
//! layer buys RWA / records appreciation, then calls this with the value).
//!
//! Source is any USDC ATA owned by the authority signer; the contract
//! constrains only the destination (`basket_vault`).
//!
//! Guard: RWT supply must already be non-zero. Otherwise authority could preload
//! capital while Book NAV still uses the INITIAL_NAV zero-supply guard, allowing
//! the first minter to capture pre-funded value at $1.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::{EARN_CONFIG_SEED, SPL_TOKEN_PROGRAM};
use crate::error::EarnError;
use crate::events::BasketGrew;
use crate::nav::calculate_nav;
use crate::state::EarnConfig;
use crate::validation::{read_mint_supply, read_token_account_mint};

#[derive(Accounts)]
pub struct AddToBasket<'info> {
    /// Must match `config.authority`.
    #[account(signer)]
    pub authority: &'info AccountView,

    /// EarnConfig PDA — mutated (capital counter bumps).
    #[account(
        mut, has_one = authority, account_type = "EarnConfig",
        seeds = [EARN_CONFIG_SEED], bump
    )]
    pub earn_config: &'info AccountView,

    /// Earn-RWT mint — supply read for the NAV snapshot.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    /// USDC source — owned by the authority.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub authority_source: &'info AccountView,

    /// Basket vault USDC ATA (EarnConfig-PDA-owned).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub basket_vault: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

fn require_nonzero_rwt_supply(supply: u64) -> Result<()> {
    if supply == 0 {
        return Err(ProgramError::from(EarnError::NoRwtSupply));
    }

    Ok(())
}

pub fn handler(ctx: Context<AddToBasket>, amount: u64) -> Result<()> {
    // `mut` binding required: capital write goes through the guard's DerefMut.
    // The earn_config PDA is NOT passed into the Transfer CPI below (the
    // authority signs, source/dest are token accounts), so the guard may stay
    // live across the transfer.
    EarnConfig::assert_account_size(ctx.accounts.earn_config)?;
    let mut config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

    // --- Checks ---
    if amount == 0 {
        return Err(ProgramError::from(EarnError::ZeroAmount));
    }
    if ctx.accounts.rwt_mint.address().as_ref() != config.rwt_mint.as_ref() {
        return Err(ProgramError::from(EarnError::InvalidRwtMint));
    }
    if ctx.accounts.basket_vault.address().as_ref() != config.basket_vault.as_ref() {
        return Err(ProgramError::from(EarnError::InvalidTokenAccount));
    }
    let source_mint = read_token_account_mint(ctx.accounts.authority_source)?;
    if source_mint != config.usdc_mint {
        return Err(ProgramError::from(EarnError::InvalidTokenAccount));
    }

    // --- NAV before snapshot ---
    let supply = read_mint_supply(ctx.accounts.rwt_mint)?;
    require_nonzero_rwt_supply(supply)?;
    let nav_before = calculate_nav(config.total_invested_capital, supply)?;

    // --- Effects: bump capital BEFORE the CPI ---
    config.total_invested_capital = config
        .total_invested_capital
        .checked_add(amount as u128)
        .ok_or_else(|| ProgramError::from(EarnError::MathOverflow))?;

    let nav_after = calculate_nav(config.total_invested_capital, supply)?;

    // --- Interaction: authority transfers USDC into basket_vault ---
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.authority_source,
        to: ctx.accounts.basket_vault,
        authority: ctx.accounts.authority,
        amount,
    }
    .invoke()?;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(BasketGrew {
        amount,
        nav_before,
        nav_after,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_code(err: ProgramError) -> u32 {
        match err {
            ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    #[test]
    fn add_to_basket_rejects_zero_rwt_supply() {
        let err = require_nonzero_rwt_supply(0).unwrap_err();
        assert_eq!(
            custom_code(err),
            custom_code(ProgramError::from(EarnError::NoRwtSupply)),
        );
    }

    #[test]
    fn add_to_basket_allows_existing_rwt_supply() {
        assert!(require_nonzero_rwt_supply(1).is_ok());
    }
}
