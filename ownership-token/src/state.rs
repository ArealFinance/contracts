use arlex_lang::prelude::*;
use arlex_lang::ArgsDeserialize;
use crate::constants::MAX_DESTINATIONS;

// =============================================================================
// Embedded struct (NOT a separate PDA)
// =============================================================================

#[derive(Copy, Clone)]
#[repr(C, packed)]
pub struct RevenueDestination {
    pub address: [u8; 32],
    pub allocation_bps: u16,
    pub label: [u8; 32],
}

// Compile-time size assertion
const _: () = assert!(core::mem::size_of::<RevenueDestination>() == 66);

impl RevenueDestination {
    pub const SIZE: usize = 66;

    pub fn zeroed() -> Self {
        Self {
            address: [0u8; 32],
            allocation_bps: 0,
            label: [0u8; 32],
        }
    }
}

// Instruction argument for batch_update_destinations
pub struct BatchDestination {
    pub address: [u8; 32],
    pub allocation_bps: u16,
    pub label: [u8; 32],
}

impl ArgsDeserialize for BatchDestination {
    fn deserialize(data: &mut &[u8]) -> core::result::Result<Self, ProgramError> {
        Ok(BatchDestination {
            address: <[u8; 32]>::deserialize(data)?,
            allocation_bps: u16::deserialize(data)?,
            label: <[u8; 32]>::deserialize(data)?,
        })
    }
}

// =============================================================================
// State Accounts
// =============================================================================

// OtConfig — 292 bytes (8 discriminator + 284 data)
// PDA Seed: ["ot_config", ot_mint]
#[account]
pub struct OtConfig {
    pub ot_mint: [u8; 32],       // 32
    pub name: [u8; 32],          // 32
    pub symbol: [u8; 10],        // 10
    pub decimals: u8,            // 1
    pub total_minted: u64,       // 8
    pub uri: [u8; 200],          // 200
    pub bump: u8,                // 1
}
// SIZE = 284, SPACE = 8 + 284 = 292

// RevenueAccount — 74 bytes (8 discriminator + 66 data)
// PDA Seed: ["revenue", ot_mint]
#[account]
pub struct RevenueAccount {
    pub ot_mint: [u8; 32],              // 32
    pub total_distributed: u64,          // 8
    pub distribution_count: u64,         // 8
    pub last_distribution_ts: i64,       // 8
    pub min_distribution_amount: u64,    // 8
    pub is_distributing: bool,           // 1
    pub bump: u8,                        // 1
}
// SIZE = 66, SPACE = 8 + 66 = 74

// RevenueConfig — 742 bytes (8 discriminator + 734 data)
// PDA Seed: ["revenue_config", ot_mint]
// NOTE: has_one CANNOT be used for areal_fee_destination because
// RevenueDestination is a custom struct that breaks PUBKEY_FIELD_OFFSETS.
// Manual validation required in handlers.
#[account]
pub struct RevenueConfig {
    pub ot_mint: [u8; 32],                              // 32
    pub destinations: [RevenueDestination; MAX_DESTINATIONS], // 660 (66 * 10)
    pub active_count: u8,                                // 1
    pub config_version: u64,                             // 8
    pub areal_fee_destination: [u8; 32],                 // 32
    pub bump: u8,                                        // 1
}
// SIZE = 734, SPACE = 8 + 734 = 742

// OtGovernance — 107 bytes (8 discriminator + 99 data)
// PDA Seed: ["ot_governance", ot_mint]
// NOTE: Option<[u8;32]> replaced with [u8;32] + bool for repr(C, packed) compat.
// Arlex #[account] type_size() doesn't handle Option, breaking PUBKEY_FIELD_OFFSETS.
#[account]
pub struct OtGovernance {
    pub ot_mint: [u8; 32],               // 32
    pub authority: [u8; 32],             // 32
    pub pending_authority: [u8; 32],     // 32 (zeroed = no pending)
    pub has_pending: bool,               // 1
    pub is_active: bool,                 // 1
    pub bump: u8,                        // 1
}
// SIZE = 99, SPACE = 8 + 99 = 107

// OtTreasury — 41 bytes (8 discriminator + 33 data)
// PDA Seed: ["ot_treasury", ot_mint]
#[account]
pub struct OtTreasury {
    pub ot_mint: [u8; 32],   // 32
    pub bump: u8,             // 1
}
// SIZE = 33, SPACE = 8 + 33 = 41
