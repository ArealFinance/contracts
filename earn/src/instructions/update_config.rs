//! `update_config` — authority-only admin tuning.
//!
//! Lets the authority retune the mint split (bps) and the minimum mint
//! amount. The three wallet addresses are IMMUTABLE — set once at
//! `initialize`. To rotate a wallet, deploy a migration ix later.
//!
//! Split bps MUST sum to 10_000; rejected otherwise.
//!
//! TODO(L1): full handler implementation.

use arlex_lang::prelude::*;

use crate::constants::EARN_CONFIG_SEED;

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,
}

pub fn handler(
    _ctx: Context<UpdateConfig>,
    _split_rwa_bps: u16,
    _split_liquidity_bps: u16,
    _split_treasury_bps: u16,
    _min_mint_amount: u64,
) -> Result<()> {
    // TODO(L1):
    // 1. Load EarnConfig
    // 2. Check signer == config.authority
    // 3. split sum == 10_000 → InvalidSplitBps
    // 4. Write fields
    // 5. Emit EarnConfigUpdated
    Ok(())
}
