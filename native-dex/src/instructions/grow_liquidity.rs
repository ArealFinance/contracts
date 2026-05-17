//! CP-7 — `grow_liquidity`. Pool Rebalancer extends the active bid wall
//! rightward when NAV rises past the 1% deviation threshold. Pulls fresh
//! USDC from the Nexus accumulator and redistributes existing active-zone
//! USDC so the geometric density peak sits at the new NAV. Permanent tail
//! and organic ask are NEVER touched.
//!
//! Source of truth: `docs/contracts/native-dex.mdx` §382-461 (Accordion
//! `grow_liquidity`), `docs/changelog/2026-04-17-monotonic-ladder.mdx`
//! §62-66. Math primitive: `concentrated::grow_redistribute` (CP-3).
//!
//! # Account layout (10 accounts, 1 signer)
//!
//! Order matches the docs spec (§392-400) with `dex_config` interleaved per
//! the canonical Layer-9 Manager-gated pattern (see `nexus_swap`). The
//! signer is the Pool Rebalancer wallet — the dispatcher verifies the
//! signer matches `dex_config.rebalancer` (CP-7 new auth surface).
//!
//! ```text
//!   0. rebalancer         (signer)
//!   1. dex_config         (read)    seeds = [b"dex_config"]
//!   2. pool_state         (mut)     must be POOL_TYPE_CONCENTRATED
//!   3. bin_array          (mut)     seeds = [b"bins", pool_state]
//!   4. liquidity_nexus    (mut)     seeds = [LIQUIDITY_NEXUS_SEED]
//!                                   PDA-signs the SPL Transfer
//!   5. nexus_usdc_ata     (mut)     Nexus-owned USDC ATA (source)
//!   6. pool_vault_b       (mut)     pool's USDC vault (destination)
//!   7. rwt_vault          (read)    RWT Engine NAV source (read-only)
//!   8. token_program      (read)    SPL Token program
//! ```
//!
//! The Nexus PDA signs the inbound SPL Transfer (`nexus_usdc_ata →
//! pool_vault_b`) via seeds `[LIQUIDITY_NEXUS_SEED, &[bump]]`, identical to
//! `nexus_swap` / `nexus_add_liquidity`.
//!
//! # NAV-bin sanity check (deferred)
//!
//! The docs spec (§383-388) computes `new_nav_bin` off-chain via
//! `new_nav_bin = floor(log(nav) / log(1 + bin_step_bps/10_000))`. A
//! straight on-chain re-derivation would need a `log` over Q64.64 against
//! the USDC-decimals NAV — fragile and CU-expensive. The math helper
//! already enforces `NotGrowthDirection` (`new_nav_bin >
//! last_rebalance_nav_bin`) and `ExceedsRightEdgeBuffer`; combined with
//! the Rebalancer signer gate, the off-chain caller is trusted for the
//! NAV-to-bin computation. The `rwt_vault` is still wired into the
//! account list so a future tighter check (e.g. `|nav -
//! price_at_bin(new_nav_bin)| < bin_step_bps`) can land without a layout
//! change.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::concentrated;
use crate::constants::*;
use crate::error::DexError;
use crate::events::LiquidityGrew;
use crate::state::{BinArray, DexConfig, LiquidityNexus, PoolState};
use crate::validation::{pubkey_bytes, read_token_account_mint, read_token_account_owner};

