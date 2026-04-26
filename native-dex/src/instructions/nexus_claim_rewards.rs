//! Layer 9 §4.7 — `nexus_claim_rewards`. Authority-gated realisation of LP
//! fees that have accrued on the Nexus's `LpPosition` for a given pool.
//! Composes `claim_lp_fees_internal` (D28) — reuses the canonical LP-fee
//! claim logic (cumulative-vs-snapshot delta, Q64.64 → u64 truncation,
//! pool-PDA-signed Transfer) with the Nexus PDA filling the `authority`
//! slot. Claimed fees flow back into the Nexus-owned token A and token B
//! ATAs, where they accrue as profit above `total_deposited_<token>` and
//! become eligible for sweep via `nexus_withdraw_profits`.
//!
//! # Auth gate
//!
//! Authority-gated, **not** Manager-gated (SD-1). The `has_one = authority`
//! constraint on `dex_config` is the access-control rule; `assert_manager`
//! is intentionally NOT invoked here. This matches the "rewards routing
//! is Treasury policy, not Manager policy" decision in Layer 9 §4.7 —
//! Manager owns swap/add/remove operations; Authority owns Treasury sweeps.
//!
//! # D28 inheritance
//!
//! `claim_lp_fees_internal` is the single source of truth for the claim
//! math (per D23 internal-call pattern). The Nexus's `LpPosition` is just
//! another LP-position with `owner == liquidity_nexus.key()`; all
//! claim-time invariants (cumulative-vs-snapshot delta, pool/owner cross-
//! checks, snapshot-advance-before-CPI per CEI) inherit automatically.
//! This handler is a thin wrapper: build Nexus signer-seeds, project the
//! account set into `ClaimLpFeesAccountsView`, invoke the helper.
//!
//! # No additional event
//!
//! `claim_lp_fees_internal` emits `LpFeesClaimed` (recipient, pool,
//! claimable_a, claimable_b, timestamp) — sufficient for indexers to
//! reconstruct the Nexus reward sweep. We intentionally do NOT emit a
//! second `NexusRewardsClaimed` event here to avoid double-emission for
//! the same effect; downstream consumers filter `LpFeesClaimed` by
//! `recipient == nexus.address()` to identify Nexus claims. (The
//! `NexusRewardsClaimed` event remains defined in `events.rs` for
//! potential future use — e.g. a single-side claim ix variant — without
//! touching the wire interface.)

use arlex_lang::prelude::*;

use crate::constants::*;
use crate::error::DexError;
use crate::state::{DexConfig, LiquidityNexus};
use crate::instructions::claim_lp_fees::{claim_lp_fees_internal, ClaimLpFeesAccountsView};

#[derive(Accounts)]
pub struct NexusClaimRewards<'info> {
    /// DEX authority. `has_one = authority` on `dex_config` is the
    /// access-control gate (Layer 9 §4.7).
    #[account(signer)]
    pub authority: &'info AccountView,

    /// DEX config singleton — `has_one = authority` is the Layer 9 §4.7
    /// access-control rule. Read-only here (no mutation).
    #[account(
        has_one = authority, account_type = "DexConfig",
        seeds = [b"dex_config"], bump
    )]
    pub dex_config: &'info AccountView,

    /// Nexus singleton PDA. Mutable because the inbound vault → Nexus ATA
    /// transfers (inside the inner helper) sign with the pool PDA seeds,
    /// but the LpPosition owned by this PDA is mutated, so we mark `mut`
    /// for the framework permission grant. The data of the Nexus account
    /// itself is not modified.
    #[account(mut, seeds = [LIQUIDITY_NEXUS_SEED], bump)]
    pub liquidity_nexus: &'info AccountView,

    /// Target pool. `claim_lp_fees_internal` reads the cumulative-fee
    /// accumulator + token mints + bump for the PDA-signed Transfer
    /// derivations. Marked `mut` to match the inner helper's account
    /// permissions (the helper does not currently write but reserves the
    /// slot for future variants — see `claim_lp_fees.rs`).
    #[account(mut)]
    pub pool_state: &'info AccountView,

    /// Nexus's `LpPosition` for this pool (PDA seed
    /// `["lp", pool_state, liquidity_nexus]`). Mutated by the inner helper
    /// to advance `fees_claimed_per_share_<side>` to the pool's current
    /// cumulative.
    #[account(mut)]
    pub lp_position: &'info AccountView,

    /// Pool vault A. PDA-signed Transfer source on the A-side claim.
    /// Validated against `pool.vault_a` inside the inner helper.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub pool_vault_a: &'info AccountView,

    /// Pool vault B. Symmetric to `pool_vault_a`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub pool_vault_b: &'info AccountView,

    /// Nexus-owned ATA for token A — receives the side-A claimable. Routes
    /// claimed fees back into the Nexus's pool of liquidity, where they
    /// accumulate above `total_deposited_<token>` for later sweep via
    /// `nexus_withdraw_profits`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_a_ata: &'info AccountView,

    /// Nexus-owned ATA for token B. Symmetric to `nexus_token_a_ata`.
    //
    // Note: the SPL Token program account is required in the on-chain TX
    // accounts list (Solana runtime must load it for the inner pool-PDA-
    // signed Transfers inside `claim_lp_fees_internal`) but intentionally
    // NOT a named field of this Accounts struct — same BPF stack-frame
    // budget rationale as `nexus_deposit`. The inner helper passes a
    // `token_program` slot through `ClaimLpFeesAccountsView`, but pinocchio
    // does not actually consume that slot at runtime; we wire it from the
    // `liquidity_nexus` account (an arbitrary live AccountView) to satisfy
    // the field initialization without inflating this dispatcher's frame.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_b_ata: &'info AccountView,
}

