use arlex_lang::prelude::*;

#[event]
pub struct FutarchyInitialized {
    pub ot_mint: [u8; 32],
    pub authority: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct ProposalCreated {
    pub proposal_id: u64,
    pub ot_mint: [u8; 32],
    pub proposer: [u8; 32],
    pub proposal_type: u8,
    pub amount: u64,
    pub destination: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct ProposalApproved {
    pub proposal_id: u64,
    pub approver: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct ProposalCancelled {
    pub proposal_id: u64,
    pub authority: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct ProposalExecuted {
    pub proposal_id: u64,
    pub proposal_type: u8,
    pub executor: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct OtGovernanceClaimed {
    pub ot_mint: [u8; 32],
    pub futarchy_config: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct AuthorityTransferProposed {
    pub current_authority: [u8; 32],
    pub pending_authority: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct AuthorityTransferAccepted {
    pub old_authority: [u8; 32],
    pub new_authority: [u8; 32],
    pub timestamp: i64,
}
