//! Events for the `staking` (stRWT) program.
//!
//! Field sets follow `docs/contracts/staking.mdx` (Events table). Chosen so
//! the backend can reconstruct the rate history (for APY) and index positions
//! without polling. `rate_after` is the canonical rate snapshot:
//!   rate_after = (active_after + V_ASSETS) × RATE_SCALE / (strwt_supply_after + V_SHARES)

use arlex_lang::prelude::*;

/// Emitted once on `initialize`.
#[event]
pub struct StakingInitialized {
    pub authority: [u8; 32],
    pub rwt_mint: [u8; 32],
    pub strwt_mint: [u8; 32],
    pub pool_vault: [u8; 32],
    pub cooldown_seconds: i64,
    pub timestamp: i64,
}

/// Emitted on every successful `stake`.
#[event]
pub struct Staked {
    pub user: [u8; 32],
    pub rwt_in: u64,
    pub strwt_out: u64,
    pub rate_after: u64,
    pub active_after: u64,
    pub strwt_supply_after: u64,
    pub timestamp: i64,
}

/// Emitted on `deposit_rewards` (reward_depositor only). No stRWT minted.
#[event]
pub struct RewardsDeposited {
    pub depositor: [u8; 32],
    pub rwt_in: u64,
    pub rate_after: u64,
    pub active_after: u64,
    pub timestamp: i64,
}

/// Emitted on `initiate_unstake` — stRWT burned, RWT moved active → reserved.
#[event]
pub struct UnstakeInitiated {
    pub user: [u8; 32],
    pub strwt_burned: u64,
    pub rwt_out: u64,
    pub unlock_ts: i64,
    pub nonce: u64,
    pub active_after: u64,
    pub reserved_after: u64,
    pub timestamp: i64,
}

/// Emitted on `complete_unstake` — reserved RWT transferred out, ticket closed.
#[event]
pub struct UnstakeCompleted {
    pub user: [u8; 32],
    pub rwt_out: u64,
    pub nonce: u64,
    pub reserved_after: u64,
    pub timestamp: i64,
}

/// Emitted by `pause` / `unpause`.
#[event]
pub struct StakingPauseToggled {
    pub is_paused: bool,
    pub timestamp: i64,
}

/// Emitted on `update_config`.
#[event]
pub struct StakingConfigUpdated {
    pub reward_depositor: [u8; 32],
    pub min_stake_amount: u64,
    pub cooldown_seconds: i64,
    pub timestamp: i64,
}

/// 2-step authority rotation events (mirror rwt-engine).
#[event]
pub struct AuthorityTransferProposed {
    pub current_authority: [u8; 32],
    pub pending_authority: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct AuthorityTransferAccepted {
    pub old_authority: [u8; 32],
    pub new_authority: [u8; 32],
    pub timestamp: i64,
}