#[derive(Accounts)]
pub struct GrowLiquidity<'info> {
    /// Pool Rebalancer wallet (Tx signer). Auth checked against
    /// `dex_config.rebalancer` in the handler (CP-7 introduces the
    /// `InvalidRebalancer` error).
    #[account(signer)]
    pub rebalancer: &'info AccountView,

    /// DEX config singleton. Source of `rebalancer` authority pubkey.
    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    /// Target Monotonic Ladder pool. Mutated: `last_rebalance_nav_bin`,
    /// `active_zone_lower`. Pool-type gate rejects StandardCurve.
    #[account(mut)]
    pub pool_state: &'info AccountView,

    /// Pool's BinArray. Mutated: active-zone bins' `liquidity_b`
    /// rebuilt via `grow_redistribute`. Permanent-tail bins and
    /// organic-ask bins are NOT touched.
    #[account(mut)]
    pub bin_array: &'info AccountView,

    /// Singleton Nexus PDA. PDA-signs the inbound SPL Transfer that
    /// drains the accumulator into the pool's USDC vault. Mutable
    /// because the PDA signs from this slot; the Nexus account data
    /// itself is not modified (counters belong to
    /// `nexus_deposit`/`nexus_withdraw_profits`).
    #[account(mut, seeds = [LIQUIDITY_NEXUS_SEED], bump)]
    pub liquidity_nexus: &'info AccountView,

    /// Nexus-owned USDC ATA. PDA-signed SPL Transfer source. Validated
    /// for SPL-owner == `liquidity_nexus` and mint == `USDC_MINT` in
    /// the handler.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_usdc_ata: &'info AccountView,

    /// Pool's USDC vault (token-B side). Validated against
    /// `pool.vault_b` in the handler.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub pool_vault_b: &'info AccountView,

    /// RWT Engine `RwtVault` PDA. Read-only — currently used to assert
    /// the account ownership invariant via `cpi::read_rwt_vault_nav`
    /// (returns `InvalidRwtVault` if owner != RWT Engine). NAV value
    /// itself is read but no on-chain mismatch check is enforced (see
    /// module-level note on the deferred sanity check).
    pub rwt_vault: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

