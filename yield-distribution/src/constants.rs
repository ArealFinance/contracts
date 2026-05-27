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

// ----- Cross-program IDs (Layer 8 CPI targets) -----
//
// Source of truth: contracts/<program>/src/lib.rs `declare_id!(...)`.
// N-5 audit note: duplicated as `[u8; 32]` because attribute macros and CPI
// invocations need a literal byte array at parse time.

// DEX: DEX8LmvJpjefPS1cGS9zWB9ybxN24vNjTTrusBeqyARL (vanity)
// Devnet override: F9PaTy8SxmrLeheGycdGVAZBEB6FETMhoBfTUeSQLJ9u
#[cfg(feature = "devnet")]
pub const DEX_PROGRAM_ID: [u8; 32] = [
    0xd2, 0x29, 0xca, 0x93, 0xb8, 0xdb, 0x8a, 0x5b,
    0x38, 0xc6, 0x99, 0xeb, 0x08, 0xab, 0x42, 0xa6,
    0xab, 0xcd, 0x5e, 0x6b, 0xc5, 0xe8, 0x16, 0x44,
    0x22, 0xa1, 0xc1, 0xcc, 0x8f, 0xe8, 0xcc, 0x30,
];
#[cfg(not(feature = "devnet"))]
pub const DEX_PROGRAM_ID: [u8; 32] = [
    0xb5, 0xc2, 0xdb, 0x9c, 0x43, 0x7f, 0xea, 0xd1,
    0x4a, 0x4b, 0x38, 0x90, 0x93, 0xf5, 0x88, 0x24,
    0x25, 0xf7, 0x5d, 0x37, 0xbb, 0xa8, 0x8c, 0x8d,
    0xe9, 0xd1, 0x93, 0xde, 0x88, 0x6e, 0x79, 0x27,
];

// RWT Engine: RWT9hgbjHQDj98xP7FYsT5QYp5X32XyK6QfMRmFtARL (vanity)
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

// Layer 9 Nexus hosting program — alias of `DEX_PROGRAM_ID` (D17 / SD-3).
// The Nexus is implemented as an extension of the DEX program; the YD
// `withdraw_liquidity_holding` handler CPIs the DEX program (not a separate
// Nexus binary) for the `nexus_record_deposit` counter-bump leg.
//
// Layer 8 shipped this constant as `NEXUS_PROGRAM_ID_PLACEHOLDER = [0u8; 32]`
// behind an unconditional `NexusNotInitialized` revert (anti-honeypot guard
// per D4 — the LiquidityHolding RWT ATA was a one-way sink). Layer 9
// migration R20 retires the placeholder and pins the bytes to the canonical
// DEX vanity ID; the handler now executes the real PDA-signed Transfer + CPI.
//
// Identical bytes to `DEX_PROGRAM_ID` above; aliased here so call sites in
// `withdraw_liquidity_holding` read against the *semantic* identity (Nexus
// hosting program) rather than the literal DEX program. Drift between the
// two is caught by the parity test in `cpi.rs::tests`.
pub const NEXUS_HOSTING_PROGRAM_ID: [u8; 32] = DEX_PROGRAM_ID;

// ----- CPI discriminators -----
//
// Pre-computed `sha256("global:<ix_name>")[0..8]`. Verified at build time
// by the parity tests in `cpi.rs` (#[cfg(test)] block).

/// `native_dex::swap`
/// sha256("global:swap")[..8]
pub const DISC_DEX_SWAP: [u8; 8] = [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];

/// `rwt_engine::mint_rwt`
/// sha256("global:mint_rwt")[..8]
pub const DISC_RWT_MINT_RWT: [u8; 8] = [0x62, 0x20, 0x73, 0xde, 0x44, 0x0c, 0xa1, 0xa2];

/// `yield_distribution::withdraw_liquidity_holding` (own ix; same wire shape
/// as Layer 8 placeholder — only the body changed in Layer 9).
/// sha256("global:withdraw_liquidity_holding")[..8]
pub const DISC_YD_WITHDRAW_LIQUIDITY_HOLDING: [u8; 8] = [
    0x07, 0x14, 0x13, 0x12, 0xe4, 0x2e, 0xb3, 0x36,
];

/// `native_dex::nexus_record_deposit` (Layer 9 Nexus counter-bump CPI).
/// sha256("global:nexus_record_deposit")[..8]
pub const DISC_DEX_NEXUS_RECORD_DEPOSIT: [u8; 8] = [
    0x88, 0x20, 0x8d, 0x28, 0x72, 0xf6, 0x1b, 0x97,
];

// ----- Token-kind dispatch (mirrors native-dex::TOKEN_KIND_*) -----
//
// `nexus_record_deposit` accepts a 1-byte `token_kind` (USDC / RWT) for
// symmetry with `nexus_deposit`. YD's `withdraw_liquidity_holding` only ever
// drains RWT (the 15% liquidity-share parked from `claim_yield`), but pinning
// both kinds here keeps the dispatch table in lock-step with the DEX side.
pub const TOKEN_KIND_USDC: u8 = 0;
pub const TOKEN_KIND_RWT: u8 = 1;

