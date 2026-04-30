use arlex_lang::prelude::*;

// =============================================================================
// RwtVault — 267 bytes (8 discriminator + 259 data)
// PDA Seed: ["rwt_vault"] (singleton)
//
// NOTE: Option<Pubkey> → [u8;32]+bool for repr(C,packed) compatibility.
// Arlex #[account] uses repr(C,packed) which doesn't support Option<T>.
// =============================================================================

#[account]
pub struct RwtVault {
    pub total_invested_capital: u128,       // 16 — first u128 field in the project
    pub total_rwt_supply: u64,              // 8
    pub nav_book_value: u64,                // 8
    pub capital_accumulator_ata: [u8; 32],  // 32
    pub rwt_mint: [u8; 32],                 // 32
    pub authority: [u8; 32],                // 32
    pub pending_authority: [u8; 32],        // 32 (zeroed = no pending)
    pub has_pending: bool,                  // 1
    pub manager: [u8; 32],                  // 32
    pub pause_authority: [u8; 32],          // 32 (immutable after init)
    pub mint_paused: bool,                  // 1
    pub areal_fee_destination: [u8; 32],    // 32 (immutable after init)
    pub bump: u8,                           // 1
}
// SIZE = 259, SPACE = 8 + 259 = 267

// Compile-time size assertion
const _: () = assert!(core::mem::size_of::<RwtVault>() == 259);

// =============================================================================
// RwtDistributionConfig — 79 bytes (8 discriminator + 71 data)
// PDA Seed: ["dist_config_rwt"] (singleton)
// =============================================================================

#[account]
pub struct RwtDistributionConfig {
    pub book_value_bps: u16,                    // 2
    pub liquidity_bps: u16,                     // 2
    pub protocol_revenue_bps: u16,              // 2
    pub liquidity_destination: [u8; 32],        // 32
    pub protocol_revenue_destination: [u8; 32], // 32
    pub bump: u8,                               // 1
}
// SIZE = 71, SPACE = 8 + 71 = 79

// Compile-time size assertion
const _: () = assert!(core::mem::size_of::<RwtDistributionConfig>() == 71);
