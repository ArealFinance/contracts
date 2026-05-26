//! `mint_rwt` — user deposits USDC, receives earn-RWT at current NAV.
//!
//! Math (Option B "splits с total deposit"):
//!   rwa_amount       = N × split_rwa_bps      / 10_000
//!   liquidity_amount = N × split_liquidity_bps / 10_000
//!   treasury_amount  = N × split_treasury_bps  / 10_000
//!   rwt_out = rwa_amount × NAV_SCALE / NAV
//!
//! NAV = (total_invested_capital × NAV_SCALE) / total_rwt_supply
//!       (with INITIAL_NAV guard when supply == 0)
//!
//! After mint:
//!   total_invested_capital += rwa_amount + liquidity_amount
//!   total_rwt_supply       += rwt_out  (via SPL Token mint_to)
//!   → NAV strictly rises (more backing per unit RWT).
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
    // 5. Compute split: rwa, liquidity, treasury (anti-dust: each >= 0)
    // 6. Compute NAV from config.total_invested_capital + read mint.supply
    // 7. rwt_out = rwa × NAV_SCALE / NAV (zero check → ZeroRwtOutput)
    // 8. min_rwt_out check → SlippageExceeded
    // 9. CPI Transfer user → rwa_wallet (rwa amount)
    // 10. CPI Transfer user → liquidity_wallet (liquidity amount)
    // 11. CPI Transfer user → treasury_wallet (treasury amount)
    // 12. CPI mint_to user_rwt_ata (signed by EarnConfig PDA seeds)
    // 13. config.total_invested_capital += (rwa + liquidity) as u128
    // 14. Emit RwtMinted
    Ok(())
}
