// Fee model
pub const BPS_DENOMINATOR: u64 = 10_000;
pub const DEFAULT_BASE_FEE_BPS: u16 = 50;          // 0.5%
pub const DEFAULT_LP_FEE_SHARE_BPS: u16 = 5_000;   // 50% of fee to LP
pub const MAX_FEE_BPS: u16 = 1_000;                // 10% max
pub const OT_TREASURY_FEE_BPS: u16 = 50;           // 0.5% additional for OT pairs

// Pool
pub const MAX_POOL_CREATORS: usize = 10;
pub const MIN_LIQUIDITY: u64 = 1_000;               // Burned on first add (anti-donation attack)
pub const POOL_TYPE_STANDARD: u8 = 0;
pub const POOL_TYPE_CONCENTRATED: u8 = 1;

// Concentrated liquidity
//
// CP-1 Monotonic Ladder rewrite (docs/changelog/2026-04-17-monotonic-ladder.mdx):
// MAX_BINS was 70 in the legacy pyramid layout; the Monotonic Ladder uses a
// log-scale BinArray.
//
// CP-1 hotfix (2026-05-17): MAX_BINS reduced 1000 → 630 to fit the Solana
// CPI realloc limit (`MAX_PERMITTED_DATA_INCREASE = 10_240` bytes per inner
// instruction). At 16 bytes per bin plus 43 bytes of trailing scalar fields
// and the 8-byte arlex account discriminator, 630 bins keeps the BinArray
// account at 10_131 bytes — ~109 bytes of buffer below the runtime cap.
// StandardCurve pools never read past the active region, so the larger array
// is rent-only impact for legacy pools.
//
// Coverage at bin_step_bps=10 (0.1% per bin):
// - NAV growth × (1.001)^630 ≈ 1.877
// - ~9.3 years at 7% APY, ~6.6 years at 10% APY
//
// The original spec (`docs/changelog/2026-04-17-monotonic-ladder.mdx`) sized
// for 1000 bins / 10+ years, but the Solana 3.x CPI realloc limit was
// identified during CP-12 deploy verification. Lifting >10_240 requires
// splitting BinArray creation into a 2-ix flow (create + extend_bin_array),
// tracked as a separate ticket.
pub const MAX_BINS: usize = 630;
pub const DEFAULT_BIN_STEP_BPS: u16 = 10;            // 0.1% price step between bins
pub const MAX_BIN_STEP_BPS: u16 = 500;               // 5% max step — prevents extreme price jumps
pub const MAX_INITIAL_ACTIVE_BIN: i32 = 10_000;     // Reasonable range for initial_active_bin
pub const CONCENTRATED_SCALE: u128 = 1_000_000_000_000; // 10^12 for pow_bps

// ----- Monotonic Ladder (CP-1) -----
//
// New constants for the log-scale concentrated rewrite. Source:
// docs/changelog/2026-04-17-monotonic-ladder.mdx and docs/contracts/native-dex.mdx
// (Monotonic Ladder section). Consumers land in CP-3+; CP-1 only introduces
// the values so downstream commits can reference them directly.

/// Width (in bins) of the dense active-bid zone where geometric density applies.
/// docs §1300.
pub const ACTIVE_ZONE_WIDTH: u16 = 40;

/// Geometric density ratio for the active bid wall, expressed in bps.
/// `r = 0.85` per docs §1303. Used by `compress_liquidity` / `grow_liquidity`
/// to compute per-bin USDC weights via geometric series.
pub const GEOMETRIC_R_BPS: u16 = 8500;

/// Default permanent-tail offset below initial NAV, in bps. docs §51 sets the
/// upper edge of the permanent tail at `initial_NAV − 1%`.
pub const DEFAULT_PERMANENT_TAIL_OFFSET_BPS: i32 = 100;

/// Lower bound for the permanent-tail offset (docs:228). Pools may opt to a
/// tighter floor but never closer than 0.3% to initial NAV.
pub const MIN_PERMANENT_TAIL_OFFSET_BPS: i32 = 30;

