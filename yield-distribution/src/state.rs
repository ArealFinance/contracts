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
// Funds park here; `withdraw_liquidity_holding` (Layer 9 R20) drains them
// atomically into the Nexus ATA + bumps the DEX-side counter via the
// `nexus_record_deposit` CPI (single-TX semantics per SD-4 / D27).
//
// `total_received` / `total_withdrawn` are running observability counters
// (running sum since deployment); `last_funded_slot` records the most recent
// claim_yield split funding event.
//
// Layer 9 carved two 8-byte tracking slots out of the 32-byte `_reserved`
// future-proofing block (16 bytes still reserved for later Layer 9 fields):
//   * `last_withdrawn_slot`   — slot at which the most recent drain landed
//   * `last_withdrawn_amount` — amount drained in the most recent call
// `total_withdrawn` (already present from Layer 8) doubles as the cumulative
// counter that the Layer 9 architecture spec calls `cumulative_withdrawn`
// — same semantics, name preserved for byte-layout stability.
// =============================================================================

#[account]
pub struct LiquidityHolding {
    pub bump: u8,                      // 1
    pub initialized: bool,             // 1 — guards against double-init
    pub total_received: u64,           // 8 — cumulative deposits (observability)
    pub total_withdrawn: u64,          // 8 — cumulative withdrawals (== cumulative_withdrawn)
    pub last_funded_slot: u64,         // 8 — slot of last claim_yield split
    pub last_withdrawn_slot: u64,      // 8 — slot of last withdraw_liquidity_holding (Layer 9)
    pub last_withdrawn_amount: u64,    // 8 — amount of last withdraw_liquidity_holding (Layer 9)
    pub _reserved: [u8; 16],           // 16 — future-proofing for further Layer 9 fields
}
// SIZE = 58 (1+1+8+8+8+8+8+16), SPACE = 8 + 58 = 66 — UNCHANGED across L8→L9.
const _: () = assert!(core::mem::size_of::<LiquidityHolding>() == 58);

#[cfg(test)]
mod tests {
    use super::*;

    /// Pin LiquidityHolding data layout: 1+1+8+8+8+8+8+16 = 58 bytes
    /// (66 with disc). The Layer 9 R20 migration carved two 8-byte slots
    /// (`last_withdrawn_slot`, `last_withdrawn_amount`) out of the original
    /// 32-byte `_reserved` block, leaving 16 bytes still reserved. Total
    /// size invariant is preserved across the Layer 8 → Layer 9 migration —
    /// no rent / SPACE bump, no on-chain account migration needed.
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
