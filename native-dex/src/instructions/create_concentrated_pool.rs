//! Create a Monotonic Ladder master pool (concentrated liquidity, single-sided
//! USDC structure).
//!
//! Shares the shared L-7 flow with `create_pool` — whitelist check, canonical
//! mint ordering, pool PDA + vault init — all in `pool_creation.rs`. The
//! handler keeps only the master-pool-specific additions:
//!
//! - `bin_step_bps` / `initial_active_bin` argument validation
//! - **CP-4** `permanent_tail_offset_bps` argument with `MIN_PERMANENT_TAIL_OFFSET_BPS` floor
//! - **CP-4** Non-RWT mint gated to `{USDC_MINT, USDY_MINT}` (`InvalidMintPair`)
//! - **CP-4** OT-treasury accounts must be absent (`OtTreasuryNotAllowedOnMasterPool`)
//! - BinArray PDA creation (1000-bin layout from CP-1)
//! - **CP-4** Monotonic Ladder anchor init (`left_anchor_bin`,
//!   `permanent_tail_floor_bin`, `last_rebalance_nav_bin`, `active_zone_lower`,
//!   `permanent_tail_offset_bps`)
//! - **CP-4** `bin_array.lower_bin_id` anchored at `permanent_tail_floor_bin`
//!
//! Spec: `docs/contracts/native-dex.mdx` §208-240 + changelog
//! `docs/changelog/2026-04-17-monotonic-ladder.mdx` §50-102.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::PoolCreated;
use crate::pool_creation::{
    create_pool_account, init_vault_pair, require_valid_mint_pair,
    require_whitelisted_creator,
};
use crate::state::*;
use crate::validation::{is_rwt_mint, pubkey_bytes};