/// Number of bins comprising the immutable permanent-tail USDC reserve
/// (docs:234). Never shifted, retained across the protocol lifetime.
pub const PERMANENT_TAIL_BIN_COUNT: i32 = 70;

/// Q64.64 representation of one (2^64). Used by the Monotonic Ladder
/// geometric-density helpers (`GEOMETRIC_WEIGHTS`) and shared with the
/// existing LP-fee accumulator math (`cumulative_fees_per_share_*`). Pulled
/// into `constants.rs` in CP-3 so growth/compression redistribute helpers
/// have a single canonical symbol.
pub const Q64: u128 = 1u128 << 64;

/// Right-edge buffer (in bins) preserved between `last_rebalance_nav_bin`
/// and the upper bound of the bin array. `grow_redistribute` refuses to
/// advance the active zone if doing so would leave fewer than this many
/// trailing bins of organic-ask headroom. Sized so the ladder can keep
/// re-growing for several rebalance cycles before the array fills.
pub const RIGHT_EDGE_BUFFER_BINS: i32 = 10;

/// Price offset above NAV at which master-pool USDC→RWT swaps reroute through
/// `rwt_engine::mint_rwt` instead of consuming organic ask (docs:633 —
/// `NAV × 1.005`).
pub const MINT_ROUTE_PRICE_OFFSET_BPS: u16 = 50;

/// USDY mint placeholder — pinned at mainnet via a dedicated mint-pinning
/// commit (mirrors the existing RWT_MINT MAINNET-REPLACE pattern). Kept as
/// the zero pubkey until then so on-chain comparisons predictably reject
/// USDY-paired pools before pinning.
pub const USDY_MINT: [u8; 32] = [0u8; 32];

// Well-known mints (set per deployment, recompile for new cluster)
// RWT_MINT = FUQX2AepBoun3hFQjoXcfbX5aGRLxfACx1sAqCC63i5
//
// MAINNET-REPLACE: devnet vanity address. For mainnet release, replace
// bytes here AND in contracts/ownership-token/src/constants.rs (RWT_MINT)
// with the production RWT mint. Mismatch causes silent DoS on token-pair
// validation across DEX swap/RWT/Layer 8 claim flows.
pub const RWT_MINT: [u8; 32] = [
    0xfe, 0x25, 0x03, 0x40, 0x7f, 0x74, 0x89, 0x10,
    0x1c, 0x81, 0x60, 0x2a, 0x86, 0x75, 0xa9, 0x7b,
    0xee, 0xf4, 0xa0, 0x6a, 0xa4, 0x1a, 0xb4, 0x89,
    0x3f, 0xbf, 0x30, 0x62, 0x12, 0x72, 0x16, 0x33,
];

// USDC_MINT — devnet test USDC (4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU)
// Not validated on-chain in Layer 4 — used only by dashboard/bots for ATA derivation.
pub const USDC_MINT: [u8; 32] = [0u8; 32]; // Not used in contract logic

// Cross-program IDs
// OT: oWnqbNwmEdjNS5KVbxz8xeuGNjKMd1aiNF89d7qdARL (vanity)
// Devnet override: Eu7hc5gdjVHGff51bQHEDF1fSsWVRvUE15vBsFwu2oaX
#[cfg(feature = "devnet")]
pub const OT_PROGRAM_ID: [u8; 32] = [
    0xce, 0x81, 0xb5, 0x3a, 0x71, 0x11, 0x87, 0xa3,
    0xa0, 0x8e, 0x5c, 0x52, 0x1d, 0x90, 0x9e, 0x70,
    0xfd, 0x9e, 0xaa, 0x0b, 0x85, 0xac, 0x47, 0x19,
    0xab, 0x8a, 0xf1, 0x1a, 0x7a, 0xbc, 0x45, 0xb8,
];
#[cfg(not(feature = "devnet"))]
pub const OT_PROGRAM_ID: [u8; 32] = [
    0x0b, 0xea, 0x66, 0xb6, 0xad, 0xc5, 0x50, 0xb8,
    0xe1, 0x4a, 0x03, 0x75, 0x5d, 0x55, 0xdc, 0x40,
    0x52, 0x1a, 0xa9, 0x2a, 0xf7, 0x06, 0xfb, 0x25,
    0x16, 0xbf, 0xc4, 0xc7, 0xf7, 0x6b, 0x12, 0x27,
];

