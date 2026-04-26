use arlex_lang::prelude::*;

#[event]
pub struct VaultInitialized {
    pub authority: [u8; 32],
    pub rwt_mint: [u8; 32],
    pub nav: u64,
    pub timestamp: i64,
}

#[event]
pub struct RwtMinted {
    pub user: [u8; 32],
    pub deposit_amount: u64,
    pub rwt_amount: u64,
    pub fee_vault: u64,
    pub fee_dao: u64,
    pub nav_after: u64,
    pub is_admin: bool,
    pub timestamp: i64,
}

#[event]
pub struct CapitalAdjusted {
    pub old_capital: u128,
    pub new_capital: u128,
    pub writedown_amount: u64,
    pub old_nav: u64,
    pub new_nav: u64,
    pub timestamp: i64,
}

#[event]
pub struct VaultManagerUpdated {
    pub old_manager: [u8; 32],
    pub new_manager: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct DistributionConfigUpdated {
    pub book_value_bps: u16,
    pub liquidity_bps: u16,
    pub protocol_revenue_bps: u16,
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

#[event]
pub struct MintPauseToggled {
    pub paused: bool,
    pub timestamp: i64,
}

#[event]
pub struct VaultSwapExecuted {
    pub token_in_mint: [u8; 32],
    pub token_out_mint: [u8; 32],
    pub amount_in: u64,
    pub amount_out: u64,
    pub timestamp: i64,
}

// ----- Layer 8 (claim_yield) -----

/// Emitted by `claim_yield` after a successful YD claim + 70/15/15 split.
/// Layer 8 architecture §6.2. Total body: 32 + 32 + 8*7 = 120 bytes.
#[event]
pub struct YieldDistributed {
    pub vault: [u8; 32],                 // RwtVault PDA (singleton)
    pub ot_mint: [u8; 32],               // OT mint of source distributor
    pub total_yield: u64,                // claimed RWT total
    pub book_value_share: u64,           // 70% — stays in vault claim ATA (NAV backing)
    pub liquidity_share: u64,            // 15% — transferred to liquidity_dest
    pub protocol_revenue_share: u64,     // 15% — transferred to protocol_revenue_dest
    pub nav_before: u64,
    pub nav_after: u64,
    pub timestamp: i64,
}

/// Emitted by `claim_yield` after the liquidity_share transfer to the
/// `LiquidityHolding` PDA RWT ATA (per D11.1, singleton). Layer 8
/// architecture §6.5 (D13). Total body: 32 + 32 + 8 + 8 = 80 bytes.
#[event]
pub struct LiquidityHoldingFunded {
    pub liquidity_holding: [u8; 32],     // == dist_config.liquidity_destination
    pub ot_mint: [u8; 32],               // OT context (source distributor)
    pub amount: u64,                     // liquidity_share transferred
    pub timestamp: i64,
}
