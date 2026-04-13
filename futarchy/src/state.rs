use arlex_lang::prelude::*;

// Proposal type constants (stored as u8 in packed struct)
pub const PROPOSAL_TYPE_MINT_OT: u8 = 0;
pub const PROPOSAL_TYPE_SPEND_TREASURY: u8 = 1;
pub const PROPOSAL_TYPE_UPDATE_DESTINATIONS: u8 = 2;

// Proposal status constants
pub const STATUS_ACTIVE: u8 = 0;
pub const STATUS_APPROVED: u8 = 1;
pub const STATUS_EXECUTED: u8 = 2;
pub const STATUS_CANCELLED: u8 = 3;

// FutarchyConfig — 115 bytes (8 discriminator + 107 data)
// PDA Seed: ["futarchy_config", ot_mint]
#[account]
pub struct FutarchyConfig {
    pub ot_mint: [u8; 32],              // 32
    pub authority: [u8; 32],            // 32
    pub pending_authority: [u8; 32],    // 32 (zeroed = no pending)
    pub has_pending: bool,              // 1
    pub next_proposal_id: u64,          // 8
    pub is_active: bool,                // 1
    pub bump: u8,                       // 1
}
// SIZE = 107, SPACE = 8 + 107 = 115

// Proposal — 203 bytes (8 discriminator + 195 data)
// PDA Seed: ["proposal", futarchy_config, proposal_id.to_le_bytes()]
#[account]
pub struct Proposal {
    pub proposal_id: u64,               // 8
    pub ot_mint: [u8; 32],              // 32
    pub proposer: [u8; 32],             // 32
    pub proposal_type: u8,              // 1
    pub amount: u64,                    // 8
    pub destination: [u8; 32],          // 32
    pub token_mint: [u8; 32],           // 32
    pub params_hash: [u8; 32],          // 32
    pub status: u8,                     // 1
    pub created_ts: i64,                // 8
    pub executed_ts: i64,               // 8
    pub bump: u8,                       // 1
}
// SIZE = 195, SPACE = 8 + 195 = 203
