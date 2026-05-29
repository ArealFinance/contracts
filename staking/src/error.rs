use arlex_lang::prelude::*;

/// Error codes for the `staking` (stRWT) program.
/// Source of truth: `docs/contracts/staking.mdx` (Errors section).
/// Base 6000 (Anchor-compatible) assigned in declaration order.
#[error_code]
pub enum StakingError {
    // ----- Access control -----
    #[msg("Signer is not the authority")]
    Unauthorized,
    #[msg("Signer is not the pause authority")]
    UnauthorizedPause,
    #[msg("Signer is not the reward depositor")]
    UnauthorizedRewardDepositor,

    // ----- Lifecycle -----
    #[msg("Staking is paused")]
    StakingPaused,

    // ----- Stake inputs -----
    #[msg("Stake below minimum")]
    BelowMinStake,
    #[msg("Stake would mint 0 stRWT")]
    ZeroStrwtOutput,
    #[msg("Unstake would release 0 RWT")]
    ZeroRwtOutput,
    #[msg("Output below minimum (slippage protection)")]
    SlippageExceeded,

    // ----- Cooldown / tickets -----
    #[msg("Cooldown has not elapsed")]
    CooldownNotElapsed,
    #[msg("Ticket owner does not match signer")]
    TicketOwnerMismatch,

    // ----- Initialize -----
    #[msg("rwt_mint does not match the canonical earn-RWT mint")]
    InvalidRwtMint,

    // ----- Authority transfer (2-step) -----
    #[msg("No pending authority transfer")]
    NoPendingAuthority,
    #[msg("Signer is not the pending authority")]
    InvalidPendingAuthority,
    #[msg("Cannot transfer authority to self")]
    SelfTransfer,

    // ----- Math / accounts -----
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Invalid token account")]
    InvalidTokenAccount,
    #[msg("Address cannot be zero")]
    ZeroAddress,
}
