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
