use arlex_lang::prelude::*;

/// Emitted once on `initialize`.
#[event]
pub struct EarnInitialized {
    pub authority: [u8; 32],
    pub rwt_mint: [u8; 32],
    pub initial_nav: u64,
    pub split_rwa_bps: u16,
    pub split_liquidity_bps: u16,
    pub split_treasury_bps: u16,
    pub timestamp: i64,
}

/// Emitted on every successful user mint. Includes the post-mint NAV so
/// off-chain indexers can reconstruct the NAV history without polling.
#[event]
pub struct RwtMinted {
    pub user: [u8; 32],
    pub deposit_amount: u64,        // user's USDC input
    pub rwa_amount: u64,            // routed to rwa_wallet
    pub liquidity_amount: u64,      // routed to liquidity_wallet
    pub treasury_amount: u64,       // routed to treasury_wallet
    pub rwt_out: u64,               // RWT minted to user
    pub nav_before: u64,
    pub nav_after: u64,
    pub timestamp: i64,
}

/// Emitted on each `stream_inflow` — authority-driven USDC top-up that
/// bumps NAV without minting RWT.
#[event]
pub struct StreamReceived {
    pub source: [u8; 32],           // authority signer
    pub amount: u64,                // USDC added to liquidity_wallet
    pub nav_before: u64,
    pub nav_after: u64,
    pub timestamp: i64,
}

/// Emitted on `writedown_capital` — authority decreases NAV to reflect
/// real-world underlying depreciation.
#[event]
pub struct CapitalWrittenDown {
    pub amount: u64,
    pub nav_before: u64,
    pub nav_after: u64,
    pub reason_code: u8,            // free-form authority hint; 0 = unspecified
    pub timestamp: i64,
}

/// Emitted by `pause` and `unpause`.
#[event]
pub struct EarnPauseToggled {
    pub is_paused: bool,
    pub timestamp: i64,
}

/// Emitted on `update_config`.
#[event]
pub struct EarnConfigUpdated {
    pub split_rwa_bps: u16,
    pub split_liquidity_bps: u16,
    pub split_treasury_bps: u16,
    pub min_mint_amount: u64,
    pub timestamp: i64,
}
