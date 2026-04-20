use arlex_lang::prelude::*;

#[event]
pub struct ConfigInitialized {
    pub authority: [u8; 32],
    pub publish_authority: [u8; 32],
    pub protocol_fee_bps: u16,
    pub areal_fee_destination: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct DistributorCreated {
    pub ot_mint: [u8; 32],
    pub reward_vault: [u8; 32],
    pub accumulator: [u8; 32],
    pub vesting_period_secs: i64,
    pub timestamp: i64,
}

#[event]
pub struct DistributorFunded {
    pub ot_mint: [u8; 32],
    pub amount: u64,
    // NOTE: renamed from `fee` → `protocol_fee` for IDL clarity.
    // Byte layout is unchanged (same u64 at the same offset) so bot parsers
    // that read by offset remain compatible. Only the field name in IDL changes.
    pub protocol_fee: u64,
    pub total_funded: u64,
    pub locked_vested: u64,
    pub timestamp: i64,
}

#[event]
pub struct RootPublished {
    pub ot_mint: [u8; 32],
    pub epoch: u64,
    pub merkle_root: [u8; 32],
    pub max_total_claim: u64,
    pub timestamp: i64,
}

#[event]
pub struct RewardsClaimed {
    pub claimant: [u8; 32],
    pub ot_mint: [u8; 32],
    pub amount: u64,
    pub cumulative_claimed: u64,
    pub timestamp: i64,
}

#[event]
pub struct ConfigUpdated {
    pub protocol_fee_bps: u16,
    pub min_distribution_amount: u64,
    pub is_active: bool,
    pub timestamp: i64,
}

#[event]
pub struct PublishAuthorityUpdated {
    pub old_publish_authority: [u8; 32],
    pub new_publish_authority: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct DistributorClosed {
    pub ot_mint: [u8; 32],
    pub unclaimed_swept: u64,
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
