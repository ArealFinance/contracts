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
    #[msg("Protocol fee destination mint does not match RWT mint")]
    InvalidProtocolFeeDestination,
    #[msg("Address cannot be zero")]
    ZeroAddress,
    #[msg("Invalid token account")]
    InvalidTokenAccount,
    #[msg("bin_step_bps must be > 0 for concentrated pools")]
    InvalidBinStep,
    #[msg("Bin range out of BinArray bounds")]
    InvalidBinRange,
    #[msg("No liquidity in bins for swap")]
    InsufficientBinLiquidity,
    #[msg("Pool type mismatch")]
    InvalidPoolType,
    // ----- Layer 8 (compound_yield) -----
    #[msg("yd_program account does not match pinned YD_PROGRAM_ID")]
    InvalidYdProgram,
    #[msg("pool PDA does not match expected derivation")]
    InvalidPoolPda,
    #[msg("target_vault does not match pool.vault_a/b on the RWT side")]
    InvalidTargetVault,
    #[msg("neither token_a_mint nor token_b_mint is RWT_MINT")]
    TargetVaultNotRwt,
    #[msg("ot_mint does not match the pool's OT side mint")]
    InvalidOtMint,
    // ----- Layer 9 (Liquidity Nexus) -----
    #[msg("Nexus is_active = false")]
    NexusNotActive,
    #[msg("Signer is not the Nexus manager")]
    InvalidNexusManager,
    #[msg("Nexus manager is the zero pubkey kill-switch")]
    NexusManagerDisabled,
    #[msg("nexus_deposit token_mint is not USDC_MINT or RWT_MINT")]
    InvalidNexusToken,
    #[msg("Withdraw amount exceeds (ATA balance - principal floor) profit")]
    InsufficientNexusProfit,
    #[msg("Nexus PDA not found in merkle tree or proof invalid")]
    NexusClaimFailed,
    #[msg("Nexus LP position is missing or owner mismatch")]
    InvalidNexusLpPosition,
    #[msg("nexus_record_deposit may only be invoked via CPI from Yield Distribution")]
    NexusRecordDepositOnlyFromYd,
    #[msg("LiquidityHolding PDA derivation does not match passed account")]
    InvalidLiquidityHoldingPda,
    // ----- Layer 9 D28 (LP-fee accumulator + claim_lp_fees) -----
    #[msg("LpPosition.pool does not match the supplied pool_state")]
    InvalidLpPosition,
    // ----- CP-3 Monotonic Ladder redistribute helpers -----
    #[msg("grow_redistribute called with new_nav_bin <= last_rebalance_nav_bin")]
    NotGrowthDirection,
    #[msg("compress_redistribute called with new_nav_bin >= last_rebalance_nav_bin")]
    NotCompressionDirection,
    #[msg("Active zone lower edge would dip below left_anchor_bin (overlaps permanent tail)")]
    ActiveZoneOverlapsTail,
    #[msg("new_nav_bin too close to the BinArray upper edge — exceeds right-edge buffer")]
    ExceedsRightEdgeBuffer,
    // ----- CP-4 create_concentrated_pool (Monotonic Ladder init) -----
    #[msg("Non-RWT side of master pool must be USDC_MINT or USDY_MINT")]
    InvalidMintPair,
    #[msg("permanent_tail_offset_bps must be in [MIN_PERMANENT_TAIL_OFFSET_BPS, +∞)")]
    InvalidPermanentTailOffset,
    #[msg("Master pools cannot carry OT-treasury accounts")]
    OtTreasuryNotAllowedOnMasterPool,
}
