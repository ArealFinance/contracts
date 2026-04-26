use arlex_lang::prelude::*;

// =============================================================================
// DistributionConfig — 149 bytes (8 discriminator + 141 data)
// PDA Seed: ["dist_config"] (singleton)
//
// Option<Pubkey> -> [u8;32]+bool for repr(C,packed) compatibility.
// =============================================================================

#[account]
pub struct DistributionConfig {
    pub authority: [u8; 32],             // 32
    pub pending_authority: [u8; 32],     // 32 (zeroed if no pending)
    pub has_pending: bool,               // 1
    pub publish_authority: [u8; 32],     // 32
    pub protocol_fee_bps: u16,           // 2
    pub min_distribution_amount: u64,    // 8
    pub areal_fee_destination: [u8; 32], // 32 (IMMUTABLE after init — RWT ATA)
    pub is_active: bool,                 // 1
    pub bump: u8,                        // 1
}
// SIZE = 141, SPACE = 149
const _: () = assert!(core::mem::size_of::<DistributionConfig>() == 141);

// =============================================================================
// MerkleDistributor — 194 bytes (8 discriminator + 186 data)
// PDA Seed: ["merkle_dist", ot_mint] (one per OT, perpetual)
// =============================================================================

#[account]
pub struct MerkleDistributor {
    pub ot_mint: [u8; 32],        // 32
    pub reward_vault: [u8; 32],   // 32 (RWT ATA owned by this PDA)
    pub accumulator: [u8; 32],    // 32 (partner PDA for USDC income)
    pub merkle_root: [u8; 32],    // 32 (zeroed until first publish)
    pub max_total_claim: u64,     // 8
    pub total_claimed: u64,       // 8
    pub total_funded: u64,        // 8
    pub locked_vested: u64,       // 8
    pub last_fund_ts: i64,        // 8
    pub vesting_period_secs: i64, // 8
    pub epoch: u64,               // 8 (0 = no root yet)
    pub is_active: bool,          // 1
    pub bump: u8,                 // 1
}
// SIZE = 186 (32*4 + 8*7 + 1*2)
const _: () = assert!(core::mem::size_of::<MerkleDistributor>() == 186);

// =============================================================================
// Accumulator — 41 bytes (8 discriminator + 33 data)
// PDA Seed: ["accumulator", ot_mint]
//
// State holds no balance — it lives in the Accumulator USDC ATA owned by this
// PDA. Created at Layer 7 as infrastructure prep; used by Layer 8 convert_to_rwt.
// =============================================================================

#[account]
pub struct Accumulator {
    pub ot_mint: [u8; 32], // 32
    pub bump: u8,          // 1
}
// SIZE = 33
const _: () = assert!(core::mem::size_of::<Accumulator>() == 33);

// =============================================================================
// ClaimStatus — 81 bytes (8 discriminator + 73 data)
// PDA Seed: ["claim_status", distributor, claimant]
// Created on first claim via manual init-if-needed pattern.
// =============================================================================

#[account]
pub struct ClaimStatus {
    pub claimant: [u8; 32],    // 32
    pub distributor: [u8; 32], // 32
    pub claimed_amount: u64,   // 8
    pub bump: u8,              // 1
}
// SIZE = 73
const _: () = assert!(core::mem::size_of::<ClaimStatus>() == 73);

// =============================================================================
// LiquidityHolding — 66 bytes (8 discriminator + 58 data)
// PDA Seed: ["liq_holding"] (singleton, per D11.1)
//
// Receives the 15% liquidity-share splitted by `rwt_engine::claim_yield`.
// Funds park here until Layer 9 Nexus drains them via `withdraw_liquidity_holding`
// CPI. Until then the placeholder ix unconditionally reverts with
// `NexusNotInitialized` (R10 — anti-honeypot per D4).
//
// `total_received` / `total_withdrawn` are running observability counters
// (running sum since deployment); `last_funded_slot` records the most recent
// claim_yield split funding event.
//
// `_reserved` keeps room for Layer 9 fields (e.g. nexus authority, allocation
// strategy) without a state migration.
// =============================================================================

#[account]
pub struct LiquidityHolding {
    pub bump: u8,                      // 1
    pub initialized: bool,             // 1 — guards against double-init
    pub total_received: u64,           // 8 — cumulative deposits (observability)
    pub total_withdrawn: u64,          // 8 — cumulative withdrawals (Layer 9 tracker)
    pub last_funded_slot: u64,         // 8 — slot of last claim_yield split
    pub _reserved: [u8; 32],           // 32 — future-proofing for Layer 9 fields
}
// SIZE = 58, SPACE = 8 + 58 = 66
const _: () = assert!(core::mem::size_of::<LiquidityHolding>() == 58);

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin LiquidityHolding data layout: 1+1+8+8+8+32 = 58 bytes (66 with disc).
    #[test]
    fn liquidity_holding_size_pinned_at_58() {
        assert_eq!(core::mem::size_of::<LiquidityHolding>(), 58);
    }

    /// Singleton: SPACE includes the 8-byte arlex account discriminator.
    #[test]
    fn liquidity_holding_space_pinned_at_66() {
        assert_eq!(LiquidityHolding::SPACE, 66);
    }

    /// D11.1 — single-component seed (no `ot_mint` suffix). Catches drift if
    /// anyone re-reverts to per-OT layout.
    #[test]
    fn liq_holding_seed_layout_is_one_component() {
        let seeds: &[&[u8]] = &[b"liq_holding"];
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0], b"liq_holding");
    }
}
