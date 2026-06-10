//! CP-7 — `compress_liquidity`. Pool Rebalancer recenters the active bid
//! wall on a LOWER NAV after a governance writedown
//! (`rwt_engine::adjust_capital`). Capital-neutral: no token inflow, no
//! Nexus accounts, no token vaults. RWT above the new NAV (the "frozen
//! ask wall") is preserved for NAV recovery via future yield.
//!
//! Source of truth: `docs/contracts/native-dex.mdx` §463-494 (Accordion
//! `compress_liquidity`). Math primitive: `concentrated::compress_redistribute`
//! (CP-3).
//!
//! # Account layout (5 accounts, 1 signer)
//!
//! Order matches the docs spec (§473-474). `rwt_vault` is added for
//! symmetry with `grow_liquidity` — the on-chain ownership check
//! (`rwt_vault.owner == RWT_ENGINE_PROGRAM_ID`) is the only NAV-side
//! invariant currently enforced (see `grow_liquidity` module-level note
//! on the deferred sanity check).
//!
//! ```text
//!   0. rebalancer         (signer)
//!   1. dex_config         (read)    seeds = [b"dex_config"]
//!   2. pool_state         (mut)     must be POOL_TYPE_CONCENTRATED
//!   3. bin_array          (mut)     seeds = [b"bins", pool_state]
//!   4. rwt_vault          (read)    RWT Engine NAV source (read-only)
//! ```

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::concentrated;
use crate::constants::*;
use crate::error::DexError;
use crate::events::LiquidityCompressed;
use crate::state::{BinArray, DexConfig, PoolState};
use crate::validation::pubkey_bytes;

#[derive(Accounts)]
pub struct CompressLiquidity<'info> {
    /// Pool Rebalancer wallet (Tx signer). Auth checked against
    /// `dex_config.rebalancer` in the handler.
    #[account(signer)]
    pub rebalancer: &'info AccountView,

    /// DEX config singleton. Source of `rebalancer` authority pubkey.
    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    /// Target Monotonic Ladder pool. Mutated: `last_rebalance_nav_bin`,
    /// `active_zone_lower`. Pool-type gate rejects StandardCurve.
    #[account(mut)]
    pub pool_state: &'info AccountView,

    /// Pool's BinArray. Mutated: USDC bins in OLD active zone zeroed
    /// and redistributed around `new_nav_bin`. RWT (liquidity_a) on
    /// every bin is preserved — the "frozen ask wall" is the slice
    /// `[new_nav_bin + 1 .. old_active_bin_id]` that retains its RWT.
    #[account(mut)]
    pub bin_array: &'info AccountView,

    /// RWT Engine `RwtVault` PDA. Read-only — currently used to assert
    /// the account ownership invariant via `cpi::read_rwt_vault_nav`
    /// (returns `InvalidRwtVault` if owner != RWT Engine). Held in the
    /// account list so a future tighter on-chain NAV-to-bin check can
    /// land without a layout change.
    pub rwt_vault: &'info AccountView,
}

