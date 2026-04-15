use arlex_lang::prelude::*;

#[error_code]
pub enum DexError {
    #[msg("Not DEX authority")]
    Unauthorized,
    #[msg("Not in pool creators whitelist")]
    CreatorNotWhitelisted,
    #[msg("Global DEX is paused")]
    DexPaused,
    #[msg("Pool is paused")]
    PoolNotActive,
    #[msg("Max 10 creators")]
    WhitelistFull,
    #[msg("token_a == token_b")]
    IdenticalMints,
    #[msg("Creator not found in whitelist")]
    CreatorNotFound,
    #[msg("Amount must be > 0")]
    ZeroAmount,
    #[msg("Pool reserves empty")]
    InsufficientLiquidity,
    #[msg("LP has fewer shares than burn amount")]
    InsufficientShares,
    #[msg("First LP deposit too small (< MIN_LIQUIDITY)")]
    InitialLiquidityTooSmall,
    #[msg("Output below min_amount_out")]
    SlippageExceeded,
    #[msg("Swap would produce 0 output")]
    ZeroOutput,
    #[msg("Cannot swap with empty reserves")]
    EmptyReserves,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("base_fee_bps exceeds MAX_FEE_BPS")]
    InvalidFee,
    #[msg("lp_fee_share_bps exceeds 10,000")]
    InvalidFeeShare,
    #[msg("Neither token is RWT_MINT")]
    MissingRwtMint,
    #[msg("token_a_mint >= token_b_mint (must be canonical order)")]
    InvalidMintOrder,
    #[msg("Vault account does not match pool_state vault")]
    InvalidVault,
    #[msg("OT Treasury PDA derivation or ownership mismatch")]
    InvalidOtTreasuryDestination,
    #[msg("Pool has OT fee but ot_treasury_fee_account not provided")]
    MissingOtTreasuryAccount,
    #[msg("OT treasury fee account does not match stored destination")]
    OtTreasuryAccountMismatch,
    #[msg("Cannot transfer authority to yourself")]
    SelfTransfer,
    #[msg("No pending authority transfer")]
    NoPendingAuthority,
    #[msg("Signer is not pending authority")]
    InvalidPendingAuthority,
    #[msg("Signer is not pause authority")]
    UnauthorizedPause,
    #[msg("Creator already in whitelist")]
    CreatorAlreadyExists,
    #[msg("Pause authority cannot be zero address")]
    InvalidPauseAuthority,
    #[msg("Fee destination cannot be zero address")]
    InvalidFeeDestination,
    #[msg("Address cannot be zero")]
    ZeroAddress,
    #[msg("Invalid token account")]
    InvalidTokenAccount,
}
