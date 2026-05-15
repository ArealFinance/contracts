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
pub const MAX_BINS: usize = 70;
pub const DEFAULT_BIN_STEP_BPS: u16 = 10;            // 0.1% price step between bins
pub const MAX_BIN_STEP_BPS: u16 = 500;               // 5% max step — prevents extreme price jumps
pub const MAX_SHIFT_DISTANCE: i32 = 35;              // Max bins from active_bin to nav_bin
pub const MAX_INITIAL_ACTIVE_BIN: i32 = 10_000;     // Reasonable range for initial_active_bin
pub const CONCENTRATED_SCALE: u128 = 1_000_000_000_000; // 10^12 for pow_bps

// Well-known mints (set per deployment, recompile for new cluster)
// RWT_MINT = FUQX2AepBoun3hFQjoXcfbX5aGRLxfACx1sAqCC63i5
//
// MAINNET-REPLACE: devnet vanity address. For mainnet release, replace
// bytes here AND in contracts/ownership-token/src/constants.rs (RWT_MINT)
// with the production RWT mint. Mismatch causes silent DoS on token-pair
// validation across DEX swap/RWT/Layer 8 claim flows.
pub const RWT_MINT: [u8; 32] = [
    0x29, 0xcd, 0xfa, 0x85, 0x2d, 0x5e, 0xd9, 0x39,
    0x85, 0x2c, 0x4a, 0x70, 0x9b, 0x3c, 0x8a, 0x66,
    0x63, 0x91, 0x04, 0xd2, 0x41, 0x9a, 0xf5, 0xd5,
    0xf3, 0x51, 0x9e, 0xce, 0x47, 0x59, 0xf1, 0xa9,
];

// USDC_MINT — devnet test USDC (4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU)
// Not validated on-chain in Layer 4 — used only by dashboard/bots for ATA derivation.
pub const USDC_MINT: [u8; 32] = [0u8; 32]; // Not used in contract logic

// Cross-program IDs
// OT: oWnqbNwmEdjNS5KVbxz8xeuGNjKMd1aiNF89d7qdARL (vanity)
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
pub const YD_PROGRAM_ID: [u8; 32] = [
    0x08, 0x06, 0xb9, 0xa3, 0xae, 0xcd, 0x1b, 0xec,
    0xb0, 0x2a, 0xf2, 0x1b, 0x03, 0x64, 0xd9, 0x29,
    0xfb, 0xb1, 0x02, 0x21, 0x7f, 0x1a, 0x93, 0xd2,
    0x89, 0x99, 0xeb, 0xc8, 0xbf, 0x60, 0xa0, 0xaa,
];

/// `yield_distribution::claim`
/// sha256("global:claim")[..8]
pub const DISC_YD_CLAIM: [u8; 8] = [0x3e, 0xc6, 0xd6, 0xc1, 0xd5, 0x9f, 0x6c, 0xd2];

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
