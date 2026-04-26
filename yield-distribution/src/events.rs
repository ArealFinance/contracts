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

#[event]
pub struct LiquidityHoldingInitialized {
    pub liquidity_holding: [u8; 32],
    pub liquidity_holding_ata: [u8; 32],
    pub payer: [u8; 32],
    pub timestamp: i64,
}

/// Emitted by `convert_to_rwt` after a successful conversion (DEX swap +/or
/// RWT mint, fee transfer, reward-vault credit). Per D2/D12: `amount` is
/// **NET RWT funded** in this transaction (= rwt_acquired − protocol_fee),
/// NOT cumulative `total_funded`. `total_funded` (post-update) is exposed as
/// a separate field so dashboards can read it without touching distributor
/// state.
///
/// Layout (128-byte body, see Layer 8 architecture §6.1):
///
/// ```text
///   0..32   distributor   PDA address (NEW vs DistributorFunded for symmetry)
///  32..64   ot_mint
///  64..72   amount        (net RWT funded this TX — D2)
///  72..80   protocol_fee  (RWT fee taken at outer level)
///  80..88   total_funded  (distributor.total_funded AFTER update)
///  88..96   locked_vested (distributor.locked_vested AFTER update)
///  96..104  timestamp
/// 104..112  usdc_in       (USDC consumed across both legs)
/// 112..120  swap_out_rwt  (RWT acquired via DEX swap)
/// 120..128  mint_out_rwt  (RWT acquired via RWT Engine mint)
/// ```
///
/// D12: distinct layout from `DistributorFunded` (offsets do NOT align with
/// the old fund event because of the `distributor` prefix); bot uses a
/// dedicated parser.
#[event]
pub struct StreamConverted {
    pub distributor: [u8; 32],
    pub ot_mint: [u8; 32],
    pub amount: u64,         // NET funded this TX (D2)
    pub protocol_fee: u64,
    pub total_funded: u64,
    pub locked_vested: u64,
    pub timestamp: i64,
    pub usdc_in: u64,
    pub swap_out_rwt: u64,
    pub mint_out_rwt: u64,
}

// Placeholder — emitted by `withdraw_liquidity_holding` once Layer 9 Nexus
// drains the holding. Layer 8 ix only reverts; this event is reserved for
// the Layer 9 implementation.
#[event]
pub struct LiquidityHoldingWithdrawn {
    pub liquidity_holding: [u8; 32],
    pub recipient: [u8; 32],
    pub nexus_program: [u8; 32],
    pub amount: u64,
    pub total_withdrawn: u64,
    pub timestamp: i64,
}