// ----- Deployment-time pins (REPLACE for each environment) -----
//
// These placeholder values are the devnet pinned mints. For mainnet, overwrite
// with the actual addresses before running `arlex-cli build`. Two compile-time
// asserts below refuse to ship a binary that still carries the placeholders:
//
//   1. `array_all_zero` — catches accidental zero-init.
//   2. `is_rwt_placeholder` / `is_usdc_placeholder` — R20 tripwire that
//      fingerprints the exact ASCII-prefix + 0x01-tail pattern below. Gated
//      behind `cfg(not(feature = "dev-placeholder-mints"))` so devnet builds
//      can still compile by passing `--features dev-placeholder-mints` (see
//      `Cargo.toml` and `scripts/e2e-bootstrap.sh`).
//
// Devnet placeholder: same ASCII pattern so it's obvious they're placeholders.
#[cfg(feature = "devnet")]
pub const RWT_MINT: [u8; 32] = [
    0x89, 0x7a, 0x7c, 0x42, 0x6c, 0x1f, 0x30, 0x2b,
    0x97, 0xcc, 0xed, 0xa5, 0xce, 0x70, 0x1a, 0x6f,
    0x76, 0x4f, 0x61, 0xc7, 0x60, 0x68, 0x70, 0x10,
    0xfd, 0xa1, 0x69, 0x3a, 0xd1, 0x3f, 0xeb, 0xc2,
];
#[cfg(not(feature = "devnet"))]
pub const RWT_MINT: [u8; 32] = [    0x29, 0xcd, 0xfa, 0x85, 0x2d, 0x5e, 0xd9, 0x39,
    0x85, 0x2c, 0x4a, 0x70, 0x9b, 0x3c, 0x8a, 0x66,
    0x63, 0x91, 0x04, 0xd2, 0x41, 0x9a, 0xf5, 0xd5,
    0xf3, 0x51, 0x9e, 0xce, 0x47, 0x59, 0xf1, 0xa9,];

#[cfg(feature = "devnet")]
pub const USDC_MINT: [u8; 32] = [
    0xc1, 0xff, 0x1c, 0xd7, 0x45, 0xe8, 0xe0, 0xbf,
    0xa7, 0xd4, 0x8e, 0x00, 0x8f, 0x2d, 0xba, 0x73,
    0x58, 0xe8, 0xd5, 0x57, 0x1d, 0x3f, 0x75, 0x5a,
    0x5f, 0x6f, 0x84, 0x03, 0x36, 0x6d, 0x00, 0xc9,
];
#[cfg(not(feature = "devnet"))]
pub const USDC_MINT: [u8; 32] = [    0xd2, 0x28, 0x91, 0x77, 0xde, 0xc3, 0x53, 0xcf,
    0xbf, 0x50, 0x93, 0x06, 0x2b, 0xec, 0x3d, 0xe8,
    0xf6, 0x81, 0x7d, 0xdf, 0x13, 0x0a, 0xb5, 0x32,
    0x2a, 0x2a, 0x2a, 0x5d, 0x2c, 0xca, 0xc1, 0xb0,];

// Compile-time safety: refuse to build with all-zero pinned mints.
const _: () = assert!(!array_all_zero(&RWT_MINT));
const _: () = assert!(!array_all_zero(&USDC_MINT));

// R20 tripwire: refuse to build with the devnet placeholder bytes unless the
// `dev-placeholder-mints` feature is explicitly enabled. Mainnet build runbook
// (see Cargo.toml R20 comment) MUST overwrite these constants with the real
// pubkeys, then build *without* the feature so this assert verifies the
// placeholder bytes have actually been replaced.
#[cfg(not(feature = "dev-placeholder-mints"))]
const _: () = assert!(
    !is_rwt_placeholder(&RWT_MINT),
    "RWT_MINT still carries the devnet R20 placeholder bytes — see Cargo.toml \
     R20 tripwire comment. Replace with the deployed mainnet RWT mint pubkey \
     before building without `--features dev-placeholder-mints`."
);
#[cfg(not(feature = "dev-placeholder-mints"))]
const _: () = assert!(
    !is_usdc_placeholder(&USDC_MINT),
    "USDC_MINT still carries the devnet R20 placeholder bytes — see Cargo.toml \
     R20 tripwire comment. Replace with the canonical USDC mint pubkey before \
     building without `--features dev-placeholder-mints`."
);

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

#[allow(dead_code)] // used by the cfg-gated R20 tripwire asserts above
const fn array_zero_in_range(a: &[u8; 32], start: usize, end: usize) -> bool {
    let mut i = start;
    while i < end {
        if a[i] != 0 {
            return false;
        }
        i += 1;
    }
    true
}

// "RWT" + 28 zeros + 0x01.
#[allow(dead_code)] // used by the cfg-gated R20 tripwire assert above
const fn is_rwt_placeholder(a: &[u8; 32]) -> bool {
    a[0] == 0x52
        && a[1] == 0x57
        && a[2] == 0x54
        && array_zero_in_range(a, 3, 31)
        && a[31] == 0x01
}

// "USDC" + 27 zeros + 0x01.
#[allow(dead_code)] // used by the cfg-gated R20 tripwire assert above
const fn is_usdc_placeholder(a: &[u8; 32]) -> bool {
    a[0] == 0x55
        && a[1] == 0x53
        && a[2] == 0x44
        && a[3] == 0x43
        && array_zero_in_range(a, 4, 31)
        && a[31] == 0x01
}
