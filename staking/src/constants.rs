//! Compile-time constants for the `staking` (stRWT) program.
//!
//! Source of truth: `docs/contracts/staking.mdx` (Constants table).
//!
//! The exchange-rate math uses a single virtual-offset model (OpenZeppelin
//! ERC4626 style): there is NO stored `initial_rate`. `VIRTUAL_ASSETS /
//! VIRTUAL_SHARES` IS the empty-pool bootstrap rate (= 10 RWT per stRWT at
//! defaults).

// ===== Rate / cooldown =====
/// 6-decimal fixed-point scale (matches RWT/USDC). Used only for the
/// `rate_after` event snapshot — the core stake/unstake math is unscaled
/// (shares are proportional, rate cancels out).
pub const RATE_SCALE: u64 = 1_000_000;

/// 21 days, in seconds. Default cooldown; tunable via `update_config`.
pub const COOLDOWN_SECONDS: i64 = 1_814_400;

// ===== Inflation-attack offsets =====
/// Virtual shares = 1 stRWT in 6-dec. Constant offset on the share side.
pub const VIRTUAL_SHARES: u64 = 1_000_000;

/// Virtual assets = 10 RWT in 6-dec. Constant offset on the asset side.
/// `VIRTUAL_ASSETS / VIRTUAL_SHARES = 10` → empty-pool bootstrap rate.
pub const VIRTUAL_ASSETS: u64 = 10_000_000;

/// Anti-dust floor: 1 RWT minimum per stake. Bounds rounding leverage.
pub const MIN_STAKE_AMOUNT: u64 = 1_000_000;

// ===== Well-known program IDs (classic SPL Token only) =====
// Mirrors `earn` / `rwt-engine` / `native-dex` so attribute macros can use
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

/// stRWT decimals — matches RWT (6-dec) so the rate fixed-point lines up.
pub const STRWT_DECIMALS: u8 = 6;

// ===== PDA seeds =====
pub const STAKING_CONFIG_SEED: &[u8] = b"staking_config";
pub const UNSTAKE_SEED: &[u8] = b"unstake";

// Sanity: bootstrap rate is exactly 10 at defaults.
const _: () = assert!(
    VIRTUAL_ASSETS / VIRTUAL_SHARES == 10,
    "VIRTUAL_ASSETS / VIRTUAL_SHARES must equal the bootstrap rate (10)",
);
