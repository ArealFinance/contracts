//! `writedown_capital` — authority-only NAV writedown.
//!
//! Decreases `total_invested_capital` by `amount`, lowering NAV
//! accordingly. Used to recognize real-world depreciation / amortization
//! of underlying RWA holdings (off-chain valuation feed).
//!
//! No USDC moves on chain — this is a purely accounting operation.
//! Implicitly RWA wallet's off-chain custody now holds less value; the
//! on-chain capital counter reflects that.
//!
//! Guard: writedown cannot reduce capital below `MIN_CAPITAL_FLOOR` to
//! preserve NAV > 0 invariant.
//!
//! TODO(L1): full handler implementation.

use arlex_lang::prelude::*;

use crate::constants::EARN_CONFIG_SEED;

#[derive(Accounts)]
pub struct WritedownCapital<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,
}

pub fn handler(
    _ctx: Context<WritedownCapital>,
    _amount: u64,
    _reason_code: u8,
) -> Result<()> {
    // TODO(L1):
    // 1. Load EarnConfig
    // 2. Check signer == config.authority
    // 3. amount > 0
    // 4. NAV before snapshot
    // 5. new_capital = config.total_invested_capital.checked_sub(amount as u128)
    //    - if new_capital < MIN_CAPITAL_FLOOR → InsufficientCapital
    // 6. config.total_invested_capital = new_capital
    // 7. NAV after snapshot
    // 8. Emit CapitalWrittenDown
    Ok(())
}
