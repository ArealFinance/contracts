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
    #[msg("Writedown would reduce capital below floor")]
    InsufficientCapital,

    // ----- Accounts -----
    #[msg("Invalid token account")]
    InvalidTokenAccount,
    #[msg("RWT mint does not match config.rwt_mint")]
    InvalidRwtMint,
    #[msg("Destination address cannot be zero")]
    ZeroDestination,
    #[msg("Pause authority cannot be zero address")]
    InvalidPauseAuthority,
    #[msg("Fee destination cannot be zero address")]
    InvalidFeeDestination,

    // ----- 2-step authority transfer -----
    #[msg("No pending authority transfer")]
    NoPendingAuthority,
    #[msg("Signer is not the pending authority")]
    InvalidPendingAuthority,
    #[msg("Cannot transfer authority to yourself")]
    SelfTransfer,
}