impl<'info> NexusClaimRewards<'info> {
    /// Project the Nexus account set into the shape expected by
    /// `claim_lp_fees_internal`. Critical wiring:
    ///   - `authority = liquidity_nexus` — Nexus PDA owns the LpPosition,
    ///     and the inner helper's pool/owner cross-check uses this address.
    ///   - `authority_token_a/b = nexus_token_a/b_ata` — claimed fees route
    ///     back to Nexus-owned ATAs (auto-recycle into protocol liquidity).
    pub(crate) fn view(&self) -> ClaimLpFeesAccountsView<'info> {
        // `claim_lp_fees_internal` reserves a `token_program` slot in
        // `ClaimLpFeesAccountsView` for forward compatibility but never
        // reads it (pinocchio_token hardcodes the SPL program ID). We
        // wire `liquidity_nexus` into that slot — an arbitrary live
        // `&AccountView` reference satisfies the field initialiser without
        // requiring a separate token_program slot in this Accounts struct
        // (saves dispatcher stack budget; see struct-level comment).
        ClaimLpFeesAccountsView {
            authority: self.liquidity_nexus,
            pool_state: self.pool_state,
            lp_position: self.lp_position,
            pool_vault_a: self.pool_vault_a,
            pool_vault_b: self.pool_vault_b,
            authority_token_a: self.nexus_token_a_ata,
            authority_token_b: self.nexus_token_b_ata,
            token_program: self.liquidity_nexus,
        }
    }
}

#[inline(never)]
pub fn handler(ctx: Context<NexusClaimRewards>) -> Result<()> {
    // `#[inline(never)]` keeps the handler's locals (PDA signer-seed array,
    // `ClaimLpFeesAccountsView` projection) out of the BPF entrypoint
    // dispatcher's stack frame. The 4 new Layer 9 ix together push the
    // dispatcher near the 4096-byte SBF stack budget; keeping each
    // handler non-inlined shifts the frame charge to a separate, isolated
    // function.

    // 1. Active-gate. The `is_active` flag is the Nexus-level kill-switch
    //    independent of the Manager kill-switch (D22) — Authority-gated
    //    operations should still respect Nexus-level deactivation.
    //    Scoped block so the load handle is dropped before the inner CPI
    //    that may borrow `liquidity_nexus` for its signer-seed derivations.
    let nexus_bump: u8 = {
        let nexus = LiquidityNexus::load(
            ctx.accounts.liquidity_nexus,
            ctx.program_id,
        )?;
        if !nexus.is_active {
            return Err(ProgramError::from(DexError::NexusNotActive).into());
        }
        nexus.bump
    };

    // 2. Build Nexus PDA signer seeds. `claim_lp_fees_internal` threads
    //    `_authority_signer_seeds` through for interface uniformity but
    //    currently does not invoke an authority-CPI leg (vault transfers
    //    sign with the pool PDA per D28). We pass the seeds anyway for
    //    forward-compatibility with future variants that may add an
    //    authority-side CPI (e.g. close-position-on-zero-shares).
    let bump_arr = [nexus_bump];
    let signer_seeds: [Seed; 2] = [
        Seed::from(LIQUIDITY_NEXUS_SEED),
        Seed::from(bump_arr.as_ref()),
    ];
    let signers = [Signer::from(&signer_seeds)];

    // 3. Reuse the canonical LP-fee claim path. D28 invariants
    //    (cumulative-vs-snapshot delta, Q64.64 → u64 truncation,
    //    pool/owner cross-check, snapshot-advance-before-CPI) inherit
    //    automatically — the Nexus's `LpPosition` is just another
    //    LP-position with `owner == liquidity_nexus.key()`.
    claim_lp_fees_internal(
        &ctx.accounts.view(),
        ctx.program_id,
        Some(&signers),
    )
}

