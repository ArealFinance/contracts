use arlex_lang::prelude::*;

#[error_code]
pub enum EarnError {
    // ----- Access control -----
    #[msg("Signer is not the authority")]
    Unauthorized,
    #[msg("Signer is not the pause authority")]
    UnauthorizedPause,

    // ----- Lifecycle / state -----
    #[msg("Earn program is paused")]
    EarnPaused,
    #[msg("Already initialized")]
    AlreadyInitialized,

    // ----- Inputs -----
    #[msg("Amount must be > 0")]
    ZeroAmount,
    #[msg("Deposit below minimum")]
    BelowMinMint,
    #[msg("min_rwt_out must be > 0")]
    ZeroSlippage,
    #[msg("Output below min_rwt_out (slippage protection)")]
    SlippageExceeded,
    #[msg("Mint would produce 0 RWT (deposit too small for current NAV)")]
    ZeroRwtOutput,

    // ----- Math -----
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Split bps must sum to 10,000")]
    InvalidSplitBps,
    #[msg("Writedown would reduce capital below floor")]
    InsufficientCapital,

    // ----- Accounts -----
    #[msg("Invalid token account")]
    InvalidTokenAccount,
    #[msg("RWA wallet mint must be USDC")]
    InvalidRwaWalletMint,
    #[msg("Liquidity wallet mint must be USDC")]
    InvalidLiquidityWalletMint,
    #[msg("Treasury wallet mint must be USDC")]
    InvalidTreasuryWalletMint,
    #[msg("RWT mint does not match config.rwt_mint")]
    InvalidRwtMint,
    #[msg("Destination address cannot be zero")]
    ZeroDestination,
    #[msg("Pause authority cannot be zero address")]
    InvalidPauseAuthority,
}
