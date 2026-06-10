//! `update_config` — authority-only admin tuning.
//!
//! Lets the authority retune the mint fee (bps), the minimum mint amount, and
//! the DAO fee destination. The `basket_vault`, `rwt_mint`, `usdc_mint`, and
//! `pause_authority` are IMMUTABLE — set once at `initialize`.
//!
//! `mint_fee_bps` is capped at `MAX_MINT_FEE_BPS` (10%); `min_mint_amount` is
//! bounded to `[MIN_MINT_AMOUNT, MAX_MIN_MINT_AMOUNT]` so the authority can
//! neither lower it below the anti-dust floor nor raise it high enough to
//! soft-brick minting; `dao_fee_destination` cannot be the zero address.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::{
    EARN_CONFIG_SEED, MAX_MINT_FEE_BPS, MAX_MIN_MINT_AMOUNT, MIN_MINT_AMOUNT,
};
use crate::error::EarnError;
use crate::events::EarnConfigUpdated;
use crate::state::EarnConfig;

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "EarnConfig",
        seeds = [EARN_CONFIG_SEED], bump
    )]
    pub earn_config: &'info AccountView,
}

fn validate_update_config_inputs(
    mint_fee_bps: u16,
    min_mint_amount: u64,
    dao_fee_destination: [u8; 32],
    basket_vault: [u8; 32],
) -> Result<()> {
    // L2: cap the mint fee at 10% (was 100% via BPS_DENOMINATOR).
    if mint_fee_bps > MAX_MINT_FEE_BPS {
        return Err(ProgramError::from(EarnError::FeeTooHigh));
    }
    if dao_fee_destination == [0u8; 32] {
        return Err(ProgramError::from(EarnError::InvalidFeeDestination));
    }
    // SECURITY (L1): the fee destination MUST differ from the immutable basket
    // vault. mint_rwt routes the deposit body into the basket (counted into
    // capital) and the 1% fee into dao_fee_destination (EXCLUDED from capital);
    // collapsing them onto one account would leak fees into the basket without
    // accounting, drifting the vault balance above tracked capital (NAV desync).
    if dao_fee_destination == basket_vault {
        return Err(ProgramError::from(EarnError::FeeDestinationIsBasketVault));
    }
    if min_mint_amount < MIN_MINT_AMOUNT {
        return Err(ProgramError::from(EarnError::BelowMinMint));
    }
    // L3: bound the anti-dust floor so it cannot be raised out of reach.
    if min_mint_amount > MAX_MIN_MINT_AMOUNT {
        return Err(ProgramError::from(EarnError::MinMintTooHigh));
    }

    Ok(())
}

pub fn handler(
    ctx: Context<UpdateConfig>,
    mint_fee_bps: u16,
    min_mint_amount: u64,
    dao_fee_destination: [u8; 32],
) -> Result<()> {
    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

    // --- Checks ---
    // Copy the packed field to a local before comparing to avoid an unaligned
    // reference into the repr(C, packed) struct. `basket_vault` is immutable.
    let basket_vault = config.basket_vault;
    validate_update_config_inputs(
        mint_fee_bps,
        min_mint_amount,
        dao_fee_destination,
        basket_vault,
    )?;

    // --- Effects ---
    config.mint_fee_bps = mint_fee_bps;
    config.min_mint_amount = min_mint_amount;
    config.dao_fee_destination = dao_fee_destination;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(EarnConfigUpdated {
        mint_fee_bps,
        min_mint_amount,
        dao_fee_destination,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::DEFAULT_MINT_FEE_BPS;

    fn custom_code(err: ProgramError) -> u32 {
        match err {
            ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    fn assert_err_code(result: Result<()>, expected: EarnError) {
        assert_eq!(
            custom_code(result.unwrap_err()),
            custom_code(ProgramError::from(expected)),
        );
    }

    // A basket vault distinct from the [1u8; 32] fee destination used below, so
    // these cases exercise only the guard under test (not the equality guard).
    const BASKET: [u8; 32] = [2u8; 32];

    #[test]
    fn update_config_rejects_min_mint_amount_below_floor() {
        assert_err_code(
            validate_update_config_inputs(
                DEFAULT_MINT_FEE_BPS,
                MIN_MINT_AMOUNT - 1,
                [1u8; 32],
                BASKET,
            ),
            EarnError::BelowMinMint,
        );
    }

    #[test]
    fn update_config_accepts_min_mint_amount_at_floor() {
        assert!(validate_update_config_inputs(
            DEFAULT_MINT_FEE_BPS,
            MIN_MINT_AMOUNT,
            [1u8; 32],
            BASKET,
        )
        .is_ok());
    }

    #[test]
    fn update_config_keeps_existing_fee_and_destination_guards() {
        // L2: above the 10% ceiling is rejected with FeeTooHigh.
        assert_err_code(
            validate_update_config_inputs(
                MAX_MINT_FEE_BPS + 1,
                MIN_MINT_AMOUNT,
                [1u8; 32],
                BASKET,
            ),
            EarnError::FeeTooHigh,
        );
        assert_err_code(
            validate_update_config_inputs(
                DEFAULT_MINT_FEE_BPS,
                MIN_MINT_AMOUNT,
                [0u8; 32],
                BASKET,
            ),
            EarnError::InvalidFeeDestination,
        );
    }

    #[test]
    fn update_config_rejects_fee_destination_equal_to_basket_vault() {
        // L1: collapsing the fee destination onto the basket vault is rejected —
        // it would leak unaccounted fees into the basket and desync NAV.
        assert_err_code(
            validate_update_config_inputs(
                DEFAULT_MINT_FEE_BPS,
                MIN_MINT_AMOUNT,
                BASKET,
                BASKET,
            ),
            EarnError::FeeDestinationIsBasketVault,
        );
    }

    #[test]
    fn update_config_accepts_fee_destination_distinct_from_basket_vault() {
        // L1: a fee destination that differs from the basket vault passes.
        assert!(validate_update_config_inputs(
            DEFAULT_MINT_FEE_BPS,
            MIN_MINT_AMOUNT,
            [1u8; 32],
            BASKET,
        )
        .is_ok());
    }

    #[test]
    fn update_config_accepts_fee_at_ceiling() {
        assert!(validate_update_config_inputs(
            MAX_MINT_FEE_BPS,
            MIN_MINT_AMOUNT,
            [1u8; 32],
            BASKET,
        )
        .is_ok());
    }

    #[test]
    fn update_config_rejects_min_mint_amount_above_ceiling() {
        // L3: a floor raised out of reach is rejected with MinMintTooHigh.
        assert_err_code(
            validate_update_config_inputs(
                DEFAULT_MINT_FEE_BPS,
                MAX_MIN_MINT_AMOUNT + 1,
                [1u8; 32],
                BASKET,
            ),
            EarnError::MinMintTooHigh,
        );
    }

    #[test]
    fn update_config_accepts_min_mint_amount_at_ceiling() {
        assert!(validate_update_config_inputs(
            DEFAULT_MINT_FEE_BPS,
            MAX_MIN_MINT_AMOUNT,
            [1u8; 32],
            BASKET,
        )
        .is_ok());
    }
}
