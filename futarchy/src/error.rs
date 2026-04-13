use arlex_lang::prelude::*;

#[error_code]
pub enum FutarchyError {
    #[msg("Signer is not the authority")]
    Unauthorized,
    #[msg("Governance is not active")]
    GovernancePaused,
    #[msg("Proposal status is not Active")]
    ProposalNotActive,
    #[msg("Proposal status is not Approved")]
    ProposalNotApproved,
    #[msg("Proposal has already been executed")]
    AlreadyExecuted,
    #[msg("Unknown proposal type")]
    InvalidProposalType,
    #[msg("Arithmetic overflow")]
    MathOverflow,
    #[msg("Cannot transfer authority to yourself")]
    SelfTransfer,
    #[msg("No pending authority transfer")]
    NoPendingAuthority,
    #[msg("Signer is not the pending authority")]
    InvalidPendingAuthority,
    #[msg("Amount must be > 0")]
    ZeroAmount,
    #[msg("Destination cannot be zero address")]
    ZeroDestination,
    #[msg("Params hash cannot be all zeros")]
    EmptyParamsHash,
    #[msg("Hash of provided destinations does not match proposal params_hash")]
    ParamsHashMismatch,
    #[msg("Executor account does not match proposal destination")]
    DestinationMismatch,
    #[msg("Token mint does not match proposal token_mint")]
    TokenMintMismatch,
    #[msg("OT governance pending_authority does not match this Futarchy config")]
    GovernanceClaimMismatch,
    #[msg("OT program account does not match OT_PROGRAM_ID")]
    InvalidOtProgram,
    #[msg("OT governance PDA derivation mismatch")]
    InvalidOtGovernance,
    #[msg("Proposal does not belong to this Futarchy config")]
    ProposalConfigMismatch,
    #[msg("OT mint account does not match config ot_mint")]
    OtMintMismatch,
}
