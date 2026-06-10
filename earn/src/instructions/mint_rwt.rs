//! `mint_rwt` — user deposits USDC at Book NAV (+1% fee), receives earn-RWT.
//!
//! Pricing rule: **mint price = Book NAV × (1 + mint_fee_bps)**.
//!   - `usdc_amount` is the basket body (NOT including fee).
//!   - `fee = usdc_amount × mint_fee_bps / 10_000` (default 1%), charged on top.
//!   - body  → basket_vault       (total_invested_capital += usdc_amount)
//!   - fee   → dao_fee_destination (Areal revenue, EXCLUDED from capital)
//!
//! RWT output (u128 before multiply — `usdc_amount × NAV_SCALE` overflows u64
//! for large deposits; truncating division rounds toward the protocol):
//!   ```text
//!   nav     = total_invested_capital == 0 ? INITIAL_NAV
//!                                         : (capital × NAV_SCALE / supply)
//!   rwt_out = (usdc_amount as u128 × NAV_SCALE) / nav
//!   ```
//!
//! Mint invariant: supply and capital grow synchronously → NAV unchanged.
//! Effects (`total_invested_capital += body`) are applied BEFORE the CPIs.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::EarnError;
use crate::events::{RwtMinted, MINT_SOURCE_USER};
use crate::nav::calculate_nav;
use crate::state::EarnConfig;
use crate::validation::{read_mint_supply, read_token_account_mint};