// Well-known program IDs (classic SPL Token only).
// N-5 audit note: duplicated across contracts so attribute macros can use
// literal byte arrays. Upstream source of truth: `arlex_lang::token::ID`.
pub const SPL_TOKEN_PROGRAM: [u8; 32] = [
    0x06, 0xdd, 0xf6, 0xe1, 0xd7, 0x65, 0xa1, 0x93,
    0xd9, 0xcb, 0xe1, 0x46, 0xce, 0xeb, 0x79, 0xac,
    0x1c, 0xb4, 0x85, 0xed, 0x5f, 0x5b, 0x37, 0x91,
    0x3a, 0x8c, 0xf5, 0x85, 0x7e, 0xff, 0x00, 0xa9,
];

pub const SYSTEM_PROGRAM: [u8; 32] = [0u8; 32];

pub const ASSOCIATED_TOKEN_PROGRAM: [u8; 32] = [
    0x8c, 0x97, 0x25, 0x8f, 0x4e, 0x24, 0x89, 0xf1,
    0xbb, 0x3d, 0x10, 0x29, 0x14, 0x8e, 0x0d, 0x83,
    0x0b, 0x5a, 0x13, 0x99, 0xda, 0xff, 0x10, 0x84,
    0x04, 0x8e, 0x7b, 0xd8, 0xdb, 0xe9, 0xf8, 0x59,
];

// ----- Layer 8 CPI targets -----
//
// Source of truth: contracts/yield-distribution/src/lib.rs `declare_id!(...)`.

// Yield Distribution: YLD9EBikcTmVCnVzdx6vuNajrDkp8tyCAgZrqTwmMXF (vanity)
// Devnet override: DaJvfRk5m6bWYBNMw4CujmBSjni1U4mzhpBLEmN2Lwro
#[cfg(feature = "devnet")]
pub const YD_PROGRAM_ID: [u8; 32] = [
    0xba, 0xd4, 0xad, 0x8c, 0x4c, 0x8f, 0x1e, 0x58,
    0x02, 0xa2, 0x3a, 0x93, 0x53, 0xef, 0xf9, 0xea,
    0xc4, 0x80, 0xbd, 0xe6, 0x6b, 0xf4, 0x20, 0x22,
    0x58, 0x5d, 0xb8, 0xd5, 0x29, 0x05, 0x13, 0x88,
];
#[cfg(not(feature = "devnet"))]
pub const YD_PROGRAM_ID: [u8; 32] = [
    0x08, 0x06, 0xb9, 0xa3, 0xae, 0xcd, 0x1b, 0xec,
    0xb0, 0x2a, 0xf2, 0x1b, 0x03, 0x64, 0xd9, 0x29,
    0xfb, 0xb1, 0x02, 0x21, 0x7f, 0x1a, 0x93, 0xd2,
    0x89, 0x99, 0xeb, 0xc8, 0xbf, 0x60, 0xa0, 0xaa,
];

/// `yield_distribution::claim`
/// sha256("global:claim")[..8]
pub const DISC_YD_CLAIM: [u8; 8] = [0x3e, 0xc6, 0xd6, 0xc1, 0xd5, 0x9f, 0x6c, 0xd2];

// ----- CP-6 RWT Engine CPI targets -----
//
// Source of truth: contracts/rwt-engine/src/lib.rs `declare_id!(...)`.
//
// CP-6 introduces a single new CPI surface — `rwt_engine::mint_rwt` — invoked
// by `swap` when a master pool's USDC→RWT trade would consume organic ask
// priced above `NAV × (1 + MINT_ROUTE_PRICE_OFFSET_BPS / BPS_DENOMINATOR)` or
// when the organic ask is empty. The user is the signer (pass-through), so
// this is an unsigned `invoke` (NOT `invoke_signed`) — no new authority
// surface is created on the DEX side.