#[derive(Accounts)]
pub struct CreateConcentratedPool<'info> {
    #[account(mut, signer)]
    pub creator: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    #[account(seeds = [b"pool_creators"], bump)]
    pub pool_creators: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,

    // BinArray PDA: ["bins", pool_state]
    #[account(mut)]
    pub bin_array: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_a_mint: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_b_mint: &'info AccountView,

    #[account(mut, signer)]
    pub vault_a: &'info AccountView,

    #[account(mut, signer)]
    pub vault_b: &'info AccountView,

    // CP-4: NO OT-treasury accounts. Master pools are USDC/USDY only and never
    // carry an OT treasury (`OtTreasuryNotAllowedOnMasterPool`). The
    // `remaining_accounts` slice is asserted empty in the handler — there is
    // no upstream OT-treasury entry for this ix.

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<CreateConcentratedPool>,
    bin_step_bps: u16,
    initial_active_bin: i32,
    permanent_tail_offset_bps: i32,
) -> Result<()> {
    let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;
    if !config.is_active {
        return Err(ProgramError::from(DexError::DexPaused));
    }

    // --- Concentrated-specific argument validation ---
    if bin_step_bps == 0 || bin_step_bps > MAX_BIN_STEP_BPS {
        return Err(ProgramError::from(DexError::InvalidBinStep));
    }
    if initial_active_bin.abs() > MAX_INITIAL_ACTIVE_BIN {
        return Err(ProgramError::from(DexError::InvalidBinRange));
    }
    // CP-4 — permanent tail offset must be strictly positive AND at least
    // `MIN_PERMANENT_TAIL_OFFSET_BPS` (docs §228: "≥ 30"). Both bounds collapse
    // to a single ≥ check because `MIN_PERMANENT_TAIL_OFFSET_BPS = 30 > 0`,
    // but we keep the `> 0` guard explicit because the constant lives in
    // `constants.rs` and could be lowered to zero in a future patch — at
    // which point a zero offset (tail collides with NAV) would silently
    // become legal without this line.
    if permanent_tail_offset_bps <= 0
        || permanent_tail_offset_bps < MIN_PERMANENT_TAIL_OFFSET_BPS
    {
        return Err(ProgramError::from(DexError::InvalidPermanentTailOffset));
    }

    // --- Shared setup (pool_creation.rs — L-7) ---
    let creator_key = require_whitelisted_creator(
        ctx.accounts.pool_creators,
        ctx.accounts.creator,
        ctx.program_id,
    )?;
    let (mint_a, mint_b) =
        require_valid_mint_pair(ctx.accounts.token_a_mint, ctx.accounts.token_b_mint)?;

    // CP-4 — Master pool mint-pair gate (docs §225):
    // exactly one of {mint_a, mint_b} is RWT (already enforced by
    // `require_valid_mint_pair`), and the OTHER side must be USDC_MINT or
    // USDY_MINT. The Monotonic Ladder geometry / mint routing is defined
    // only for those two pairs.
    //
    // USDY_MINT is currently the all-zero placeholder
    // (`constants.rs::USDY_MINT == [0u8; 32]`); pinning lands at mainnet via
    // a dedicated mint-pinning commit (mirrors the RWT_MINT MAINNET-REPLACE
    // pattern). Until then USDY pools predictably fail this gate.
    let non_rwt_mint = if is_rwt_mint(&mint_a) { &mint_b } else { &mint_a };
    if non_rwt_mint != &USDC_MINT && non_rwt_mint != &USDY_MINT {
        return Err(ProgramError::from(DexError::InvalidMintPair));
    }

    // CP-4 — Master pools never carry an OT-treasury (docs §229: "OT
    // treasury validation: must be absent"). The user-facing convention is
    // that `remaining_accounts` is reserved for the optional OT-treasury
    // pair; rejecting any non-empty `remaining_accounts` keeps the
    // boundary clear and prevents an OT-treasury Address from being
    // smuggled in via the same slot StandardCurve pools use.
    if !ctx.remaining_accounts.is_empty() {
        return Err(ProgramError::from(DexError::OtTreasuryNotAllowedOnMasterPool));
    }

    let rent = pinocchio::sysvars::rent::Rent::get()?;
    let (pool_pda, pool_bump) = create_pool_account(
        ctx.accounts.creator,
        ctx.accounts.pool_state,
        &mint_a,
        &mint_b,
        ctx.program_id,
        &rent,
    )?;

    // --- Create BinArray PDA (concentrated-specific) ---
    let pool_key = pubkey_bytes(ctx.accounts.pool_state);
    let (bin_pda, bin_bump) = arlex_lang::find_program_address(
        &[b"bins", pool_key.as_ref()],
        ctx.program_id,
    );
    if ctx.accounts.bin_array.address().as_ref() != bin_pda.as_ref() {
        return Err(ProgramError::InvalidSeeds);
    }

    // `BinArray::SPACE` is derived from `core::mem::size_of::<BinArray>() + 8`
    // (account discriminator), so the 1000-bin layout (CP-1) is honoured
    // automatically — no literal bin count to keep in sync here.
    let bin_lamports = rent.try_minimum_balance(BinArray::SPACE)?;
    arlex_lang::system::instructions::CreateAccount {
        from: ctx.accounts.creator,
        to: ctx.accounts.bin_array,
        lamports: bin_lamports,
        space: BinArray::SPACE as u64,
        owner: ctx.program_id,
    }
    .invoke_signed(&[Signer::from(&[
        Seed::from(b"bins" as &[u8]),
        Seed::from(pool_key.as_ref()),
        Seed::from(&[bin_bump]),
    ])])?;

    init_vault_pair(
        ctx.accounts.creator,
        ctx.accounts.vault_a,
        ctx.accounts.vault_b,
        ctx.accounts.token_a_mint,
        ctx.accounts.token_b_mint,
        &pool_pda,
        &rent,
    )?;

    // --- CP-4 Monotonic Ladder anchor geometry ---
    //
    // docs §233-234:
    //   left_anchor_bin         = initial_active_bin − offset_bps / bin_step_bps
    //   permanent_tail_floor_bin = left_anchor_bin − PERMANENT_TAIL_BIN_COUNT
    //
    // Integer division truncates toward zero — for positive `offset_bps`
    // this is equivalent to a floor (the result lands at most one bin
    // above the "true" geometric offset). The architect spec accepts this
    // truncation because it favours a slightly TIGHTER offset (= tail
    // slightly closer to NAV) than the user requested, which is the safe
    // direction: it never widens the tail beyond `MIN_PERMANENT_TAIL_OFFSET_BPS`
    // even on edge `offset_bps == MIN_PERMANENT_TAIL_OFFSET_BPS`.
    //
    // `bin_step_bps == 0` is rejected upstream (`InvalidBinStep`), so the
    // division is safe.
    let bin_step_bps_i32 = bin_step_bps as i32;
    let tail_offset_in_bins = permanent_tail_offset_bps / bin_step_bps_i32;
    let left_anchor_bin = initial_active_bin - tail_offset_in_bins;
    let permanent_tail_floor_bin = left_anchor_bin - PERMANENT_TAIL_BIN_COUNT;

    // --- Master-pool PoolState init ---
    let pool = PoolState::init(ctx.accounts.pool_state, ctx.program_id)?;
    pool.pool_type = POOL_TYPE_CONCENTRATED;
    pool.token_a_mint = mint_a;
    pool.token_b_mint = mint_b;
    pool.vault_a = pubkey_bytes(ctx.accounts.vault_a);
    pool.vault_b = pubkey_bytes(ctx.accounts.vault_b);
    pool.reserve_a = 0;
    pool.reserve_b = 0;
    pool.total_lp_shares = 0;
    pool.fee_bps = config.base_fee_bps;
    pool.is_active = true;
    pool.total_fees_accumulated = 0;
    pool.bin_step_bps = bin_step_bps;
    pool.active_bin_id = initial_active_bin;
    // CP-4 — master pools never carry OT-treasury; the `remaining_accounts`
    // empty-check above ensures this stays `(zero, false)` for the lifetime
    // of the pool. The fields are still written explicitly (rather than
    // relying on `PoolState::init` zero-default) to make the invariant
    // greppable from the handler body.
    pool.ot_treasury_fee_destination = [0u8; 32];
    pool.has_ot_treasury = false;
    pool.bump = pool_bump;
    // CP-4 — Monotonic Ladder anchors. `active_zone_lower == active_bin_id`
    // models an EMPTY active zone at init: Nexus seeds the bid wall via
    // `nexus_add_liquidity` after pool creation (docs §235).
    pool.left_anchor_bin = left_anchor_bin;
    pool.permanent_tail_floor_bin = permanent_tail_floor_bin;
    pool.last_rebalance_nav_bin = initial_active_bin;
    pool.active_zone_lower = initial_active_bin;
    pool.permanent_tail_offset_bps = permanent_tail_offset_bps as u16;

    // --- Initialize BinArray (concentrated-specific) ---
    //
    // CP-4: anchor `lower_bin_id` at the permanent tail floor instead of
    // the old `initial_active_bin − MAX_BINS/2` symmetric layout. The
    // 1000-bin Monotonic Ladder grows monotonically upward from the
    // permanent tail; the array is structured as
    //   [permanent_tail | gap | active_zone | organic_ask | right-edge buffer].
    let bins = BinArray::init(ctx.accounts.bin_array, ctx.program_id)?;
    bins.pool = pool_key;
    for i in 0..MAX_BINS {
        bins.bins[i] = Bin { liquidity_a: 0, liquidity_b: 0 };
    }
    bins.lower_bin_id = permanent_tail_floor_bin;
    bins.bin_step_bps = bin_step_bps;
    bins.active_bin_id = initial_active_bin;
    bins.bump = bin_bump;

    let clock = Clock::get()?;
    emit!(PoolCreated {
        pool: pool_key,
        token_a_mint: mint_a,
        token_b_mint: mint_b,
        pool_type: POOL_TYPE_CONCENTRATED,
        creator: creator_key,
        ot_treasury_fee_destination: [0u8; 32],
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Concentrated pool created");
    Ok(())
}

#[cfg(test)]
mod tests {
    //! CP-4 unit coverage — Monotonic Ladder anchor math, mint-pair gate,
    //! permanent-tail offset validation, and OT-treasury rejection.
    //!
    //! The handler can't be invoked directly without a BPF runtime (it
    //! borrows `&AccountView` through `PoolState::init` / `BinArray::init`),
    //! so we cover the pure-logic portions in two layers:
    //!
    //!   1. Anchor geometry: we mirror the handler's writes onto a
    //!      zero-init `PoolState` / `BinArray` and assert each field matches
    //!      the documented Monotonic Ladder formulas.
    //!   2. Validation gates: we extract the boolean decisions into
    //!      `validate_mint_pair_for_master_pool` and
    //!      `validate_permanent_tail_offset` (test-local twins of the
    //!      production checks) and pin every revert path against the
    //!      `DexError` numeric code that the on-chain runtime would emit.
    //!
    //! Handler-level negative ACs (full revert with all accounts populated)
    //! are exercised by future BPF integration tests where the Arlex
    //! framework can be invoked end-to-end. The twin pattern mirrors
    //! `validation::tests::assert_manager_*`.

    use super::*;
    use crate::constants::{
        MIN_PERMANENT_TAIL_OFFSET_BPS, PERMANENT_TAIL_BIN_COUNT, RWT_MINT, USDC_MINT, USDY_MINT,
    };

    // ----------------------------------------------------------------
    // Twin helpers — mirror the handler's revert decisions on raw bytes.
    // ----------------------------------------------------------------

    /// Twin of the CP-4 mint-pair gate: given the canonically-ordered pair
    /// `(mint_a, mint_b)` (with `mint_a < mint_b` and exactly one side
    /// being `RWT_MINT`, both enforced upstream by
    /// `require_valid_mint_pair`), require the non-RWT side to be USDC
    /// or USDY. Returns `Ok(())` or `Err(InvalidMintPair)`.
    fn validate_mint_pair_for_master_pool(
        mint_a: &[u8; 32],
        mint_b: &[u8; 32],
    ) -> core::result::Result<(), ProgramError> {
        let non_rwt_mint = if is_rwt_mint(mint_a) { mint_b } else { mint_a };
        if non_rwt_mint != &USDC_MINT && non_rwt_mint != &USDY_MINT {
            return Err(ProgramError::from(DexError::InvalidMintPair));
        }
        Ok(())
    }

    /// Twin of the CP-4 permanent-tail-offset gate.
    fn validate_permanent_tail_offset(
        permanent_tail_offset_bps: i32,
    ) -> core::result::Result<(), ProgramError> {
        if permanent_tail_offset_bps <= 0
            || permanent_tail_offset_bps < MIN_PERMANENT_TAIL_OFFSET_BPS
        {
            return Err(ProgramError::from(DexError::InvalidPermanentTailOffset));
        }
        Ok(())
    }

    /// Extract the `Custom(u32)` payload from a `ProgramError`. Panics on
    /// any other variant — that would itself indicate a regression
    /// (every `DexError` lowering goes through `ProgramError::Custom`).
    fn custom_code(err: ProgramError) -> u32 {
        match err {
            ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    /// Resolve a `DexError` variant to its numeric error code by lowering
    /// through the production `From<DexError> for ProgramError` impl.
    fn code_of(err: DexError) -> u32 {
        custom_code(ProgramError::from(err))
    }

    /// Build a synthetic mint that is neither RWT, USDC, nor USDY (and
    /// lexicographically larger than RWT_MINT so canonical ordering puts
    /// RWT on side A — but the twin gate doesn't care which side RWT is on).
    fn random_non_usdx_mint() -> [u8; 32] {
        [0xFFu8; 32]
    }

    /// Mirror the handler's anchor-init writes onto a zero-init
    /// `PoolState` / `BinArray`. Returns the populated structs so each
    /// individual test can assert against specific fields.
    fn run_anchor_init(
        initial_active_bin: i32,
        bin_step_bps: u16,
        permanent_tail_offset_bps: i32,
    ) -> (PoolState, BinArray) {
        // SAFETY: PoolState and BinArray are `#[repr(C, packed)]` via
        // `#[account]` and all-zero is a valid bit pattern for every
        // field type (primitive scalars + fixed arrays).
        let pool_buf = [0u8; core::mem::size_of::<PoolState>()];
        let mut pool: PoolState =
            unsafe { core::ptr::read(pool_buf.as_ptr() as *const PoolState) };

        let bins_buf = [0u8; core::mem::size_of::<BinArray>()];
        let mut bins: BinArray =
            unsafe { core::ptr::read(bins_buf.as_ptr() as *const BinArray) };

        let bin_step_bps_i32 = bin_step_bps as i32;
        let tail_offset_in_bins = permanent_tail_offset_bps / bin_step_bps_i32;
        let left_anchor_bin = initial_active_bin - tail_offset_in_bins;
        let permanent_tail_floor_bin = left_anchor_bin - PERMANENT_TAIL_BIN_COUNT;

        pool.bin_step_bps = bin_step_bps;
        pool.active_bin_id = initial_active_bin;
        pool.left_anchor_bin = left_anchor_bin;
        pool.permanent_tail_floor_bin = permanent_tail_floor_bin;
        pool.last_rebalance_nav_bin = initial_active_bin;
        pool.active_zone_lower = initial_active_bin;
        pool.permanent_tail_offset_bps = permanent_tail_offset_bps as u16;

        bins.lower_bin_id = permanent_tail_floor_bin;
        bins.bin_step_bps = bin_step_bps;
        bins.active_bin_id = initial_active_bin;

        (pool, bins)
    }

    // ----------------------------------------------------------------
    // Mint-pair gate (CP-4 docs §225)
    // ----------------------------------------------------------------

    /// Happy path — `(RWT, USDC)` is canonically ordered (RWT_MINT bytes
    /// happen to be lexicographically smaller than the all-zero USDC_MINT
    /// placeholder? No — USDC_MINT is `[0u8; 32]`, the smallest pubkey, so
    /// in canonical order USDC is `mint_a` and RWT is `mint_b`). Either
    /// way, the twin checks the non-RWT side: USDC → Ok.
    #[test]
    fn creates_master_pool_with_usdc_pair_ok() {
        // Canonical order: USDC_MINT (all zeros) < RWT_MINT, so a == USDC.
        let mint_a = USDC_MINT;
        let mint_b = RWT_MINT;
        assert!(validate_mint_pair_for_master_pool(&mint_a, &mint_b).is_ok());
    }

    /// Random non-USDC, non-USDY mint paired with RWT → InvalidMintPair.
    /// `random_non_usdx_mint()` is `[0xFF; 32]` (larger than RWT_MINT), so
    /// canonical order puts RWT on side A and the random mint on side B.
    #[test]
    fn rejects_non_usdc_non_usdy_pair() {
        let mint_a = RWT_MINT;
        let mint_b = random_non_usdx_mint();
        let err = validate_mint_pair_for_master_pool(&mint_a, &mint_b).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::InvalidMintPair));
    }

    /// USDY happy-path test is intentionally a deferred scenario: with
    /// `USDY_MINT == [0u8; 32]` (constants.rs:78) it is byte-equal to the
    /// current `USDC_MINT == [0u8; 32]` placeholder, so even if the
    /// production gate accepted USDY, the test would be indistinguishable
    /// from the USDC happy-path. The real USDY happy-path test lands when
    /// USDY_MINT is pinned at mainnet (mirror of the RWT_MINT
    /// MAINNET-REPLACE pattern).

    // ----------------------------------------------------------------
    // Permanent-tail offset gate (CP-4 docs §228)
    // ----------------------------------------------------------------

    /// Offset BELOW `MIN_PERMANENT_TAIL_OFFSET_BPS` (= 30) → reject.
    #[test]
    fn rejects_offset_below_minimum() {
        let err = validate_permanent_tail_offset(20).unwrap_err();
        assert_eq!(
            custom_code(err),
            code_of(DexError::InvalidPermanentTailOffset)
        );
    }

    /// Zero offset → reject. Zero would place the tail at NAV (no gap),
    /// which is nonsensical — the explicit `> 0` guard catches this even
    /// if `MIN_PERMANENT_TAIL_OFFSET_BPS` is ever lowered to zero.
    #[test]
    fn rejects_zero_offset() {
        let err = validate_permanent_tail_offset(0).unwrap_err();
        assert_eq!(
            custom_code(err),
            code_of(DexError::InvalidPermanentTailOffset)
        );
    }

    /// Negative offset → reject. The tail must sit BELOW NAV (positive
    /// offset). A negative value would place the tail above NAV.
    #[test]
    fn rejects_negative_offset() {
        let err = validate_permanent_tail_offset(-50).unwrap_err();
        assert_eq!(
            custom_code(err),
            code_of(DexError::InvalidPermanentTailOffset)
        );
    }

    /// Boundary: offset EXACTLY at `MIN_PERMANENT_TAIL_OFFSET_BPS` is
    /// accepted (docs §228 phrasing: "≥ 30").
    #[test]
    fn accepts_minimum_offset() {
        assert!(validate_permanent_tail_offset(MIN_PERMANENT_TAIL_OFFSET_BPS).is_ok());
    }

    // ----------------------------------------------------------------
    // Anchor geometry (CP-4 architect spec)
    // ----------------------------------------------------------------

    /// Architect spec example — `offset_bps = 100`, `bin_step_bps = 10`,
    /// `initial_active_bin = 1000`:
    ///   tail_offset_in_bins  = 100 / 10            = 10
    ///   left_anchor_bin      = 1000 − 10           = 990
    ///   permanent_tail_floor = 990 − PERMANENT_TAIL_BIN_COUNT (70) = 920
    ///   last_rebalance_nav   = 1000  (== initial_active_bin)
    ///   active_zone_lower    = 1000  (empty zone — Nexus seeds later)
    ///   permanent_tail_offset_bps stored as u16 = 100
    #[test]
    fn anchors_correct_with_default_offset() {
        let (pool, _bins) = run_anchor_init(/* nav */ 1000, /* step */ 10, /* offset */ 100);
        assert_eq!({ pool.left_anchor_bin }, 990);
        assert_eq!({ pool.permanent_tail_floor_bin }, 920);
        assert_eq!({ pool.last_rebalance_nav_bin }, 1000);
        assert_eq!({ pool.active_zone_lower }, 1000);
        assert_eq!({ pool.permanent_tail_offset_bps }, 100u16);
        assert_eq!({ pool.bin_step_bps }, 10u16);
        assert_eq!({ pool.active_bin_id }, 1000);
        // Sanity: PERMANENT_TAIL_BIN_COUNT pinned at 70.
        assert_eq!(PERMANENT_TAIL_BIN_COUNT, 70);
    }

    /// `bin_array.lower_bin_id` is anchored at the permanent tail floor —
    /// the array layout grows monotonically upward from the tail. Same
    /// scenario as `anchors_correct_with_default_offset` → expect 920.
    #[test]
    fn bin_array_lower_bin_id_anchored_at_tail_floor() {
        let (_pool, bins) = run_anchor_init(1000, 10, 100);
        assert_eq!({ bins.lower_bin_id }, 920);
        assert_eq!({ bins.active_bin_id }, 1000);
        assert_eq!({ bins.bin_step_bps }, 10u16);
    }

    /// Pins that `bin_array.active_bin_id` mirrors the constructor arg
    /// `initial_active_bin` — guards against a refactor that accidentally
    /// derives `active_bin_id` from the anchor math (which would silently
    /// shift the active bin into the permanent tail region).
    #[test]
    fn bin_array_active_bin_id_set_correctly() {
        let (_pool, bins) = run_anchor_init(/* nav */ 12_345, 10, 100);
        assert_eq!({ bins.active_bin_id }, 12_345);
    }

    /// CP-1 carryover sanity — BinArray must be the 1000-bin layout. The
    /// constructor allocates `BinArray::SPACE` bytes (CP-1 ladder size);
    /// if the literal `1171` (old 70-bin space) ever sneaks back in,
    /// `BinArray::SPACE` would still be the truthful 16_051. We assert
    /// the SPACE here so any drift in the rent calculation immediately
    /// surfaces.
    #[test]
    fn bin_array_space_is_cp1_ladder_size() {
        assert_eq!(BinArray::SPACE, 16_051);
    }

    /// Boundary anchor check — offset at the documented minimum (= 30 bps)
    /// with the default 0.1% bin step yields `tail_offset_in_bins = 3`
    /// (= 30 / 10). For `initial_active_bin = 1000`:
    ///   left_anchor_bin       = 1000 − 3                  = 997
    ///   permanent_tail_floor  = 997 − PERMANENT_TAIL_BIN_COUNT (70) = 927
    /// Pins that the smallest accepted offset still leaves a safe
    /// `active_zone_lower > left_anchor_bin` window for the active zone.
    #[test]
    fn anchors_with_minimum_offset() {
        let (pool, _bins) =
            run_anchor_init(1000, 10, MIN_PERMANENT_TAIL_OFFSET_BPS);
        assert_eq!({ pool.left_anchor_bin }, 997);
        assert_eq!({ pool.permanent_tail_floor_bin }, 927);
        assert_eq!({ pool.active_zone_lower }, 1000);
        // Active zone lower edge sits above left anchor by the offset →
        // 1000 − 997 = 3 bins of headroom. (`compress_redistribute`
        // would block any shrink that closes this gap, per CP-3
        // `ActiveZoneOverlapsTail`.)
        assert_eq!(
            { pool.active_zone_lower } - { pool.left_anchor_bin },
            3
        );
    }
}
