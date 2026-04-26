use arlex_lang::prelude::*;

#[error_code]
pub enum YdError {
    #[msg("Signer is not the authority")]
    Unauthorized,
    #[msg("Signer is not the publish authority")]
    UnauthorizedPublisher,
    #[msg("YD system is paused")]
    SystemPaused,
    #[msg("Distributor is not active")]
    DistributorNotActive,
    #[msg("Root not yet published (epoch == 0)")]
    RootNotPublished,
    #[msg("Merkle proof too long (max 20)")]
    ProofTooLong,
    #[msg("Merkle proof verification failed")]
    InvalidProof,
    #[msg("max_total_claim must equal total_funded")]
    InvalidMaxClaim,
    #[msg("max_total_claim must be > 0")]
    ZeroMaxClaim,
    #[msg("max_total_claim below total_claimed")]
    MaxClaimBelowClaimed,
    #[msg("Total claimed would exceed max_total_claim")]
    ExceedsMaxClaim,
    #[msg("Amount must be > 0")]
    ZeroAmount,
    #[msg("Amount below minimum distribution")]
    BelowMinDistribution,
    #[msg("Vesting period must be > 0")]
    InvalidVestingPeriod,
    #[msg("Invalid token account or mint mismatch")]
    InvalidTokenAccount,
    #[msg("protocol_fee_bps must be <= 10_000")]
    InvalidFeeBps,
    #[msg("ot_mint does not match distributor's expected ot_mint")]
    InvalidOtMint,
    #[msg("reward_vault does not match distributor.reward_vault")]
    InvalidRewardVault,
    #[msg("fee_account does not match config.areal_fee_destination")]
    InvalidFeeAccount,
    #[msg("ClaimStatus claimant/distributor mismatch (replay guard)")]
    InvalidClaimStatus,
    #[msg("claimant_token owner does not match claimant signer")]
    InvalidClaimantTokenOwner,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Cannot transfer authority to yourself")]
    SelfTransfer,
    #[msg("No pending authority transfer")]
    NoPendingAuthority,
    #[msg("Signer is not the pending authority")]
    InvalidPendingAuthority,
    #[msg("Destination address cannot be zero")]
    ZeroDestination,
    #[msg("LiquidityHolding PDA already initialized")]
    LiquidityHoldingAlreadyInitialized,
    #[msg("LiquidityHolding PDA not yet initialized")]
    LiquidityHoldingNotInitialized,
    #[msg("LiquidityHolding withdraw is paused (is_active == false)")]
    LiquidityHoldingNotActive,
    #[msg("liquidity_holding_ata mint or owner mismatch")]
    InvalidLiquidityHoldingAta,
    #[msg("Insufficient LiquidityHolding ATA balance for the requested withdraw amount")]
    InsufficientLiquidityHoldingBalance,
    #[msg("dex_program does not match pinned NEXUS_HOSTING_PROGRAM_ID")]
    InvalidNexusHostingProgram,
    #[msg("Legacy: Layer 9 Nexus not initialized — superseded by R20 (kept for ABI stability)")]
    NexusNotInitialized,
    #[msg("dex_program does not match pinned DEX_PROGRAM_ID")]
    InvalidDexProgram,
    #[msg("rwt_engine_program does not match pinned RWT_ENGINE_PROGRAM_ID")]
    InvalidRwtProgram,
    #[msg("Accumulator USDC/RWT ATA owner or mint mismatch")]
    InvalidAccumulatorAta,
    #[msg("rwt_acquired below caller-specified min_rwt_out")]
    ConversionSlippage,
    #[msg("No USDC available to convert (or zero RWT acquired)")]
    NoUsdcToConvert,
}