#[inline(never)]
pub fn handler(
    ctx: Context<CompressLiquidity>,
    new_nav_bin: i32,
    active_zone_width: u16,
) -> Result<()> {
    // 1. Authority check — signer must match `dex_config.rebalancer`.
    //    Scoped block to drop the load handle before subsequent work.
    {
        let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;
        if ctx.accounts.rebalancer.address().as_ref() != config.rebalancer.as_ref() {
            return Err(ProgramError::from(DexError::InvalidRebalancer).into());
        }
    }

    // 2. Pool-type gate — master pools only. Capture anchors for the
    //    math step; scoped so the mut handle is released before
    //    re-loading at the state-update step.
    let pool_key = pubkey_bytes(ctx.accounts.pool_state);
    let (pool_left_anchor, pool_last_rebalance, pool_bin_step_bps) = {
        let pool = PoolState::load(ctx.accounts.pool_state, ctx.program_id)?;
        if pool.pool_type != POOL_TYPE_CONCENTRATED {
            return Err(ProgramError::from(DexError::InvalidPoolType).into());
        }
        if !pool.is_active {
            return Err(ProgramError::from(DexError::PoolNotActive).into());
        }
        (pool.left_anchor_bin, pool.last_rebalance_nav_bin, pool.bin_step_bps)
    };

    // 3. NAV read — verifies `rwt_vault.owner == RWT_ENGINE_PROGRAM_ID`,
    //    discriminator match, and proves the account is a real RwtVault
    //    buffer. CP-12.5: the NAV value is now consumed by the NAV-bin
    //    sanity gate below.
    let nav = crate::cpi::read_rwt_vault_nav(ctx.accounts.rwt_vault)?;

    // 3b. NAV-bin sanity gate (CP-12.5). Symmetric with `grow_liquidity` —
    //     `new_nav_bin` must round-trip to `nav` within `± 2 × bin_step_bps`,
    //     removing the trust dependency on the Rebalancer key for
    //     ladder-geometry correctness.
    if !concentrated::nav_bin_within_tolerance(new_nav_bin, nav, pool_bin_step_bps)? {
        return Err(ProgramError::from(DexError::NavBinMismatch).into());
    }

    // 4. Math — `compress_redistribute` enforces direction
    //    (`NotCompressionDirection` if `new_nav_bin >= last_rebalance_nav_bin`),
    //    tail-overlap (`ActiveZoneOverlapsTail`), and array-extent guards.
    //    Returns the new active-zone lower bound. Capital-neutral —
    //    sum(liquidity_b) across the OLD active zone equals
    //    sum(liquidity_b) across the NEW active zone (modulo rounding
    //    dust folded onto the peak bin).
    let new_active_zone_lower = {
        // `&mut *bin_array`: DerefMut through the guard yields the
        // `&mut BinArray` the helper expects.
        let mut bin_array = BinArray::load_mut(ctx.accounts.bin_array, ctx.program_id)?;
        concentrated::compress_redistribute(
            &mut *bin_array,
            pool_left_anchor,
            pool_last_rebalance,
            new_nav_bin,
            active_zone_width,
        )?
    };

    // 5. State delta — Monotonic Ladder anchors. `active_bin_id` is NOT
    //    moved by `compress_liquidity` (only swaps move it); `reserve_b`
    //    is unchanged (capital-neutral). Compress is a pure structural
    //    redistribute on existing pool capital.
    {
        // `mut` binding: anchor writes go through the guard's DerefMut. No CPI.
        let mut pool = PoolState::load_mut(ctx.accounts.pool_state, ctx.program_id)?;
        pool.last_rebalance_nav_bin = new_nav_bin;
        pool.active_zone_lower = new_active_zone_lower;
    }

    // 6. Event for off-chain observability.
    let clock = Clock::get()?;
    emit!(LiquidityCompressed {
        pool: pool_key,
        new_nav_bin,
        new_active_zone_lower,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! CP-7 pure-Rust pinning tests for `compress_liquidity`. Same pattern
    //! as `grow_liquidity::tests` — fabricate `BinArray`, drive
    //! `concentrated::compress_redistribute` directly (the helper the
    //! production handler calls), and assert state transitions.

    use super::*;
    use crate::concentrated::compress_redistribute;
    use crate::state::Bin;

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

    /// Pool-type gate twin (shared semantics with `grow_liquidity`).
    fn enforce_concentrated(pool_type: u8) -> core::result::Result<(), ProgramError> {
        if pool_type != POOL_TYPE_CONCENTRATED {
            return Err(ProgramError::from(DexError::InvalidPoolType));
        }
        Ok(())
    }

    /// Rebalancer auth twin.
    fn check_rebalancer(
        config_rebalancer: &[u8; 32],
        signer_bytes: &[u8; 32],
    ) -> core::result::Result<(), ProgramError> {
        if signer_bytes != config_rebalancer {
            return Err(ProgramError::from(DexError::InvalidRebalancer));
        }
        Ok(())
    }

    /// CP-12.5 — NAV-bin sanity-gate twin. Mirrors the production check in
    /// `compress_liquidity` handler step 3b.
    fn check_nav_bin(
        new_nav_bin: i32,
        nav: u64,
        bin_step_bps: u16,
    ) -> core::result::Result<(), ProgramError> {
        if !crate::concentrated::nav_bin_within_tolerance(new_nav_bin, nav, bin_step_bps)? {
            return Err(ProgramError::from(DexError::NavBinMismatch));
        }
        Ok(())
    }

    /// Pool-type gate rejects StandardCurve pools. Symmetric to
    /// `grow_liquidity::tests::rejects_standard_curve`.
    #[test]
    fn rejects_standard_curve() {
        let err = enforce_concentrated(POOL_TYPE_STANDARD).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::InvalidPoolType));
        assert!(enforce_concentrated(POOL_TYPE_CONCENTRATED).is_ok());
    }

    /// Rebalancer auth — wrong signer revert.
    #[test]
    fn rejects_wrong_rebalancer_signer() {
        let rebalancer_key = [0xAAu8; 32];
        let other = [0xBBu8; 32];
        let err = check_rebalancer(&rebalancer_key, &other).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::InvalidRebalancer));
    }

    /// CP-12.5 — NAV-bin sanity gate. Symmetric with
    /// `grow_liquidity::tests::rejects_nav_bin_mismatch`.
    #[test]
    fn rejects_nav_bin_mismatch() {
        // Far-off bin: should fail.
        let err = check_nav_bin(500, 1_000_000, 10).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NavBinMismatch));
        // Matching bin: should pass.
        assert!(check_nav_bin(0, 1_000_000, 10).is_ok());
    }

    /// Direction gate — `new_nav_bin > last_rebalance_nav_bin` reverts
    /// with `NotCompressionDirection`. Symmetric counterpart to
    /// `grow_liquidity::tests::rejects_compression_direction`.
    #[test]
    fn rejects_growth_direction() {
        let mut ba = empty_bin_array(0, 500);
        let err = compress_redistribute(
            &mut ba,
            /* left_anchor_bin */ 100,
            /* last_rebalance_nav_bin */ 500,
            /* new_nav_bin */ 600,
            ACTIVE_ZONE_WIDTH,
        )
        .unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NotCompressionDirection));
        // Equal NAV also rejected (strict `>=`).
        let err = compress_redistribute(&mut ba, 100, 500, 500, ACTIVE_ZONE_WIDTH).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NotCompressionDirection));
    }

    /// Tail-overlap revert — `new_zone_lower = new_nav_bin - 40 + 1 = 100
    /// - 39 = 61` dips below `left_anchor_bin = 80`.
    #[test]
    fn rejects_overlap_with_tail() {
        let mut ba = empty_bin_array(0, 200);
        let err = compress_redistribute(
            &mut ba,
            /* left_anchor_bin */ 80,
            /* last_rebalance_nav_bin */ 200,
            /* new_nav_bin */ 100,
            ACTIVE_ZONE_WIDTH,
        )
        .unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::ActiveZoneOverlapsTail));
    }

    /// Capital-preservation invariant — total USDC across the array
    /// before and after `compress_redistribute` agrees within the
    /// per-bin rounding-dust tolerance (`ACTIVE_ZONE_WIDTH` ulps,
    /// reconciled onto the peak bin).
    #[test]
    fn happy_path_preserves_capital() {
        let mut ba = empty_bin_array(0, 500);
        // Seed OLD active zone [461..=500] with 4_000 USDC each.
        for bin_id in 461..=500 {
            let idx = (bin_id - ba.lower_bin_id) as usize;
            ba.bins[idx].liquidity_b = 4_000;
        }
        let pre_total: u128 = {
            let mut s: u128 = 0;
            for i in 0..MAX_BINS {
                s += { ba.bins[i].liquidity_b } as u128;
            }
            s
        };

        // Compress NAV 500 → 480. NEW zone = [441..=480].
        let new_lo = compress_redistribute(
            &mut ba,
            /* left_anchor_bin */ 100,
            /* last_rebalance_nav_bin */ 500,
            /* new_nav_bin */ 480,
            ACTIVE_ZONE_WIDTH,
        )
        .expect("compress_redistribute");
        assert_eq!(new_lo, 441);

        let post_total: u128 = {
            let mut s: u128 = 0;
            for i in 0..MAX_BINS {
                s += { ba.bins[i].liquidity_b } as u128;
            }
            s
        };
        let diff = pre_total as i128 - post_total as i128;
        assert!(
            diff.unsigned_abs() <= ACTIVE_ZONE_WIDTH as u128,
            "capital drift > 40 ulps: pre={pre_total}, post={post_total}"
        );
    }

    /// Frozen-ask-wall invariant — RWT (`liquidity_a`) in bins between
    /// the OLD active bin and `new_nav_bin` survives the compression
    /// untouched. Off-chain dashboards interpret this slice as the
    /// "frozen ask wall" awaiting NAV recovery.
    #[test]
    fn happy_path_freezes_ask_wall() {
        let mut ba = empty_bin_array(0, 500);
        // Seed OLD active zone with USDC.
        for bin_id in 461..=500 {
            let idx = (bin_id - ba.lower_bin_id) as usize;
            ba.bins[idx].liquidity_b = 4_000;
        }
        // Plant RWT in bins above new NAV (480) but below OLD active
        // bin (500). These bins enter the "frozen ask wall" after
        // compression.
        let i485 = (485 - ba.lower_bin_id) as usize;
        let i495 = (495 - ba.lower_bin_id) as usize;
        ba.bins[i485].liquidity_a = 3_333;
        ba.bins[i495].liquidity_a = 5_555;

        compress_redistribute(&mut ba, 100, 500, 480, ACTIVE_ZONE_WIDTH)
            .expect("compress_redistribute");

        // Frozen-ask RWT preserved. `Bin` is `repr(C, packed)` — reads
        // must go through copies (the braces).
        assert_eq!({ ba.bins[i485].liquidity_a }, 3_333u64);
        assert_eq!({ ba.bins[i495].liquidity_a }, 5_555u64);
    }
}