#[cfg(test)]
mod tests {
    //! Layer 9 §4.7 — pure-Rust pinning tests for the D28-inheritance
    //! contract surface. Handler-level negative ACs (non-Authority signer,
    //! Nexus inactive, LpPosition owner mismatch) require the BPF runtime;
    //! here we mirror the inner helper's math against synthetic state to
    //! pin the inherited semantics so a refactor cannot silently regress
    //! the Nexus's claim correctness.
    use crate::instructions::claim_lp_fees::compute_claimable;
    use crate::state::{LpPosition, PoolState};

    fn make_pool(cumulative_a: u128, cumulative_b: u128) -> PoolState {
        // SAFETY: zero-init valid for `PoolState`; see `state::tests`.
        let buf = [0u8; core::mem::size_of::<PoolState>()];
        let mut pool: PoolState =
            unsafe { core::ptr::read(buf.as_ptr() as *const PoolState) };
        pool.cumulative_fees_per_share_a = cumulative_a;
        pool.cumulative_fees_per_share_b = cumulative_b;
        pool
    }

    fn make_position(
        shares: u128,
        fees_claimed_a: u128,
        fees_claimed_b: u128,
    ) -> LpPosition {
        // SAFETY: zero-init valid for `LpPosition`; see `state::tests`.
        let buf = [0u8; core::mem::size_of::<LpPosition>()];
        let mut lp: LpPosition =
            unsafe { core::ptr::read(buf.as_ptr() as *const LpPosition) };
        lp.shares = shares;
        lp.fees_claimed_per_share_a = fees_claimed_a;
        lp.fees_claimed_per_share_b = fees_claimed_b;
        lp
    }

    /// **D28 inheritance** — when the pool's cumulative fee per share has
    /// advanced beyond the Nexus's `LpPosition.fees_claimed_per_share`,
    /// `claim_lp_fees_internal` computes a non-zero claimable. The Nexus's
    /// LpPosition is a vanilla LpPosition record (only the `owner` field
    /// distinguishes it), so the inner helper's math applies verbatim.
    #[test]
    fn nexus_claim_rewards_inherits_d28_claim_logic() {
        // Pool advanced cumulative_a from prior swaps; cumulative_b unchanged.
        // Per-share value: 4 << 64 means each share earned 4 tokens of fee.
        let pool = make_pool(/* a */ 4u128 << 64, /* b */ 0);
        // Nexus position with 2_500 shares, zero snapshot ⇒ entire delta owed.
        let nexus_lp = make_position(/* shares */ 2_500, 0, 0);

        // Mirror the inner helper's delta + payout math.
        let delta_a = pool
            .cumulative_fees_per_share_a
            .checked_sub(nexus_lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b = pool
            .cumulative_fees_per_share_b
            .checked_sub(nexus_lp.fees_claimed_per_share_b)
            .unwrap();
        let claim_a = compute_claimable(delta_a, nexus_lp.shares).unwrap();
        let claim_b = compute_claimable(delta_b, nexus_lp.shares).unwrap();

        // (4 << 64) * 2_500 >> 64 == 10_000.
        assert_eq!(claim_a, 10_000u64);
        assert_eq!(claim_b, 0u64);
    }

    /// **Zero-pending no-op** — when the pool's cumulative-per-share equals
    /// the Nexus's snapshot (already-claimed-up-to-date), both deltas are
    /// zero and the helper performs no Transfer (skipped per side when
    /// claimable is zero, see `claim_lp_fees.rs`). The handler still
    /// returns Ok — Authority can call repeatedly without spurious reverts.
    #[test]
    fn nexus_claim_rewards_zero_pending_no_op() {
        let cumulative_a: u128 = 7u128 << 64;
        let cumulative_b: u128 = 11u128 << 64;
        let pool = make_pool(cumulative_a, cumulative_b);
        // Snapshot already matches pool cumulative → delta = 0 on both sides.
        let nexus_lp = make_position(/* shares */ 1_000, cumulative_a, cumulative_b);

        let delta_a = pool
            .cumulative_fees_per_share_a
            .checked_sub(nexus_lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b = pool
            .cumulative_fees_per_share_b
            .checked_sub(nexus_lp.fees_claimed_per_share_b)
            .unwrap();
        assert_eq!(delta_a, 0u128);
        assert_eq!(delta_b, 0u128);
        assert_eq!(compute_claimable(delta_a, nexus_lp.shares).unwrap(), 0u64);
        assert_eq!(compute_claimable(delta_b, nexus_lp.shares).unwrap(), 0u64);
    }
}
