//! Compile-time constants for the `earn` program.
//!
//! Keeps the same NAV scaling and SPL/system program IDs as `rwt-engine`
//! so on-chain types interoperate naturally. The `RWT_MINT`/`USDC_MINT`
//! pins MUST be replaced before deploy to a cluster other than the local
//! Areal Testnet validator (`3pBtHBiBwh…` for RWT, `F9NVj8d…` for USDC).

// ===== NAV pricing =====
pub const NAV_SCALE: u64 = 1_000_000;   // 6-decimal fixed-point (matches USDC decimals)
pub const INITIAL_NAV: u64 = NAV_SCALE; // $1.00 when supply == 0
pub const MIN_CAPITAL_FLOOR: u64 = 1;   // prevents NAV = 0 with supply > 0
pub const RWT_DECIMALS: u8 = 6;

// ===== Mint split defaults (sum = 10_000) =====
// Aggressive bootstrap calibration: high Liquidity bump per mint (30%)
// to grow redemption / DEX-LP buffer quickly; high Treasury cut (10%) to
// fund operations during V1 before secondary revenue streams kick in.
// Calibration is tunable via `update_config` once bootstrap phase ends.
pub const BPS_DENOMINATOR: u64 = 10_000;
pub const DEFAULT_SPLIT_RWA_BPS: u16 = 6_000;        // 60% → RWA wallet (buys underlying)
pub const DEFAULT_SPLIT_LIQUIDITY_BPS: u16 = 3_000;  // 30% → Liquidity wallet (counts in NAV)
pub const DEFAULT_SPLIT_TREASURY_BPS: u16 = 1_000;   // 10% → ARL Treasury (revenue, not in NAV)

pub const MIN_MINT_AMOUNT: u64 = 1_000_000; // $1.00 minimum deposit (anti-dust)

// ===== Well-known program IDs (classic SPL Token only) =====
// Mirrors `rwt-engine` and `native-dex` so attribute macros can use literal arrays.
// Upstream source of truth: `arlex_lang::token::ID`.
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

// ===== Token mint pins =====
// Earn-RWT is a NEW SPL mint, separate from the big-app's rwt-engine RWT.
// Will be set in `initialize` from the actual mint account; this constant
// is reserved for potential future hardcoded validation.
//
// USDC mint matches the Areal Testnet validator pin
// (F9NVj8dFsqxbCfytfmrEWDjdDhmpV1YrjRuxiusGr9Ys).
pub const USDC_MINT: [u8; 32] = [
    0xd2, 0x28, 0x91, 0x77, 0xde, 0xc3, 0x53, 0xcf,
    0xbf, 0x50, 0x93, 0x06, 0x2b, 0xec, 0x3d, 0xe8,
    0xf6, 0x81, 0x7d, 0xdf, 0x13, 0x0a, 0xb5, 0x32,
    0x2a, 0x2a, 0x2a, 0x5d, 0x2c, 0xca, 0xc1, 0xb0,
];

// PDA seeds
pub const EARN_CONFIG_SEED: &[u8] = b"earn_config";

// Sanity check at compile time
const _: () = assert!(
    DEFAULT_SPLIT_RWA_BPS as u64
        + DEFAULT_SPLIT_LIQUIDITY_BPS as u64
        + DEFAULT_SPLIT_TREASURY_BPS as u64
        == BPS_DENOMINATOR,
    "mint split defaults must sum to BPS_DENOMINATOR",
);
