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

// Well-known mints (set at deployment, recompile for new addresses)
// Placeholder — replace with actual deployed addresses before devnet deploy
pub const RWT_MINT: [u8; 32] = [0u8; 32]; // TODO: set after RWT Engine deploy
pub const USDC_MINT: [u8; 32] = [0u8; 32]; // TODO: set per cluster

// Cross-program IDs
pub const OT_PROGRAM_ID: [u8; 32] = [0u8; 32]; // TODO: set after OT deploy

// Well-known program IDs (classic SPL Token only)
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
