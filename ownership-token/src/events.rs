use arlex_lang::prelude::*;

#[event]
pub struct OtInitialized {
    pub ot_mint: [u8; 32],
    pub authority: [u8; 32],
    pub decimals: u8,
    pub timestamp: i64,
}

#[event]
pub struct OtMinted {
    pub ot_mint: [u8; 32],
    pub recipient: [u8; 32],
    pub amount: u64,
    pub new_total_minted: u64,
    pub timestamp: i64,
}

#[event]
pub struct RevenueDistributed {
    pub ot_mint: [u8; 32],
    pub total_amount: u64,
    pub protocol_fee: u64,
    pub distribution_count: u64,
    pub num_destinations: u8,
    pub timestamp: i64,
}

#[event]
pub struct DestinationConfigUpdated {
    pub ot_mint: [u8; 32],
    pub config_version: u64,
    pub active_count: u8,
    pub timestamp: i64,
}

#[event]
pub struct AuthorityTransferProposed {
    pub ot_mint: [u8; 32],
    pub current_authority: [u8; 32],
    pub pending_authority: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct AuthorityTransferAccepted {
    pub ot_mint: [u8; 32],
    pub old_authority: [u8; 32],
    pub new_authority: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct TreasurySpent {
    pub ot_mint: [u8; 32],
    pub token_mint: [u8; 32],
    pub amount: u64,
    pub destination: [u8; 32],
    pub timestamp: i64,
}

/// Emitted by `claim_yd_for_treasury` when the OT Treasury PDA successfully
/// claims vested RWT from a Yield Distribution distributor. Cross-project
/// yield supported: `ot_mint` is THIS treasury's OT, `yd_ot_mint` is the OT
/// of the source distributor (may differ for ARL Treasury claiming RCP yield).
/// See Layer 8 architecture §6.4. Body layout: 32+32+8+8 = 80 bytes.
#[event]
pub struct TreasuryYieldClaimed {
    pub ot_mint: [u8; 32],
    pub yd_ot_mint: [u8; 32],
    pub amount: u64,
    pub timestamp: i64,
}
