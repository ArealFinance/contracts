use arlex_lang::prelude::*;

// =============================================================================
// DexConfig — 175 bytes (8 discriminator + 167 data)
// PDA Seed: ["dex_config"] (singleton)
//
// NOTE: Option<Pubkey> → [u8;32]+bool for repr(C,packed) compatibility.
// =============================================================================

#[account]
pub struct DexConfig {
    pub authority: [u8; 32],                // 32
    pub pending_authority: [u8; 32],        // 32 (zeroed = no pending)
    pub has_pending: bool,                  // 1
    pub pause_authority: [u8; 32],          // 32 (immutable)
    pub base_fee_bps: u16,                  // 2
    pub lp_fee_share_bps: u16,             // 2
    pub areal_fee_destination: [u8; 32],    // 32 (immutable)
    pub rebalancer: [u8; 32],               // 32
    pub is_active: bool,                    // 1
    pub bump: u8,                           // 1
}
// SIZE = 167, SPACE = 8 + 167 = 175

const _: () = assert!(core::mem::size_of::<DexConfig>() == 167);

// =============================================================================
// PoolState — 220 bytes (8 discriminator + 212 data)
// PDA Seed: ["pool", token_a_mint, token_b_mint]
// =============================================================================

#[account]
pub struct PoolState {
    pub pool_type: u8,                          // 1 (0=StandardCurve, 1=Concentrated)
    pub token_a_mint: [u8; 32],                 // 32
    pub token_b_mint: [u8; 32],                 // 32
    pub vault_a: [u8; 32],                      // 32
    pub vault_b: [u8; 32],                      // 32
    pub reserve_a: u64,                         // 8
    pub reserve_b: u64,                         // 8
    pub total_lp_shares: u128,                  // 16
    pub fee_bps: u16,                           // 2 (immutable after creation)
    pub is_active: bool,                        // 1
    pub total_fees_accumulated: u64,            // 8
    pub bin_step_bps: u16,                      // 2 (0 for StandardCurve)
    pub active_bin_id: i32,                     // 4 (0 for StandardCurve)
    pub ot_treasury_fee_destination: [u8; 32],  // 32 (zeroed = no OT fee)
    pub has_ot_treasury: bool,                  // 1 (Option pattern)
    pub bump: u8,                               // 1
}
// SIZE = 212, SPACE = 8 + 212 = 220

const _: () = assert!(core::mem::size_of::<PoolState>() == 212);

// =============================================================================
// PoolCreators — 362 bytes (8 discriminator + 354 data)
// PDA Seed: ["pool_creators"] (singleton)
// =============================================================================

#[account]
pub struct PoolCreators {
    pub authority: [u8; 32],                // 32
    pub creators: [[u8; 32]; 10],           // 320
    pub active_count: u8,                   // 1
    pub bump: u8,                           // 1
}
// SIZE = 354, SPACE = 8 + 354 = 362

const _: () = assert!(core::mem::size_of::<PoolCreators>() == 354);

// =============================================================================
// LpPosition — 97 bytes (8 discriminator + 89 data)
// PDA Seed: ["lp", pool_state, provider]
// =============================================================================

#[account]
pub struct LpPosition {
    pub pool: [u8; 32],                     // 32
    pub owner: [u8; 32],                    // 32
    pub shares: u128,                       // 16
    pub last_update_ts: i64,                // 8
    pub bump: u8,                           // 1
}
// SIZE = 89, SPACE = 8 + 89 = 97

const _: () = assert!(core::mem::size_of::<LpPosition>() == 89);
