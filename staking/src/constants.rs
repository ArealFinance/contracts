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

// ===== Deployment pins =====
// Devnet uses the deployer recorded in data/devnet-addresses.json. The
// non-devnet (mainnet) pin is the Areal deployer
// CyFCB88B3kMiPJSFLSXqP1u12dULeBaPh9qqjqquA1Np.
#[cfg(feature = "devnet")]
pub const BOOTSTRAP_AUTHORITY: [u8; 32] = [
    113, 99, 232, 220, 208, 39, 236, 240, 174, 142, 195, 171, 59, 231, 66, 10, 180, 85, 248, 95,
    186, 109, 55, 241, 57, 145, 147, 66, 159, 126, 75, 134,
];
#[cfg(not(feature = "devnet"))]
pub const BOOTSTRAP_AUTHORITY: [u8; 32] = [
    177, 217, 33, 18, 233, 27, 244, 236, 162, 235, 97, 21, 27, 235, 106, 191, 85, 147, 163, 7, 216,
    235, 215, 57, 206, 206, 120, 13, 144, 232, 154, 153,
];

// Canonical earn-RWT mint used by staking. This is the earn-specific RWT mint,
// not the legacy rwt-engine mint used by the larger app.
//   - devnet:  72,84,179,... (Areal devnet earn-RWT)
//   - mainnet: RWTeFt9M635Tf6w6yveAoXQR2ZwfXs7MfA7W3grDuGT (vanity earn-RWT mint)
#[cfg(feature = "devnet")]
pub const EARN_RWT_MINT: [u8; 32] = [
    114, 84, 179, 105, 246, 164, 208, 109, 85, 94, 130, 16, 92, 188, 157, 187, 210, 249, 30, 233,
    244, 25, 119, 235, 251, 227, 88, 2, 106, 40, 169, 110,
];
#[cfg(not(feature = "devnet"))]
pub const EARN_RWT_MINT: [u8; 32] = [
    6, 71, 63, 204, 93, 98, 64, 227, 160, 205, 60, 106, 13, 126, 100, 34, 86, 51, 235, 154, 222,
    235, 47, 128, 28, 138, 234, 50, 207, 141, 249, 32,
];

// ===== Well-known program IDs (classic SPL Token only) =====
// Mirrors `earn` / `rwt-engine` / `native-dex` so attribute macros can use
// literal byte arrays. Upstream source of truth: `arlex_lang::token::ID`.
pub const SPL_TOKEN_PROGRAM: [u8; 32] = [
    0x06, 0xdd, 0xf6, 0xe1, 0xd7, 0x65, 0xa1, 0x93, 0xd9, 0xcb, 0xe1, 0x46, 0xce, 0xeb, 0x79, 0xac,
    0x1c, 0xb4, 0x85, 0xed, 0x5f, 0x5b, 0x37, 0x91, 0x3a, 0x8c, 0xf5, 0x85, 0x7e, 0xff, 0x00, 0xa9,
];

pub const SYSTEM_PROGRAM: [u8; 32] = [0u8; 32];

pub const ASSOCIATED_TOKEN_PROGRAM: [u8; 32] = [
    0x8c, 0x97, 0x25, 0x8f, 0x4e, 0x24, 0x89, 0xf1, 0xbb, 0x3d, 0x10, 0x29, 0x14, 0x8e, 0x0d, 0x83,
    0x0b, 0x5a, 0x13, 0x99, 0xda, 0xff, 0x10, 0x84, 0x04, 0x8e, 0x7b, 0xd8, 0xdb, 0xe9, 0xf8, 0x59,
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
