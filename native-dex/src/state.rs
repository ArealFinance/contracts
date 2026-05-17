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
// PoolState — 272 bytes (8 discriminator + 264 data)
// PDA Seed: ["pool", token_a_mint, token_b_mint]
//
// Fields `cumulative_fees_per_share_{a,b}` were added by Layer 9 D28
// (LP-fee accumulator infrastructure). Pre-Layer-9 contracts on devnet/mainnet
// were never `initialize_dex`-ed (state uninitialized per memory
// `project_layer1to8_complete.md`), so no on-chain migration is required —
// every `PoolState` account is created post-D28 with the new fields.
//
// CP-1 Monotonic Ladder rewrite (docs/changelog/2026-04-17-monotonic-ladder.mdx
// §99-102) appends five Monotonic Ladder anchor fields + 2-byte explicit
// padding for +20 bytes total. StandardCurve pools (`pool_type != 1`) leave
// these fields at zero (the sentinel), and no Monotonic Ladder code path
// reads them outside `pool_type == POOL_TYPE_CONCENTRATED`. There is no
// on-chain migration: master pools are not yet live on mainnet.
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
    /// Layer 9 D28 — cumulative LP-fee per share, side A (Q64.64 fixed-point).
    /// Per swap on side A, accumulator updates as
    /// `cumulative_fees_per_share_a += (fee_lp << 64) / total_lp_shares`.
    /// LpPosition tracks delta against `fees_claimed_per_share_a` to compute
    /// the claimable amount on `claim_lp_fees`.
    pub cumulative_fees_per_share_a: u128,      // 16
    /// Layer 9 D28 — cumulative LP-fee per share, side B (Q64.64 fixed-point).
    pub cumulative_fees_per_share_b: u128,      // 16
    // ----- CP-1 Monotonic Ladder anchors (concentrated pools only) -----
    //
    // All five anchors are zero-default for StandardCurve pools. The new
    // `compress_liquidity` / `grow_liquidity` handlers (CP-4/CP-5) initialise
    // them at concentrated-pool creation. The legacy `shift_liquidity`
    // instruction was removed in CP-2.
    /// Leftmost bin currently bracketed by the Monotonic Ladder (active
    /// extended bid edge). Initialised by `create_concentrated_pool`,
    /// advanced by `compress_liquidity`.
    pub left_anchor_bin: i32,                   // 4
    /// Upper edge of the permanent-tail USDC reserve. Frozen for the
    /// lifetime of the pool — never advanced by Rebalancer (docs §51).
    pub permanent_tail_floor_bin: i32,          // 4
    /// NAV bin at the time of the most recent rebalance. Used by
    /// `grow_liquidity` to assert monotonicity of growth.
    pub last_rebalance_nav_bin: i32,            // 4
    /// Lower edge of the dense active bid zone (`ACTIVE_ZONE_WIDTH` bins
    /// below `active_bin_id`).
    pub active_zone_lower: i32,                 // 4
    /// Configured permanent-tail offset below initial NAV, in bps. Bounded by
    /// `[MIN_PERMANENT_TAIL_OFFSET_BPS, DEFAULT_PERMANENT_TAIL_OFFSET_BPS]`.
    pub permanent_tail_offset_bps: u16,         // 2
    /// Explicit padding to keep the appended block at +20 B and leave a
    /// well-defined slot for future Monotonic Ladder anchors without
    /// requiring another size assert reflow.
    pub _pad_monotonic: [u8; 2],                // 2
}
// SIZE = 264, SPACE = 8 + 264 = 272

const _: () = assert!(core::mem::size_of::<PoolState>() == 264);

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
// LpPosition — 129 bytes (8 discriminator + 121 data)
// PDA Seed: ["lp", pool_state, provider]
//
// Fields `fees_claimed_per_share_{a,b}` were added by Layer 9 D28 alongside
// `PoolState.cumulative_fees_per_share_{a,b}`. New positions snapshot the
// pool's current cumulative on init so they cannot retroactively claim fees
// that accrued before they joined. `claim_lp_fees` computes claimable as
// `((pool.cumulative - position.fees_claimed) * shares) >> 64` and then
// updates the snapshot. Pre-Layer-9 state is uninitialized so no migration
// is required (see PoolState header).
// =============================================================================

