use arlex_lang::prelude::*;

#[event]
pub struct DexInitialized {
    pub authority: [u8; 32],
    pub base_fee_bps: u16,
    pub timestamp: i64,
}

#[event]
pub struct PoolCreated {
    pub pool: [u8; 32],
    pub token_a_mint: [u8; 32],
    pub token_b_mint: [u8; 32],
    pub pool_type: u8,
    pub creator: [u8; 32],
    pub ot_treasury_fee_destination: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct LiquidityAdded {
    pub pool: [u8; 32],
    pub provider: [u8; 32],
    pub amount_a: u64,
    pub amount_b: u64,
    pub shares_minted: u128,
    pub timestamp: i64,
}

#[event]
pub struct ZapLiquidityExecuted {
    pub pool: [u8; 32],
    pub provider: [u8; 32],
    pub input_a: u64,
    pub input_b: u64,
    pub swapped_amount: u64,
    pub shares_minted: u128,
    pub timestamp: i64,
}

#[event]
pub struct LiquidityRemoved {
    pub pool: [u8; 32],
    pub provider: [u8; 32],
    pub amount_a: u64,
    pub amount_b: u64,
    pub shares_burned: u128,
    pub timestamp: i64,
}

#[event]
pub struct SwapExecuted {
    pub pool: [u8; 32],
    pub user: [u8; 32],
    pub a_to_b: bool,
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee_lp: u64,
    pub fee_protocol: u64,
    pub fee_ot_treasury: u64,
    pub timestamp: i64,
}

#[event]
pub struct PoolCreatorsUpdated {
    pub wallet: [u8; 32],
    pub action: u8,
    pub active_count: u8,
    pub timestamp: i64,
}

#[event]
pub struct DexConfigUpdated {
    pub base_fee_bps: u16,
    pub lp_fee_share_bps: u16,
    pub rebalancer: [u8; 32],
    pub is_active: bool,
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
pub struct PoolPaused {
    pub pool: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct PoolUnpaused {
    pub pool: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct LiquidityShifted {
    pub pool: [u8; 32],
    pub rebalancer: [u8; 32],
    pub old_lower: i32,
    pub old_upper: i32,
    pub new_lower: i32,
    pub new_upper: i32,
    pub timestamp: i64,
}