/// RWT Engine program ID: `RWT9hgbjHQDj98xP7FYsT5QYp5X32XyK6QfMRmFtARL` (vanity).
/// Mirrored from `contracts/rwt-engine/src/lib.rs::declare_id!`. A parity test
/// in `cpi.rs::tests` enforces that the bytes match the canonical vanity
/// address (same pattern as `YD_PROGRAM_ID`).
// Devnet override: G5zE57v3fBdWuxPvMmpwTPxATdnR99u6j5U9YfU4kABw
#[cfg(feature = "devnet")]
pub const RWT_ENGINE_PROGRAM_ID: [u8; 32] = [
    0xe0, 0x26, 0x55, 0x01, 0x91, 0x4a, 0xde, 0x72,
    0x30, 0x82, 0x30, 0x4b, 0x7a, 0x82, 0xb7, 0xb8,
    0xa6, 0x4c, 0x7b, 0xb7, 0x77, 0x9a, 0xed, 0xa1,
    0xf4, 0xdf, 0x67, 0x15, 0xf7, 0xe8, 0x55, 0x86,
];
#[cfg(not(feature = "devnet"))]
pub const RWT_ENGINE_PROGRAM_ID: [u8; 32] = [
    0x06, 0x47, 0x3d, 0x57, 0x5a, 0xee, 0x84, 0xb9,
    0x31, 0xd6, 0xc0, 0x90, 0x1e, 0x42, 0xc6, 0xb5,
    0xf4, 0x57, 0x82, 0x1c, 0x13, 0x68, 0x40, 0xc6,
    0x49, 0xa1, 0x15, 0xcd, 0x39, 0xdb, 0x48, 0x1f,
];

/// `rwt_engine::mint_rwt` discriminator: `sha256("global:mint_rwt")[..8]`.
/// Verified against the canonical SHA-256 by `cpi.rs::tests::disc_rwt_mint_matches_sha256`.
pub const DISC_RWT_MINT: [u8; 8] = [0x62, 0x20, 0x73, 0xde, 0x44, 0x0c, 0xa1, 0xa2];

/// Byte offset of `RwtVault.nav_book_value` (u64 LE) inside the account's
/// data buffer, EXCLUDING the 8-byte arlex account discriminator. The actual
/// on-chain layout starts with the 8-byte discriminator, so the absolute
/// offset from `data_ptr()` is `RWT_VAULT_DISC_LEN + RWT_VAULT_NAV_OFFSET = 32`.
///
/// Layout (from `contracts/rwt-engine/src/state.rs::RwtVault`, `repr(C, packed)`):
///   offset 0..16   total_invested_capital (u128)
///   offset 16..24  total_rwt_supply       (u64)
///   offset 24..32  nav_book_value         (u64)  ← reading here
///   offset 32..64  capital_accumulator_ata
///   ...
pub const RWT_VAULT_NAV_OFFSET: usize = 24;

/// Length of the arlex account discriminator prefix (same as Anchor: 8 bytes).
pub const RWT_VAULT_DISC_LEN: usize = 8;

/// arlex `#[account]` discriminator for rwt_engine::RwtVault =
/// sha256("account:RwtVault")[..8]. Mirrored here for defence-in-depth
/// in `read_rwt_vault_nav`. Tripwire test in `cpi.rs::tests` asserts it
/// matches the canonical sha256 derivation.
pub const DISC_RWT_VAULT: [u8; 8] = [0x60, 0x43, 0xda, 0x41, 0x42, 0x7a, 0x0a, 0x11];

// ----- Layer 9 (Liquidity Nexus) -----

/// PDA seed for the singleton `LiquidityNexus` account. One Nexus per DEX
/// deployment. Layer 9 §3, SD-2.
pub const LIQUIDITY_NEXUS_SEED: &[u8] = b"liquidity_nexus";