#[account]
pub struct LpPosition {
    pub pool: [u8; 32],                     // 32
    pub owner: [u8; 32],                    // 32
    pub shares: u128,                       // 16
    pub last_update_ts: i64,                // 8
    pub bump: u8,                           // 1
    /// Layer 9 D28 — snapshot of `PoolState.cumulative_fees_per_share_a` taken
    /// at the most recent `claim_lp_fees` (or at position open). Q64.64.
    pub fees_claimed_per_share_a: u128,     // 16
    /// Layer 9 D28 — snapshot of `PoolState.cumulative_fees_per_share_b`.
    pub fees_claimed_per_share_b: u128,     // 16
}
// SIZE = 121, SPACE = 8 + 121 = 129

const _: () = assert!(core::mem::size_of::<LpPosition>() == 121);

// =============================================================================
// Bin — 16 bytes (liquidity per price bin for concentrated pools)
// =============================================================================

#[derive(Copy, Clone)]
#[repr(C, packed)]
pub struct Bin {
    pub liquidity_a: u64,   // 8 — RWT (ask side above active, both in active)
    pub liquidity_b: u64,   // 8 — USDC (bid side below active, both in active)
}

const _: () = assert!(core::mem::size_of::<Bin>() == 16);

// =============================================================================
// BinArray — 16_051 bytes (8 discriminator + 16_043 data)
// PDA Seed: ["bins", pool_state]
//
// CP-1 Monotonic Ladder rewrite (docs/changelog/2026-04-17-monotonic-ladder.mdx
// §50): MAX_BINS grew 70 → 1000 to host the log-scale ladder. The 16 KB
// account fits comfortably inside the Solana 10 MB account-size ceiling and
// costs ~0.11 SOL of rent per master pool. Concentrated bin-walk swap math
// (concentrated.rs) is bounds-checked against `MAX_BINS` symbolically so it
// scales without code changes.
// =============================================================================

#[account]
pub struct BinArray {
    pub pool: [u8; 32],                          // 32
    pub bins: [Bin; crate::constants::MAX_BINS], // 16_000 (1000 × 16)
    pub lower_bin_id: i32,                       // 4
    pub bin_step_bps: u16,                       // 2
    pub active_bin_id: i32,                      // 4
    pub bump: u8,                                // 1
}
// SIZE = 16_043, SPACE = 8 + 16_043 = 16_051

const _: () = assert!(core::mem::size_of::<BinArray>() == 16_043);

// =============================================================================
// LiquidityNexus — 58 bytes (8 discriminator + 50 data)
// PDA Seed: ["liquidity_nexus"] (singleton, one Nexus per DEX deployment).
//
// Layer 9 §3 — Areal Finance LP-management PDA. Owns token ATAs (USDC + RWT)
// and LpPosition entries (where `owner == nexus.key()`). Manager wallet
// executes swap/add/remove ix; principal counters track cumulative deposits
// and act as on-chain floor for `nexus_withdraw_profits`.
//
// Field naming follows docs canonical (`docs/contracts/native-dex.mdx`
// LiquidityNexus state table, SD-2 / D16). `total_deposited_*` are
// monotonically non-decreasing — they never reflect impairment; impairment
// surfaces via ATA balance vs floor comparison inside `nexus_withdraw_profits`.
//
// `manager == [0u8; 32]` is the documented kill-switch (D22) — `assert_manager`
// helper reverts `NexusManagerDisabled` regardless of which wallet signed.
// =============================================================================

#[account]
pub struct LiquidityNexus {
    pub manager: [u8; 32],                  // 32 — bot wallet, signer for nexus_swap/add/remove.
    // Cumulative USDC deposited via nexus_deposit. Monotonically non-decreasing.
    // Acts as on-chain principal floor for nexus_withdraw_profits — ATA balance
    // minus this counter is the withdrawable profit.
    pub total_deposited_usdc: u64,          // 8
    // Cumulative RWT deposited via nexus_deposit + withdraw_liquidity_holding
    // CPI drain. Monotonically non-decreasing. Same floor semantics as USDC.
    pub total_deposited_rwt: u64,           // 8
    pub is_active: bool,                    // 1 — Nexus kill-switch (initialized = true).
    pub bump: u8,                           // 1 — PDA bump.
}
// SIZE = 50, SPACE = 8 + 50 = 58

const _: () = assert!(core::mem::size_of::<LiquidityNexus>() == 50);

#[cfg(test)]
mod tests {
    use super::*;

