use arlex_lang::prelude::*;

#[error_code]
pub enum OtError {
    #[msg("Signer is not the governance authority")]
    Unauthorized,
    #[msg("Amount must be > 0")]
    ZeroAmount,
    #[msg("Destination allocations don't sum to 10,000")]
    InvalidBpsTotal,
    #[msg("BPS not in range 1-10,000")]
    InvalidAllocationBps,
    #[msg("Same address used twice in destinations")]
    DuplicateDestination,
    #[msg("Destination list is empty")]
    EmptyDestinationList,
    #[msg("More than 10 destinations")]
    TooManyDestinations,
    #[msg("ATA balance < minimum distribution amount")]
    BelowMinDistribution,
    #[msg("Less than 7 days since last distribution")]
    DistributionCooldown,
    #[msg("Distribution already in progress (reentrancy)")]
    DistributionInProgress,
    #[msg("Not enough remaining accounts for all destinations")]
    InsufficientRemainingAccounts,
    #[msg("Remaining account doesn't match destination address")]
    DestinationAccountMismatch,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Mint supply must be 0 (fresh mint)")]
    InvalidMintSupply,
    #[msg("Mint authority must be deployer")]
    InvalidMintAuthority,
    #[msg("Freeze authority must be None")]
    FreezeAuthoritySet,
    #[msg("Token name is empty")]
    InvalidName,
    #[msg("Token symbol is empty")]
    InvalidSymbol,
    #[msg("Decimals must be 1-9")]
    InvalidDecimals,
    #[msg("No pending authority transfer")]
    NoPendingAuthority,
    #[msg("Signer is not the pending authority")]
    InvalidPendingAuthority,
    #[msg("Cannot transfer authority to yourself")]
    AuthorityTransferToSelf,
    #[msg("Destination address cannot be the fee destination")]
    FeeDestinationCollision,
    #[msg("Destination address cannot be zero")]
    ZeroDestinationAddress,
    #[msg("Active destinations BPS sum must be 10,000")]
    ActiveBpsSumMismatch,
    #[msg("Governance is inactive")]
    GovernanceInactive,
    #[msg("Token account not owned by SPL Token Program")]
    InvalidTokenAccountOwner,
    #[msg("Token account mint mismatch")]
    TokenMintMismatch,
    #[msg("Areal fee account does not match revenue config")]
    InvalidFeeAccount,
    #[msg("Initial authority cannot be zero address")]
    InvalidInitialAuthority,
    #[msg("New authority cannot be zero address")]
    ZeroAuthority,
}
