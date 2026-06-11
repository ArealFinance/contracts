use arlex_lang::prelude::*;

#[error_code]
pub enum EarnError {
    // ----- Access control -----
    #[msg("Signer is not the authority")]
    Unauthorized,
    #[msg("Signer is not the bootstrap authority")]
    UnauthorizedBootstrap,

    // ----- Lifecycle / state -----
    #[msg("RWT supply must be non-zero")]
    NoRwtSupply,

    // ----- Inputs -----
    #[msg("Amount must be > 0")]
    ZeroAmount,
    #[msg("Deposit below minimum")]
    BelowMinMint,
    #[msg("mint_fee_bps exceeds the maximum")]
    FeeTooHigh,
    #[msg("min_mint_amount exceeds the maximum")]
    MinMintTooHigh,
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
    #[msg("RWT mint authority must be the EarnConfig PDA")]
    InvalidMintAuthority,
    #[msg("RWT mint supply must be zero at initialize")]
    InvalidMintSupply,
    #[msg("RWT mint decimals mismatch")]
    InvalidMintDecimals,
    #[msg("RWT mint freeze authority must be unset")]
    InvalidFreezeAuthority,
    #[msg("Token account owner mismatch")]
    InvalidTokenOwner,
    #[msg("Destination address cannot be zero")]
    ZeroDestination,
    #[msg("Fee destination cannot be zero address")]
    InvalidFeeDestination,

    // ----- 2-step authority transfer -----
    #[msg("No pending authority transfer")]
    NoPendingAuthority,
    #[msg("Signer is not the pending authority")]
    InvalidPendingAuthority,
    #[msg("Cannot transfer authority to yourself")]
    SelfTransfer,

    // ----- Accounting integrity -----
    #[msg("Fee destination cannot be the basket vault")]
    FeeDestinationIsBasketVault,

    // ----- Basket vault custody (external treasury) -----
    #[msg("Basket vault is not configured")]
    BasketVaultNotSet,
    #[msg("Basket vault cannot be the zero address")]
    ZeroBasketVault,

    // ----- Genesis (one-time founder seed) -----
    #[msg("Genesis seed already complete (supply is non-zero)")]
    GenesisAlreadyComplete,
}
