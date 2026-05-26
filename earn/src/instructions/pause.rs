//! `pause` / `unpause` — pause-authority-only emergency stop for mint flow.
//!
//! `stream_inflow`, `writedown_capital`, `update_config` are NOT gated by
//! pause (admin must remain able to write down impaired capital or top up
//! liquidity even during a pause).
//!
//! TODO(L1): full handler implementation.

use arlex_lang::prelude::*;

use crate::constants::EARN_CONFIG_SEED;

#[derive(Accounts)]
pub struct PauseEarn<'info> {
    #[account(signer)]
    pub pause_authority: &'info AccountView,

    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,
}

#[derive(Accounts)]
pub struct UnpauseEarn<'info> {
    #[account(signer)]
    pub pause_authority: &'info AccountView,

    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,
}

pub fn pause_handler(_ctx: Context<PauseEarn>) -> Result<()> {
    // TODO(L1):
    // 1. Load EarnConfig
    // 2. Check signer == config.pause_authority
    // 3. config.is_paused = true (idempotent)
    // 4. Emit EarnPauseToggled { is_paused: true }
    Ok(())
}

pub fn unpause_handler(_ctx: Context<UnpauseEarn>) -> Result<()> {
    // TODO(L1):
    // 1. Load EarnConfig
    // 2. Check signer == config.pause_authority
    // 3. config.is_paused = false (idempotent)
    // 4. Emit EarnPauseToggled { is_paused: false }
    Ok(())
}