    /// SD-2 / SD-15 / D16 — pin LiquidityNexus data layout at 50 bytes
    /// (32 + 8 + 8 + 1 + 1). Catches drift if a field is reordered, resized,
    /// or if `_reserved` slack is added without a state migration plan.
    #[test]
    fn liquidity_nexus_size_is_50_bytes() {
        assert_eq!(core::mem::size_of::<LiquidityNexus>(), 50);
        // SPACE includes the 8-byte arlex account discriminator.
        assert_eq!(LiquidityNexus::SPACE, 58);
    }

    /// Singleton PDA — single-component seed, no per-token suffix. Catches
    /// drift if anyone re-introduces a per-mint Nexus layout.
    #[test]
    fn liquidity_nexus_seed_is_singleton() {
        let seeds: &[&[u8]] = &[crate::constants::LIQUIDITY_NEXUS_SEED];
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0], b"liquidity_nexus");
    }

    /// Default-init layout sanity check: zero-filled bytes mean every counter
    /// starts at 0, manager is the zero pubkey (kill-switch active before
    /// `initialize_nexus`), `is_active` is false. `initialize_nexus` is the
    /// only ix that flips `is_active` to true; this test catches any drift
    /// where a fabricated default would imply an "already-active" state.
    #[test]
    fn liquidity_nexus_default_uninitialized() {
        // SAFETY: LiquidityNexus is `#[repr(C, packed)]` via #[account] and
        // sums to 50 bytes exactly with no padding. All-zero is a valid bit
        // pattern for every field type used here.
        let buf = [0u8; core::mem::size_of::<LiquidityNexus>()];
        let nexus: LiquidityNexus =
            unsafe { core::ptr::read(buf.as_ptr() as *const LiquidityNexus) };
        assert_eq!(nexus.manager, [0u8; 32]);
        assert_eq!({ nexus.total_deposited_usdc }, 0);
        assert_eq!({ nexus.total_deposited_rwt }, 0);
        assert!(!nexus.is_active);
        assert_eq!(nexus.bump, 0);
    }

    // -------------------------------------------------------------------
    // Layer 9 D28 — LP-fee accumulator state extension
    //
    // PoolState gained `cumulative_fees_per_share_{a,b}: u128` (32 bytes
    // total). LpPosition gained `fees_claimed_per_share_{a,b}: u128` (32
    // bytes total). The four tests below pin the new sizes (compile-time
    // asserts already cover them; these add a runtime check + Layer-9
    // migration footprint witness) and confirm zero-default semantics.
    // -------------------------------------------------------------------

    /// D28 + CP-1 — PoolState size includes 32 bytes of LP-fee accumulator
    /// plus 20 bytes of Monotonic Ladder anchors. Pre-D28 size was 212 bytes;
    /// post-D28 was 244 bytes (212 + 16 + 16); post-CP-1 is 264 bytes
    /// (244 + 4 + 4 + 4 + 4 + 2 + 2). Catches drift if any of these field
    /// groups are reordered, resized, or dropped without going through state
    /// migration.
    #[test]
    fn pool_state_size_includes_lp_fee_accumulators() {
        assert_eq!(core::mem::size_of::<PoolState>(), 264);
        assert_eq!(PoolState::SPACE, 272);
    }

    /// CP-1 — explicit witness that the Monotonic Ladder additions are the
    /// only delta from the post-D28 baseline (244 B). Splitting this out
    /// from the umbrella assertion above makes a future review obvious if
    /// anyone tries to land a "side-channel" struct change.
    #[test]
    fn pool_state_size_grew_by_20_bytes() {
        const POST_D28_SIZE: usize = 244;
        const CP1_DELTA: usize = 20; // 4 + 4 + 4 + 4 + 2 + 2
        assert_eq!(core::mem::size_of::<PoolState>(), POST_D28_SIZE + CP1_DELTA);
    }

    /// CP-1 — zero-default sentinel pin: every Monotonic Ladder anchor on a
    /// freshly-zeroed PoolState reads as 0. StandardCurve pools rely on this
    /// (they are never touched by `grow_liquidity` / `compress_liquidity` and
    /// must continue to validate against the zero sentinel without explicit
    /// init). Zero-fill is a valid bit pattern for `i32`, `u16`, and `[u8;2]`.
    #[test]
    fn pool_state_default_zeroes_monotonic_fields() {
        // SAFETY: PoolState is `#[repr(C, packed)]` via #[account] and sums
        // to 264 bytes with no padding. All-zero is a valid bit pattern for
        // every field type used here.
        let buf = [0u8; core::mem::size_of::<PoolState>()];
        let pool: PoolState = unsafe { core::ptr::read(buf.as_ptr() as *const PoolState) };
        assert_eq!({ pool.left_anchor_bin }, 0i32);
        assert_eq!({ pool.permanent_tail_floor_bin }, 0i32);
        assert_eq!({ pool.last_rebalance_nav_bin }, 0i32);
        assert_eq!({ pool.active_zone_lower }, 0i32);
        assert_eq!({ pool.permanent_tail_offset_bps }, 0u16);
        assert_eq!({ pool._pad_monotonic }, [0u8; 2]);
    }

    /// CP-1 — pin BinArray repr-C layout at 16_043 bytes (32 + 1000 × 16 +
    /// 4 + 2 + 4 + 1). Catches drift if MAX_BINS, the Bin struct, or any of
    /// the trailing scalar fields are touched without a state migration plan.
    #[test]
    fn bin_array_size_matches_repr_c() {
        assert_eq!(core::mem::size_of::<BinArray>(), 16_043);
        assert_eq!(BinArray::SPACE, 16_051);
    }

    /// CP-1 — sanity pin for MAX_BINS itself. Bin-walk swap and bin
    /// distribution math everywhere in `concentrated.rs` is bounded by
    /// MAX_BINS symbolically, but having the literal value asserted catches
    /// an accidental edit that compiles but breaks Monotonic Ladder
    /// dimensioning.
    #[test]
    fn bin_array_max_bins_is_1000() {
        assert_eq!(crate::constants::MAX_BINS, 1000);
    }

    /// D28 — newly-created PoolState accounts must default-init both
    /// accumulators to 0. Zero-filled bytes are a valid bit pattern for u128
    /// (per `repr(C, packed)`). Catches any drift where a fabricated default
    /// would imply pre-existing fees that an LpPosition snapshot at 0 could
    /// then "claim".
    #[test]
    fn pool_state_default_lp_fee_accumulators_zero() {
        // SAFETY: PoolState is `#[repr(C, packed)]` via #[account] and sums
        // to 244 bytes with no padding. All-zero is a valid bit pattern for
        // every field type (u8, [u8;32], u64, u128, u16, i32, bool).
        let buf = [0u8; core::mem::size_of::<PoolState>()];
        let pool: PoolState = unsafe { core::ptr::read(buf.as_ptr() as *const PoolState) };
        assert_eq!({ pool.cumulative_fees_per_share_a }, 0u128);
        assert_eq!({ pool.cumulative_fees_per_share_b }, 0u128);
    }

    /// D28 — LpPosition size now includes 32 bytes of per-side claimed-fee
    /// snapshot. Pre-D28 size was 89 bytes; post-D28 size is 121 bytes
    /// (89 + 16 + 16). Catches drift in the snapshot fields.
    #[test]
    fn lp_position_size_includes_fees_claimed_snapshots() {
        assert_eq!(core::mem::size_of::<LpPosition>(), 121);
        assert_eq!(LpPosition::SPACE, 129);
    }

    /// D28 — newly-created LpPosition accounts must default-init the
    /// claimed-fee snapshots to 0. Note the canonical Layer 9 path is:
    /// `add_liquidity` SHOULD initialise `fees_claimed_per_share_*` to the
    /// pool's current `cumulative_fees_per_share_*` so a brand-new LP can't
    /// claim fees that accrued before they joined. This test pins the raw
    /// zero-default; the handler-level snapshot wiring is covered by the
    /// `claim_lp_fees` and `add_liquidity` tests.
    #[test]
    fn lp_position_default_fees_claimed_zero() {
        // SAFETY: LpPosition is `#[repr(C, packed)]` via #[account] and sums
        // to 121 bytes with no padding. All-zero is a valid bit pattern.
        let buf = [0u8; core::mem::size_of::<LpPosition>()];
        let lp: LpPosition = unsafe { core::ptr::read(buf.as_ptr() as *const LpPosition) };
        assert_eq!({ lp.fees_claimed_per_share_a }, 0u128);
        assert_eq!({ lp.fees_claimed_per_share_b }, 0u128);
    }
}