#[inline(never)]
pub fn handler(
    ctx: Context<GrowLiquidity>,
    new_nav_bin: i32,
    active_zone_width: u16,
) -> Result<()> {
    // 1. Authority check — signer must match `dex_config.rebalancer`.
    //    Mirrors the canonical Manager-gate pattern from `nexus_swap`,
    //    only the auth field lives on `DexConfig`, not `LiquidityNexus`.
    //    Scoped block so the load handle is dropped before the math step.
    {
        let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;
        if ctx.accounts.rebalancer.address().as_ref() != config.rebalancer.as_ref() {
            return Err(ProgramError::from(DexError::InvalidRebalancer).into());
        }
    }

    // 2. Pool-type gate — master (Monotonic Ladder) pools only. Mirrors
    //    the symmetric Nexus-gate inside `nexus_add_liquidity`. Scoped
    //    so the mut handle is dropped before bin-array mutation;
    //    re-loaded after the math step to apply the state delta.
    let pool_key = pubkey_bytes(ctx.accounts.pool_state);
    let (
        pool_left_anchor,
        pool_tail_floor,
        pool_last_rebalance,
        pool_vault_b_addr,
    ) = {
        let pool = PoolState::load(ctx.accounts.pool_state, ctx.program_id)?;
        if pool.pool_type != POOL_TYPE_CONCENTRATED {
            return Err(ProgramError::from(DexError::InvalidPoolType).into());
        }
        if !pool.is_active {
            return Err(ProgramError::from(DexError::PoolNotActive).into());
        }
        (
            pool.left_anchor_bin,
            pool.permanent_tail_floor_bin,
            pool.last_rebalance_nav_bin,
            pool.vault_b,
        )
    };

    // 3. Vault address pin — defence-in-depth against the caller passing
    //    a foreign USDC vault whose mint happens to match. `validate_vault`
    //    style guard, inline to keep import surface small.
    if ctx.accounts.pool_vault_b.address().as_ref() != pool_vault_b_addr.as_ref() {
        return Err(ProgramError::from(DexError::InvalidVault).into());
    }

    // 4. NAV read — verifies `rwt_vault.owner == RWT_ENGINE_PROGRAM_ID`
    //    and proves the account is a real RwtVault buffer. The NAV value
    //    itself is currently consumed only for the on-chain ownership
    //    invariant; the off-chain Rebalancer is trusted for the NAV→bin
    //    computation (see module-level note on deferred sanity check).
    let _nav = crate::cpi::read_rwt_vault_nav(ctx.accounts.rwt_vault)?;

    // 5. Nexus USDC source ATA invariants — SPL-owner must equal the
    //    Nexus PDA (else a foreign USDC ATA could be drained), mint must
    //    equal USDC_MINT (else cross-token transfer would revert mid-CPI
    //    with a worse error). USDC_MINT is `[0u8; 32]` on test-validator,
    //    so the mint check is structural only on mainnet (per the
    //    constants.rs MAINNET-REPLACE note); on devnet the SPL Transfer
    //    would still reject a mismatched destination.
    let nexus_addr = pubkey_bytes(ctx.accounts.liquidity_nexus);
    let src_owner = read_token_account_owner(ctx.accounts.nexus_usdc_ata)?;
    if src_owner != nexus_addr {
        return Err(ProgramError::from(DexError::InvalidTokenAccount).into());
    }
    let src_mint = read_token_account_mint(ctx.accounts.nexus_usdc_ata)?;
    if src_mint != USDC_MINT {
        return Err(ProgramError::from(DexError::InvalidNexusToken).into());
    }

    // 6. Read Nexus accumulator balance + capture `is_active` + bump.
    //    Scoped so the Nexus mut handle is dropped before the CPI re-uses
    //    the AccountView as the `authority` slot.
    let (nexus_available, nexus_bump) = {
        let nexus = LiquidityNexus::load(ctx.accounts.liquidity_nexus, ctx.program_id)?;
        if !nexus.is_active {
            return Err(ProgramError::from(DexError::NexusNotActive).into());
        }
        let bal = read_spl_token_amount(ctx.accounts.nexus_usdc_ata)?;
        (bal, nexus.bump)
    };
    if nexus_available == 0 {
        return Err(ProgramError::from(DexError::NexusAccumulatorEmpty).into());
    }

    // 7. Math — `grow_redistribute` enforces direction (`NotGrowthDirection`
    //    if `new_nav_bin <= last_rebalance_nav_bin`), tail-overlap
    //    (`ActiveZoneOverlapsTail`), and right-edge buffer
    //    (`ExceedsRightEdgeBuffer`). Returns the new active-zone lower bound.
    let new_active_zone_lower = {
        let bin_array = BinArray::load_mut(ctx.accounts.bin_array, ctx.program_id)?;
        concentrated::grow_redistribute(
            bin_array,
            pool_left_anchor,
            pool_tail_floor,
            pool_last_rebalance,
            new_nav_bin,
            active_zone_width,
            nexus_available,
        )?
    };

    // 8. PDA-signed SPL Transfer: `nexus_usdc_ata → pool_vault_b`.
    //    Seeds mirror `nexus_swap` exactly (`[LIQUIDITY_NEXUS_SEED, &[bump]]`).
    let bump_arr = [nexus_bump];
    let signer_seeds: [Seed; 2] = [
        Seed::from(LIQUIDITY_NEXUS_SEED),
        Seed::from(bump_arr.as_ref()),
    ];
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.nexus_usdc_ata,
        to: ctx.accounts.pool_vault_b,
        authority: ctx.accounts.liquidity_nexus,
        amount: nexus_available,
    }
    .invoke_signed(&[Signer::from(&signer_seeds)])?;

    // 9. State delta — update Monotonic Ladder anchors. `active_bin_id` is
    //    NOT changed by `grow_liquidity` (only swaps move the active bin
    //    per docs §449). `pool.reserve_b` is also bumped so the on-chain
    //    `reserve_<side>` accounting tracks the inbound USDC.
    {
        let pool = PoolState::load_mut(ctx.accounts.pool_state, ctx.program_id)?;
        pool.last_rebalance_nav_bin = new_nav_bin;
        pool.active_zone_lower = new_active_zone_lower;
        pool.reserve_b = pool
            .reserve_b
            .checked_add(nexus_available)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }

    // 10. Event for off-chain observability.
    let clock = Clock::get()?;
    emit!(LiquidityGrew {
        pool: pool_key,
        new_nav_bin,
        fresh_usdc: nexus_available,
        new_active_zone_lower,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

/// Read SPL Token Account `amount` field (bytes 64..72 LE) via zero-copy.
///
/// Mirrors `nexus_withdraw_profits::read_token_account_amount` and
/// `compound_yield::read_token_account_amount`. Local copy avoids a
/// `pub`-export across instruction modules until N-6 lands a shared
/// validation helper.
fn read_spl_token_amount(account: &AccountView) -> Result<u64> {
    // SAFETY: standard Pinocchio zero-copy pattern. Length check below
    // ensures no out-of-bounds indexing.
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 72 {
        return Err(ProgramError::InvalidAccountData.into());
    }
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    //! CP-7 pure-Rust pinning tests for `grow_liquidity`. Handler-level
    //! negative ACs (wrong signer, foreign vault, etc.) require the BPF
    //! runtime; here we pin the math contract surface and the
    //! decision-table inputs so a refactor cannot silently regress the
    //! Rebalancer's growth path.
    //!
    //! Each test fabricates a zero-init `BinArray` + a known set of pool
    //! anchors, drives `concentrated::grow_redistribute` (the same helper
    //! the production handler calls), and asserts state transitions.

    use super::*;
    use crate::concentrated::grow_redistribute;
    use crate::state::Bin;

    // ---- Test helpers (mirror `concentrated::tests`) ------------------

    fn empty_bin_array(lower_bin_id: i32, active_bin_id: i32) -> BinArray {
        BinArray {
            pool: [0u8; 32],
            bins: [Bin { liquidity_a: 0, liquidity_b: 0 }; MAX_BINS],
            lower_bin_id,
            bin_step_bps: 10,
            active_bin_id,
            bump: 0,
        }
    }

    fn custom_code(err: ProgramError) -> u32 {
        match err {
            ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    fn code_of(err: DexError) -> u32 {
        custom_code(ProgramError::from(err))
    }

    /// CP-7 — pool-type gate twin. Mirrors the production guard at handler
    /// step 2 against StandardCurve pools.
    fn enforce_concentrated(pool_type: u8) -> core::result::Result<(), ProgramError> {
        if pool_type != POOL_TYPE_CONCENTRATED {
            return Err(ProgramError::from(DexError::InvalidPoolType));
        }
        Ok(())
    }

    /// CP-7 — Rebalancer auth twin. Mirrors the production check in
    /// handler step 1.
    fn check_rebalancer(
        config_rebalancer: &[u8; 32],
        signer_bytes: &[u8; 32],
    ) -> core::result::Result<(), ProgramError> {
        if signer_bytes != config_rebalancer {
            return Err(ProgramError::from(DexError::InvalidRebalancer));
        }
        Ok(())
    }

    /// CP-7 — Nexus accumulator empty-guard twin.
    fn check_nexus_balance(balance: u64) -> core::result::Result<(), ProgramError> {
        if balance == 0 {
            return Err(ProgramError::from(DexError::NexusAccumulatorEmpty));
        }
        Ok(())
    }

    // ---- Handler-level pin tests --------------------------------------

    /// Pool-type gate rejects StandardCurve pools. The Rebalancer's
    /// growth path is reserved for master pools; StandardCurve liquidity
    /// is user-LP territory.
    #[test]
    fn rejects_standard_curve() {
        let err = enforce_concentrated(POOL_TYPE_STANDARD).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::InvalidPoolType));
        // Concentrated accepted.
        assert!(enforce_concentrated(POOL_TYPE_CONCENTRATED).is_ok());
    }

    /// Rebalancer auth — wrong signer revert maps to `InvalidRebalancer`.
    #[test]
    fn rejects_wrong_rebalancer_signer() {
        let rebalancer_key = [0xAAu8; 32];
        let other = [0xBBu8; 32];
        let err = check_rebalancer(&rebalancer_key, &other).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::InvalidRebalancer));
        // Matching signer accepted.
        assert!(check_rebalancer(&rebalancer_key, &rebalancer_key).is_ok());
    }

    /// Direction gate — `new_nav_bin < last_rebalance_nav_bin` reverts
    /// with `NotGrowthDirection`. Math-helper-level invariant, but pinned
    /// at the handler test surface to document the contract.
    #[test]
    fn rejects_compression_direction() {
        let mut ba = empty_bin_array(0, 500);
        // Last rebalance = 500, new_nav_bin = 400 → compression, rejected.
        let err = grow_redistribute(
            &mut ba,
            /* left_anchor_bin */ 100,
            /* permanent_tail_floor_bin */ 100,
            /* last_rebalance_nav_bin */ 500,
            /* new_nav_bin */ 400,
            ACTIVE_ZONE_WIDTH,
            10_000,
        )
        .unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NotGrowthDirection));
    }

    /// Zero movement (`new_nav_bin == last_rebalance_nav_bin`) also
    /// reverts — `grow_redistribute` enforces strict `>`.
    #[test]
    fn rejects_no_movement() {
        let mut ba = empty_bin_array(0, 500);
        let err = grow_redistribute(
            &mut ba,
            100,
            100,
            500,
            500, // equal → reject
            ACTIVE_ZONE_WIDTH,
            10_000,
        )
        .unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NotGrowthDirection));
    }

    /// Nexus accumulator empty → `NexusAccumulatorEmpty`. Production
    /// check runs BEFORE the math (step 6 of the handler) so an empty
    /// accumulator never triggers a redistribute on zero capital.
    #[test]
    fn rejects_empty_nexus() {
        let err = check_nexus_balance(0).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NexusAccumulatorEmpty));
        // Any non-zero balance accepted.
        assert!(check_nexus_balance(1).is_ok());
    }

    /// Tail-overlap revert propagates from `grow_redistribute`.
    /// `new_zone_lower = new_nav_bin - ACTIVE_ZONE_WIDTH + 1 = 100 - 39 = 61`
    /// dips below `left_anchor_bin = 80`.
    #[test]
    fn rejects_overlap_with_tail() {
        let mut ba = empty_bin_array(0, 50);
        let err = grow_redistribute(
            &mut ba,
            /* left_anchor_bin */ 80,
            /* permanent_tail_floor_bin */ 10,
            /* last_rebalance_nav_bin */ 50,
            /* new_nav_bin */ 100,
            ACTIVE_ZONE_WIDTH,
            10_000,
        )
        .unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::ActiveZoneOverlapsTail));
    }

    /// Right-edge buffer revert — `new_nav_bin = 995` with `lower = 0`
    /// and `MAX_BINS = 1000` leaves only 4 trailing bins of headroom
    /// (`upper - new_nav_bin = 4`, below `RIGHT_EDGE_BUFFER_BINS = 10`).
    #[test]
    fn rejects_right_edge_buffer() {
        let mut ba = empty_bin_array(0, 800);
        let err = grow_redistribute(
            &mut ba,
            50,
            50,
            800,
            995,
            ACTIVE_ZONE_WIDTH,
            10_000,
        )
        .unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::ExceedsRightEdgeBuffer));
    }

    /// Happy path — verifies the state-transition trio the handler
    /// commits after `grow_redistribute`:
    ///   1. New active-zone lower bound returned from math is correct.
    ///   2. Active-zone bins carry fresh USDC (plus pre-existing).
    ///   3. Permanent tail and organic ask untouched.
    #[test]
    fn happy_path_redistributes_correctly() {
        let mut ba = empty_bin_array(0, 500);
        // Seed old active zone [461..=500] with 10_000 USDC each.
        for bin_id in 461..=500 {
            let idx = (bin_id - ba.lower_bin_id) as usize;
            ba.bins[idx].liquidity_b = 10_000;
        }
        // Seed permanent tail [30..100] with 1_234 each — must stay
        // byte-identical post-grow.
        for bin_id in 30..100 {
            let idx = (bin_id - ba.lower_bin_id) as usize;
            ba.bins[idx].liquidity_b = 1_234;
        }
        // Seed organic ask at bin 700 with RWT — must survive.
        let i700 = (700 - ba.lower_bin_id) as usize;
        ba.bins[i700].liquidity_a = 42_000;

        let fresh = 50_000u64;
        let new_active_lo = grow_redistribute(
            &mut ba,
            /* left_anchor_bin */ 100,
            /* permanent_tail_floor_bin */ 30,
            /* last_rebalance_nav_bin */ 500,
            /* new_nav_bin */ 600,
            ACTIVE_ZONE_WIDTH,
            fresh,
        )
        .expect("grow_redistribute");

        // (1) Lower bound = new_nav_bin - ACTIVE_ZONE_WIDTH + 1 = 561.
        assert_eq!(new_active_lo, 561);

        // (2) New active zone holds redistributed USDC (sum within 40-ulp).
        let mut new_zone_total: u128 = 0;
        for bin_id in new_active_lo..=600 {
            let idx = (bin_id - ba.lower_bin_id) as usize;
            new_zone_total += { ba.bins[idx].liquidity_b } as u128;
        }
        let pre_zone = 40u128 * 10_000;
        let expected = pre_zone + fresh as u128;
        let diff = expected as i128 - new_zone_total as i128;
        assert!(
            diff.unsigned_abs() <= ACTIVE_ZONE_WIDTH as u128,
            "redistribution drift > 40 ulps: expected {expected}, got {new_zone_total}"
        );

        // (3a) Permanent tail unchanged byte-identical.
        for bin_id in 30..100 {
            let idx = (bin_id - ba.lower_bin_id) as usize;
            assert_eq!({ ba.bins[idx].liquidity_b }, 1_234u64);
        }
        // (3b) Organic ask RWT survives.
        assert_eq!({ ba.bins[i700].liquidity_a }, 42_000u64);
    }
}