/// Address of the program that hosts the `LiquidityNexus` PDA — the DEX
/// program itself (`DEX8LmvJpjefPS1cGS9zWB9ybxN24vNjTTrusBeqyARL`). Used by
/// Yield Distribution's `withdraw_liquidity_holding` to derive the Nexus PDA
/// and to validate the CPI target program ID for `nexus_record_deposit`.
/// SD-3 / D17.
///
/// Source of truth: this crate's `declare_id!(...)` in `lib.rs`. Duplicated
/// as a `[u8; 32]` literal here because attribute macros and CPI invocations
/// (across DEX and YD) need a literal byte array at parse time (N-5 audit
/// note pattern). A parity test in `cpi.rs::tests` enforces that the bytes
/// match the canonical vanity address.
// Devnet override: F9PaTy8SxmrLeheGycdGVAZBEB6FETMhoBfTUeSQLJ9u (DEX devnet program ID)
#[cfg(feature = "devnet")]
pub const NEXUS_HOSTING_PROGRAM_ID: [u8; 32] = [
    0xd2, 0x29, 0xca, 0x93, 0xb8, 0xdb, 0x8a, 0x5b,
    0x38, 0xc6, 0x99, 0xeb, 0x08, 0xab, 0x42, 0xa6,
    0xab, 0xcd, 0x5e, 0x6b, 0xc5, 0xe8, 0x16, 0x44,
    0x22, 0xa1, 0xc1, 0xcc, 0x8f, 0xe8, 0xcc, 0x30,
];
#[cfg(not(feature = "devnet"))]
pub const NEXUS_HOSTING_PROGRAM_ID: [u8; 32] = [
    0xb5, 0xc2, 0xdb, 0x9c, 0x43, 0x7f, 0xea, 0xd1,
    0x4a, 0x4b, 0x38, 0x90, 0x93, 0xf5, 0x88, 0x24,
    0x25, 0xf7, 0x5d, 0x37, 0xbb, 0xa8, 0x8c, 0x8d,
    0xe9, 0xd1, 0x93, 0xde, 0x88, 0x6e, 0x79, 0x27,
];

/// Manager kill-switch sentinel value (the zero pubkey). Per D22, when
/// `LiquidityNexus.manager` equals this value, every manager-gated ix
/// (`nexus_swap`, `nexus_add_liquidity`, `nexus_remove_liquidity`) reverts
/// with `NexusManagerDisabled` via the `assert_manager` helper, regardless
/// of which wallet signed. `update_nexus_manager` intentionally allows
/// setting the manager to this value — that is the on-chain kill-switch.
pub const NEXUS_MANAGER_KILL_SWITCH: [u8; 32] = [0u8; 32];

/// Rebalancer kill-switch sentinel (zero pubkey). Per CP-12.5, setting
/// `DexConfig.rebalancer` to this value freezes `grow_liquidity` and
/// `compress_liquidity` — no signer can produce a signature for [0u8;32].
/// Symmetric with NEXUS_MANAGER_KILL_SWITCH. `update_dex_config` permits
/// this value by design (incident-response lever).
pub const REBALANCER_KILL_SWITCH: [u8; 32] = [0u8; 32];

/// Token-kind tags used by `nexus_deposit` / `nexus_record_deposit` /
/// `nexus_withdraw_profits` to disambiguate the principal counter to bump
/// or read. Layer 9 §4.2 / §4.6 / §4.9.
pub const TOKEN_KIND_USDC: u8 = 0;
pub const TOKEN_KIND_RWT: u8 = 1;

/// Source-kind tags emitted in `NexusDeposited.source_kind` to distinguish
/// permissionless `nexus_deposit` calls from the YD `withdraw_liquidity_holding`
/// CPI drain. Layer 9 §4.2 / §4.9.
pub const NEXUS_DEPOSIT_SOURCE_DIRECT: u8 = 0;
pub const NEXUS_DEPOSIT_SOURCE_LIQUIDITY_HOLDING: u8 = 1;
