use arlex_lang::prelude::*;

/// `RwtMinted.source` discriminator. Distinguishes a retail user mint from
/// the off-chain executor's buyback-mint (earn-layers §"buyback vs mint").
/// Lets the backend separate organic demand from protocol reinvestment.
pub const MINT_SOURCE_USER: u8 = 0;
#[allow(dead_code)]
pub const MINT_SOURCE_EXECUTOR: u8 = 1;

/// Emitted once on `initialize`.
#[event]
pub struct EarnInitialized {
    pub authority: [u8; 32],
    pub rwt_mint: [u8; 32],
    pub basket_vault: [u8; 32],
    pub dao_fee_destination: [u8; 32],
    pub initial_nav: u64,
    pub timestamp: i64,
}

/// Emitted on every successful mint. Includes the post-mint NAV so off-chain
/// indexers can reconstruct the NAV history without polling.
#[event]
pub struct RwtMinted {
    pub minter: [u8; 32],
    pub deposit_body: u64,          // basket body (→ basket_vault, += capital)
    pub fee: u64,                   // 1% commission (→ dao_fee_destination)
    pub rwt_out: u64,               // RWT minted to user
    pub nav_after: u64,             // NAV after mint (== NAV before — invariant)
    pub source: u8,                 // MINT_SOURCE_USER / MINT_SOURCE_EXECUTOR
    pub timestamp: i64,
}

/// Emitted on each `add_to_basket` — authority/off-chain executor deposits
/// USDC into the basket. NAV rises for all holders (no RWT minted).
#[event]
pub struct BasketGrew {
    pub amount: u64,                // USDC added to basket_vault
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
    pub mint_fee_bps: u16,
    pub min_mint_amount: u64,
    pub dao_fee_destination: [u8; 32],
    pub timestamp: i64,
}

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
