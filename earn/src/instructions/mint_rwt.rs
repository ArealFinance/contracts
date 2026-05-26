//! `mint_rwt` — user deposits USDC, receives earn-RWT at current NAV.
//!
//! Pricing rule: **mint price = Book NAV** (no premium above NAV). The
//! user receives RWT only for the portion of their deposit that lands in
//! NAV-counted buckets (RWA + Liquidity). The Treasury portion is the
//! implicit mint fee — user does not receive RWT for it.
//!
//! Math:
//!   rwa_amount       = N × split_rwa_bps       / 10_000   (60% default)
//!   liquidity_amount = N × split_liquidity_bps / 10_000   (30% default)
//!   treasury_amount  = N × split_treasury_bps  / 10_000   (10% default — mint fee)
//!
//!   backing_added = rwa_amount + liquidity_amount   (= 0.9 × N at defaults)
//!   rwt_out       = backing_added × NAV_SCALE / NAV
//!
//! NAV = (total_invested_capital × NAV_SCALE) / total_rwt_supply
//!       (with INITIAL_NAV guard when supply == 0)
//!
//! After mint:
//!   total_invested_capital += backing_added
//!   total_rwt_supply       += rwt_out  (via SPL Token mint_to)
//!   → NAV stays EXACTLY constant (backing grows in lockstep with supply).
//!
//! Example at defaults: user deposits $100 at NAV $1.00 → receives 90 RWT.
//! Effective mint fee = 10% (vs $100 USDC for 100 RWT at exact NAV).
//!
//! TODO(L1): full handler implementation.

use arlex_lang::prelude::*;

use crate::constants::{EARN_CONFIG_SEED, SPL_TOKEN_PROGRAM, SYSTEM_PROGRAM};

#[derive(Accounts)]
pub struct MintRwt<'info> {
    /// User depositing USDC.
    #[account(mut, signer)]
    pub user: &'info AccountView,

    /// EarnConfig PDA (mutated: capital counter bumps).
    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,

    /// User's USDC source ATA.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_usdc_ata: &'info AccountView,

    /// User's RWT destination ATA. Created idempotently if missing.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_rwt_ata: &'info AccountView,

    /// USDC ATAs receiving the 3-way split.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwa_wallet: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub liquidity_wallet: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub treasury_wallet: &'info AccountView,

    /// Earn-RWT mint (mutated by SPL Token mint_to CPI).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(
    _ctx: Context<MintRwt>,
    _usdc_amount: u64,
    _min_rwt_out: u64,
) -> Result<()> {
    // TODO(L1): implement
    // 1. Load EarnConfig, check !is_paused
    // 2. Validate ATAs against config (rwa/liquidity/treasury addresses match)
    // 3. Validate rwt_mint matches config.rwt_mint
    // 4. amount >= min_mint_amount
    // 5. Compute split: rwa, liquidity, treasury (each ≥ 1 to avoid dust)
    // 6. Compute NAV from config.total_invested_capital + read mint.supply
    //    (INITIAL_NAV guard when supply == 0)
    // 7. backing_added = rwa + liquidity
    // 8. rwt_out = backing_added × NAV_SCALE / NAV
    //    - rwt_out == 0 → ZeroRwtOutput
    //    - rwt_out < min_rwt_out → SlippageExceeded
    // 9. CPI Transfer user → rwa_wallet (rwa amount)
    // 10. CPI Transfer user → liquidity_wallet (liquidity amount)
    // 11. CPI Transfer user → treasury_wallet (treasury amount)
    // 12. CPI mint_to user_rwt_ata (rwt_out, signed by EarnConfig PDA)
    // 13. config.total_invested_capital += backing_added as u128
    //     (NAV stays constant — supply grew by rwt_out, capital grew by backing_added,
    //      ratio preserved since rwt_out = backing_added × NAV_SCALE / NAV)
    // 14. Emit RwtMinted with nav_before == nav_after (sanity invariant)
    Ok(())
}
