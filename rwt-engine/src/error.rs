use arlex_lang::prelude::*;

#[error_code]
pub enum RwtError {
    #[msg("Signer is not the authority")]
    Unauthorized,
    #[msg("Signer is not the manager")]
    UnauthorizedManager,
    #[msg("Signer is not the pause authority")]
    UnauthorizedPause,
    #[msg("Minting is paused")]
    MintPaused,
    #[msg("Amount must be > 0")]
    ZeroAmount,
    #[msg("Backing capital must be > 0")]
    ZeroBackingCapital,
    #[msg("min_amount_out must be > 0")]
    ZeroSlippage,
    #[msg("Deposit below minimum ($1)")]
    BelowMinMint,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Writedown would reduce capital below floor")]
    InsufficientCapital,
    #[msg("Distribution ratios don't sum to 10,000")]
    InvalidDistributionRatios,
    #[msg("Cannot transfer authority to yourself")]
    SelfTransfer,
    #[msg("No pending authority transfer")]
    NoPendingAuthority,
    #[msg("Signer is not the pending authority")]
    InvalidPendingAuthority,
    #[msg("Mint would produce 0 RWT (deposit too small for current NAV)")]
    ZeroRwtOutput,
    #[msg("Pause authority cannot be zero address")]
    InvalidPauseAuthority,
    #[msg("Fee destination cannot be zero address")]
    InvalidFeeDestination,
    #[msg("Invalid token account")]
    InvalidTokenAccount,
    #[msg("Output below minimum (slippage protection)")]
    SlippageExceeded,
    #[msg("Destination address cannot be zero")]
    ZeroDestination,
    #[msg("DEX program account does not match DEX_PROGRAM_ID")]
    InvalidDexProgram,
    #[msg("Vault token account not owned by vault PDA")]
    InvalidVaultTokenOwner,
    #[msg("Manager wallet not set (zero address)")]
    ManagerDisabled,
    #[msg("Input and output token accounts must be different")]
    SameTokenAccount,
}