#[derive(Accounts)]
pub struct MintRwt<'info> {
    /// User depositing USDC.
    #[account(signer)]
    pub user: &'info AccountView,

    /// EarnConfig PDA (mutated: capital counter bumps).
    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,

    /// Earn-RWT mint (mutated by SPL Token mint_to CPI). Supply read for NAV.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    /// User's USDC source ATA.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_usdc: &'info AccountView,

    /// User's RWT destination ATA (receives minted RWT).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_rwt: &'info AccountView,

    /// Basket vault USDC ATA (EarnConfig-PDA-owned) — receives the body.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub basket_vault: &'info AccountView,

    /// DAO fee destination USDC ATA — receives the 1% commission.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub dao_fee_destination: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(ctx: Context<MintRwt>, usdc_amount: u64, min_rwt_out: u64) -> Result<()> {
    // CHECKS + EFFECTS in a scope that drops the EarnConfig borrow guard before
    // the CPIs below. The earn_config PDA is passed into the MintTo CPI as the
    // mint_authority signer, which the checked invoke rejects while the account
    // is still mutably borrowed. Copy out the bump and computed amounts; the
    // guard's Drop releases the borrow flag at the end of this block.
    let (fee, rwt_out, nav_after, config_bump);
    {
        let mut config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

        // --- Checks ---
        if config.is_paused {
            return Err(ProgramError::from(EarnError::EarnPaused));
        }
        if usdc_amount == 0 {
            return Err(ProgramError::from(EarnError::ZeroAmount));
        }
        if usdc_amount < config.min_mint_amount {
            return Err(ProgramError::from(EarnError::BelowMinMint));
        }
        if min_rwt_out == 0 {
            return Err(ProgramError::from(EarnError::ZeroSlippage));
        }

        // Validate accounts match config state.
        if ctx.accounts.rwt_mint.address().as_ref() != config.rwt_mint.as_ref() {
            return Err(ProgramError::from(EarnError::InvalidRwtMint));
        }
        if ctx.accounts.basket_vault.address().as_ref() != config.basket_vault.as_ref() {
            return Err(ProgramError::from(EarnError::InvalidTokenAccount));
        }
        if ctx.accounts.dao_fee_destination.address().as_ref() != config.dao_fee_destination.as_ref() {
            return Err(ProgramError::from(EarnError::InvalidTokenAccount));
        }

        // SECURITY: validate token-account mints (defense-in-depth; SPL also checks).
        // user_usdc, basket_vault, dao_fee_destination must all hold USDC.
        let deposit_mint = read_token_account_mint(ctx.accounts.user_usdc)?;
        if deposit_mint != config.usdc_mint {
            return Err(ProgramError::from(EarnError::InvalidTokenAccount));
        }
        let basket_mint = read_token_account_mint(ctx.accounts.basket_vault)?;
        if basket_mint != config.usdc_mint {
            return Err(ProgramError::from(EarnError::InvalidTokenAccount));
        }
        let dao_fee_mint = read_token_account_mint(ctx.accounts.dao_fee_destination)?;
        if dao_fee_mint != config.usdc_mint {
            return Err(ProgramError::from(EarnError::InvalidTokenAccount));
        }
        // user_rwt must hold the earn-RWT mint.
        let user_rwt_mint = read_token_account_mint(ctx.accounts.user_rwt)?;
        if user_rwt_mint != config.rwt_mint {
            return Err(ProgramError::from(EarnError::InvalidTokenAccount));
        }

        // --- Pricing ---
        // NAV from current capital + on-chain supply (INITIAL_NAV guard if supply == 0).
        let supply = read_mint_supply(ctx.accounts.rwt_mint)?;
        let nav = calculate_nav(config.total_invested_capital, supply)?;

        // fee = usdc_amount × mint_fee_bps / 10_000 (charged on top of the body).
        fee = arlex_lang::math::mul_div_u64(
            usdc_amount, config.mint_fee_bps as u64, BPS_DENOMINATOR,
        ).ok_or_else(|| ProgramError::from(EarnError::MathOverflow))?;

        // rwt_out = (usdc_amount × NAV_SCALE) / nav — u128 BEFORE the multiply.
        let rwt_out_u128 = (usdc_amount as u128)
            .checked_mul(NAV_SCALE as u128)
            .ok_or_else(|| ProgramError::from(EarnError::MathOverflow))?
            / (nav as u128);
        rwt_out = u64::try_from(rwt_out_u128)
            .map_err(|_| ProgramError::from(EarnError::MathOverflow))?;

        // SECURITY: reject a mint that would produce 0 RWT (user pays, gets nothing).
        if rwt_out == 0 {
            return Err(ProgramError::from(EarnError::ZeroRwtOutput));
        }
        // SECURITY: slippage protection.
        if rwt_out < min_rwt_out {
            return Err(ProgramError::from(EarnError::SlippageExceeded));
        }

        // --- Effects: update config state BEFORE CPIs ---
        // Only the body grows capital; the fee is DAO revenue, excluded → NAV-neutral.
        config.total_invested_capital = config.total_invested_capital
            .checked_add(usdc_amount as u128)
            .ok_or_else(|| ProgramError::from(EarnError::MathOverflow))?;

        // nav_after == nav (mint invariant): supply grows by rwt_out, capital by
        // body, ratio preserved since rwt_out = body × NAV_SCALE / nav.
        nav_after = nav;

        // Copy out the bump for the MintTo signer below.
        config_bump = config.bump;
    } // EarnConfig guard dropped here — borrow flag released before the CPIs.

    // --- Interactions: CPIs ---

    // 1. User transfers body → basket_vault.
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.user_usdc,
        to: ctx.accounts.basket_vault,
        authority: ctx.accounts.user,
        amount: usdc_amount,
    }.invoke()?;

    // 2. User transfers fee → dao_fee_destination.
    if fee > 0 {
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.user_usdc,
            to: ctx.accounts.dao_fee_destination,
            authority: ctx.accounts.user,
            amount: fee,
        }.invoke()?;
    }

    // 3. EarnConfig PDA mints RWT to the user.
    // The EarnConfig borrow guard was dropped at the end of the CHECKS+EFFECTS
    // block above, so passing earn_config as the mint_authority signer here is
    // accepted by the checked invoke (no live AccountRefMut on this account).
    let bump = [config_bump];
    let seeds = [
        Seed::from(EARN_CONFIG_SEED),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    arlex_lang::token::instructions::MintTo {
        mint: ctx.accounts.rwt_mint,
        account: ctx.accounts.user_rwt,
        mint_authority: ctx.accounts.earn_config,
        amount: rwt_out,
    }.invoke_signed(&[signer])?;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(RwtMinted {
        minter: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.user.address().as_ref());
            arr
        },
        deposit_body: usdc_amount,
        fee,
        rwt_out,
        nav_after,
        source: MINT_SOURCE_USER,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Arithmetic characterization for the Book-NAV mint. Pins the fee math,
    //! the mint invariant (NAV unchanged), bootstrap NAV, and the u128 path.
    use super::*;
    use crate::nav::calculate_nav;

    /// Mirror the handler's pricing math without a BPF runtime.
    fn price(capital: u128, supply: u64, usdc_amount: u64, fee_bps: u16) -> (u64, u64, u64) {
        let nav = calculate_nav(capital, supply).unwrap();
        let fee = arlex_lang::math::mul_div_u64(usdc_amount, fee_bps as u64, BPS_DENOMINATOR).unwrap();
        let rwt_out_u128 = (usdc_amount as u128) * (NAV_SCALE as u128) / (nav as u128);
        let rwt_out = u64::try_from(rwt_out_u128).unwrap();
        (nav, fee, rwt_out)
    }

    /// Bootstrap: first mint at supply == 0 sees NAV $1.00, so $100 body → 100 RWT.
    #[test]
    fn bootstrap_first_mint_at_initial_nav() {
        let (nav, fee, rwt_out) = price(0, 0, 100_000_000, DEFAULT_MINT_FEE_BPS);
        assert_eq!(nav, INITIAL_NAV, "supply 0 → $1.00");
        assert_eq!(fee, 1_000_000, "1% of $100 body = $1");
        assert_eq!(rwt_out, 100_000_000, "100 RWT at NAV $1.00");
    }

    /// Fee math: fee is 1% of the body; the body (not the fee) grows capital.
    #[test]
    fn fee_is_one_percent_and_excluded_from_capital() {
        let usdc_amount: u64 = 100_000_000; // $100 body
        let (_, fee, _) = price(0, 0, usdc_amount, DEFAULT_MINT_FEE_BPS);
        assert_eq!(fee, 1_000_000); // $1 fee
        // Capital grows ONLY by the body (mirrors the handler effect).
        let capital_after = 0u128 + usdc_amount as u128;
        assert_eq!(capital_after, 100_000_000u128, "fee excluded from capital");
    }

    /// Mint invariant: NAV is unchanged after a mint at any starting NAV.
    /// supply grows by rwt_out, capital by body, ratio preserved.
    #[test]
    fn mint_invariant_nav_unchanged() {
        // Start at NAV = $1.20: capital = 120, supply = 100 (in RWT base units).
        let capital0: u128 = 120_000_000;
        let supply0: u64 = 100_000_000;
        let nav0 = calculate_nav(capital0, supply0).unwrap();
        assert_eq!(nav0, 1_200_000, "starting NAV $1.20");

        let usdc_amount: u64 = 60_000_000; // $60 body
        let (_, _, rwt_out) = price(capital0, supply0, usdc_amount, DEFAULT_MINT_FEE_BPS);

        let capital1 = capital0 + usdc_amount as u128;
        let supply1 = supply0 + rwt_out;
        let nav1 = calculate_nav(capital1, supply1).unwrap();
        // NAV must be unchanged (within ±1 lamport from integer truncation).
        assert!(
            (nav1 as i64 - nav0 as i64).abs() <= 1,
            "mint invariant violated: nav before {nav0}, after {nav1}"
        );
    }

    /// u128 path: a large deposit where usdc_amount × NAV_SCALE > u64::MAX must
    /// not overflow (would wrap if done in u64).
    #[test]
    fn u128_path_large_deposit_no_overflow() {
        // usdc_amount = 20_000_000_000_000 ($20M). × NAV_SCALE (1e6) = 2e19,
        // which exceeds u64::MAX (~1.8e19) — must be computed in u128.
        let usdc_amount: u64 = 20_000_000_000_000;
        assert!(
            (usdc_amount as u128) * (NAV_SCALE as u128) > u64::MAX as u128,
            "test fixture must exceed u64 to exercise the u128 path",
        );
        let (nav, _, rwt_out) = price(0, 0, usdc_amount, DEFAULT_MINT_FEE_BPS);
        assert_eq!(nav, INITIAL_NAV);
        assert_eq!(rwt_out, usdc_amount, "at NAV $1.00, RWT out == body");
    }
}
