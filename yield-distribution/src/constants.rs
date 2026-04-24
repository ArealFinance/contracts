//! Yield Distribution — constants pinned at compile time.
//!
//! `RWT_MINT` / `USDC_MINT` must be filled from deployment output before the
//! program is compiled for mainnet. On devnet the placeholder values below are
//! used; compile-time asserts prevent shipping with all-zero pins.

// ----- Protocol tuning -----
pub const BPS_DENOMINATOR: u64 = 10_000;
pub const DEFAULT_PROTOCOL_FEE_BPS: u16 = 25; // 0.25%
pub const DEFAULT_MIN_DISTRIBUTION: u64 = 100_000_000; // $100 (6-decimals RWT)
pub const DEFAULT_VESTING_PERIOD: i64 = 31_536_000; // 365d
pub const MAX_PROOF_LEN: usize = 20; // ~1M holders
pub const MIN_VESTED_AMOUNT: u64 = 1_000_000; // 1 RWT floor

// ----- Well-known program IDs (classic SPL Token only) -----
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

// ----- Deployment-time pins (REPLACE for each environment) -----
//
// These placeholder values are the devnet pinned mints. For mainnet, overwrite
// with the actual addresses before running `arlex-cli build`. The
// `array_all_zero` assertion below ensures we cannot ship with zero values.
//
// Devnet placeholder: same ASCII pattern so it's obvious they're placeholders.
pub const RWT_MINT: [u8; 32] = [
    0x52, 0x57, 0x54, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
];

pub const USDC_MINT: [u8; 32] = [
    0x55, 0x53, 0x44, 0x43, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
];

// Compile-time safety: refuse to build with all-zero pinned mints.
const _: () = assert!(!array_all_zero(&RWT_MINT));
const _: () = assert!(!array_all_zero(&USDC_MINT));

const fn array_all_zero(a: &[u8; 32]) -> bool {
    let mut i = 0;
    let mut zero = true;
    while i < 32 {
        if a[i] != 0 {
            zero = false;
        }
        i += 1;
    }
    zero
}
