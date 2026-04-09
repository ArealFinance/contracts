// Revenue distribution
pub const BPS_DENOMINATOR: u64 = 10_000;
pub const AREAL_PROTOCOL_FEE_BPS: u64 = 25; // 0.25%
pub const MAX_DESTINATIONS: usize = 10;
pub const MIN_DISTRIBUTION_AMOUNT: u64 = 100_000_000; // $100 USDC (6 decimals)
pub const DISTRIBUTION_COOLDOWN: i64 = 604_800; // 7 days in seconds

// Token constraints
pub const MAX_DECIMALS: u8 = 9;

// Well-known program IDs
//
// NOTE: This contract only supports classic SPL Token (TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA).
// Token-2022 accounts (owned by TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb) will be rejected
// by `owner = SPL_TOKEN_PROGRAM` constraints. This is acceptable for Layer 1 — all protocol
// tokens (USDC, RWT, OT) use classic SPL Token.
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
