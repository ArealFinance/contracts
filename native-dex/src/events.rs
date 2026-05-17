use arlex_lang::prelude::*;

#[event]
pub struct DexInitialized {
    pub authority: [u8; 32],
    pub base_fee_bps: u16,
    pub timestamp: i64,
}

#[event]
pub struct PoolCreated {
    pub pool: [u8; 32],
    pub token_a_mint: [u8; 32],
    pub token_b_mint: [u8; 32],
    pub pool_type: u8,
    pub creator: [u8; 32],
    pub ot_treasury_fee_destination: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct LiquidityAdded {
    pub pool: [u8; 32],
    pub provider: [u8; 32],
    pub amount_a: u64,
    pub amount_b: u64,
    pub shares_minted: u128,
    pub timestamp: i64,
}

#[event]
pub struct ZapLiquidityExecuted {
    pub pool: [u8; 32],
    pub provider: [u8; 32],
    pub input_a: u64,
    pub input_b: u64,
    pub swapped_amount: u64,
    pub shares_minted: u128,
    pub timestamp: i64,
}

#[event]
pub struct LiquidityRemoved {
    pub pool: [u8; 32],
    pub provider: [u8; 32],
    pub amount_a: u64,
    pub amount_b: u64,
    pub shares_burned: u128,
    pub timestamp: i64,
}

#[event]
pub struct SwapExecuted {
    pub pool: [u8; 32],
    pub user: [u8; 32],
    pub a_to_b: bool,
    pub amount_in: u64,
    pub amount_out: u64,
    pub fee_lp: u64,
    pub fee_protocol: u64,
    pub fee_ot_treasury: u64,
    pub timestamp: i64,
}

#[event]
pub struct PoolCreatorsUpdated {
    pub wallet: [u8; 32],
    pub action: u8,
    pub active_count: u8,
    pub timestamp: i64,
}

#[event]
pub struct DexConfigUpdated {
    pub base_fee_bps: u16,
    pub lp_fee_share_bps: u16,
    pub rebalancer: [u8; 32],
    pub is_active: bool,
    pub timestamp: i64,
}

/// Emitted by `update_areal_fee_destination` when the authority rotates
/// `DexConfig.areal_fee_destination` to a new RWT ATA. Captures both the
/// previous and new destinations for off-chain audit trails. The
/// instruction is idempotent: when called with the current destination,
/// state is left unchanged and this event is NOT emitted.
/// Body layout: 32 + 32 + 8 = 72 bytes.
#[event]
pub struct ArealFeeDestinationUpdated {
    pub old_destination: [u8; 32],
    pub new_destination: [u8; 32],
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
pub struct PoolPaused {
    pub pool: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct PoolUnpaused {
    pub pool: [u8; 32],
    pub timestamp: i64,
}

/// Emitted by `compound_yield` when the pool PDA successfully claims vested
/// RWT from a Yield Distribution distributor and folds the received amount
/// back into `reserve_<rwt_side>`. `rwt_side` encodes which side of the pair
/// is RWT: `0 = side A`, `1 = side B`. `reserve_after` is the post-compound
/// pool reserve on the RWT side (snapshot for analytics / dashboard).
/// See Layer 8 architecture §6.3. Body layout: 32+32+8+1+8+8 = 89 bytes.
#[event]
pub struct CompoundYieldExecuted {
    pub pool: [u8; 32],
    pub ot_mint: [u8; 32],
    pub rwt_claimed: u64,
    pub rwt_side: u8,
    pub reserve_after: u64,
    pub timestamp: i64,
}

// ----- Layer 9 (Liquidity Nexus) -----
//
// Events emitted by the 9 Nexus instructions. Manager/Authority operations
// (swap, add_liquidity, remove_liquidity) reuse the existing `SwapExecuted`,
// `LiquidityAdded`, `LiquidityRemoved` events — only the 5 events below are
// new in Layer 9 (per architecture §6 / docs §Events table).
//
// Field naming follows docs canonical (`docs/contracts/native-dex.mdx`
// §Events). All `Pubkey` slots use `[u8; 32]` to stay compatible with
// `repr(C, packed)` event payloads consumed by parity tests.

/// Emitted once by `initialize_nexus`. Records the initial manager and the
/// genesis timestamp of the Nexus singleton.
/// Body layout: 32 + 8 = 40 bytes.
#[event]
pub struct NexusInitialized {
    pub manager: [u8; 32],
    pub timestamp: i64,
}

/// Emitted by `nexus_deposit` (permissionless caller) and by
/// `nexus_record_deposit` (CPI target invoked by YD's
/// `withdraw_liquidity_holding`). `source_kind` distinguishes the two
/// origins per Layer 9 architecture §4.2 / §4.9:
/// - `NEXUS_DEPOSIT_SOURCE_DIRECT (0)` — direct `nexus_deposit` call.
/// - `NEXUS_DEPOSIT_SOURCE_LIQUIDITY_HOLDING (1)` — YD CPI drain.
///
/// `new_total_deposited` is the post-bump value of the matching
/// `LiquidityNexus.total_deposited_*` counter (USDC or RWT depending on
/// `token_mint`); indexers should treat the counter as monotonically
/// non-decreasing across consecutive events for the same token.
/// Body layout: 32 + 8 + 8 + 1 + 8 = 57 bytes.
#[event]
pub struct NexusDeposited {
    pub token_mint: [u8; 32],
    pub amount: u64,
    pub new_total_deposited: u64,
    pub source_kind: u8,
    pub timestamp: i64,
}

/// Emitted by `nexus_withdraw_profits`. `amount` is the withdrawn amount;
/// `remaining_profit` is the post-withdrawal `(ata_balance - principal_floor)`
/// gap (zero when the entire profit was swept). `treasury_destination` is the
/// resolved ARL Treasury ATA receiving the funds (mirrors
/// `dex_config.areal_fee_destination` for the matching token). Layer 9 §4.6.
/// Body layout: 32 + 8 + 8 + 32 + 8 = 88 bytes.
#[event]
pub struct NexusProfitsWithdrawn {
    pub token_mint: [u8; 32],
    pub amount: u64,
    pub remaining_profit: u64,
    pub treasury_destination: [u8; 32],
    pub timestamp: i64,
}

/// Emitted by `nexus_claim_rewards`. Authority-driven LP-fee sweep from a
/// pool's `fee_vault` to the ARL Treasury, signed by the Nexus PDA.
/// `amount` is the claimable RWT delta computed from the pool's
/// `cumulative_fees_per_share`. Layer 9 §4.7.
/// Body layout: 8 + 32 + 8 = 48 bytes.
#[event]
pub struct NexusRewardsClaimed {
    pub amount: u64,
    pub treasury_destination: [u8; 32],
    pub timestamp: i64,
}

/// Emitted by `update_nexus_manager`. Captures both the previous manager and
/// the new manager (which may equal the zero kill-switch pubkey per D22).
/// Body layout: 32 + 32 + 8 = 72 bytes.
#[event]
pub struct NexusManagerUpdated {
    pub old_manager: [u8; 32],
    pub new_manager: [u8; 32],
    pub timestamp: i64,
}

// ----- Layer 9 D28 (LP-fee accumulator + claim_lp_fees) -----

/// Emitted by `claim_lp_fees` when an LP realises accumulator-tracked
/// fees on either side of the pair. `claimable_a` / `claimable_b` are
/// post-truncation u64s (Q64.64 → integer via `>> 64`). Both may be
/// non-zero when the position has accrued fees on both sides since the
/// last claim. Body layout: 32 + 32 + 8 + 8 + 8 = 88 bytes.
#[event]
pub struct LpFeesClaimed {
    pub recipient: [u8; 32],
    pub pool: [u8; 32],
    pub claimable_a: u64,
    pub claimable_b: u64,
    pub timestamp: i64,
}
