//! `stream_inflow` — authority-only USDC top-up that bumps NAV.
//!
//! Anyone in the authority role moves USDC into `liquidity_wallet` and
//! the contract records the inflow into `total_invested_capital`. No RWT
//! minted. Net effect: NAV ↑ for all existing holders.
//!
//! Source: any USDC ATA owned by the authority signer. The contract does
//! not constrain the source — only the destination is the program's
//! liquidity_wallet.
//!
//! TODO(L1): full handler implementation.

use arlex_lang::prelude::*;

use crate::constants::{EARN_CONFIG_SEED, SPL_TOKEN_PROGRAM};

#[derive(Accounts)]
pub struct StreamInflow<'info> {
    /// Must match `config.authority`.
    #[account(signer)]
    pub authority: &'info AccountView,

    /// EarnConfig PDA — mutated (capital counter bumps).
    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,

    /// USDC source — owned by authority.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub source_usdc_ata: &'info AccountView,

    /// Earn liquidity_wallet (USDC ATA, PDA-owned).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub liquidity_wallet: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(_ctx: Context<StreamInflow>, _amount: u64) -> Result<()> {
    // TODO(L1):
    // 1. Load EarnConfig
    // 2. Check signer == config.authority
    // 3. amount > 0
    // 4. Validate liquidity_wallet address matches config
    // 5. NAV before snapshot
    // 6. CPI Transfer source → liquidity_wallet (signed by authority)
    // 7. config.total_invested_capital += amount as u128
    // 8. NAV after snapshot
    // 9. Emit StreamReceived
    Ok(())
}
