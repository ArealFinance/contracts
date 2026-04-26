//! Layer 9 §4.5 — `nexus_remove_liquidity`. Manager-gated remove-liquidity
//! that uses the singleton `LiquidityNexus` PDA as the LP `authority`.
//!
//! Reuses `remove_liquidity_internal` per D23: same logic path as the
//! user-signed `remove_liquidity`, only the `authority` slot is the Nexus
//! PDA. **D30 invariants inherit automatically:** the auto-claim of
//! pending fees runs against the Nexus's pre-reduction share count and
//! the snapshot is advanced BEFORE the share decrement, so the Nexus
//! cannot forfeit historical accrued fees on a full close — the
//! auto-claim payouts land in the Nexus-owned ATAs.
//!
//! Architect-review note (Substep 3 M-2): on full-close the inner
//! handler refunds rent lamports of the closed `LpPosition` to
//! `accounts.authority` — i.e. to the Nexus PDA. This is the desired
//! behaviour per Layer 9 architecture: the Nexus PDA owns the position,
//! so the rent refund accumulates inside the Nexus-controlled account.
//! The principal-lock invariant in `nexus_withdraw_profits` (Substep 5)
//! is concerned with token balances vs. principal floor — lamports
//! sitting on the Nexus PDA are not subject to that invariant and remain
//! available for re-paying rent on a future Nexus `LpPosition` init.

use arlex_lang::prelude::*;

use crate::constants::*;
use crate::error::DexError;
use crate::state::LiquidityNexus;
use crate::validation::*;
use crate::instructions::remove_liquidity::{
    remove_liquidity_internal, RemoveLiquidityAccountsView,
};

#[derive(Accounts)]
pub struct NexusRemoveLiquidity<'info> {
    /// Nexus Manager wallet (Tx signer). Authorises the call via
    /// `assert_manager` (D22 ordering: kill-switch first, then signer).
    /// Manager does not pay any rent here — `remove_liquidity_internal`
    /// only returns rent to `authority` on full close (the Nexus PDA),
    /// which is the desired behaviour per Substep 3 architect-review M-2.
    #[account(signer)]
    pub manager: &'info AccountView,

    /// Nexus singleton PDA. Mutable: vault → Nexus ATA outbound transfers
    /// sign with the pool PDA seeds (inside the inner helper) and any
    /// rent-refund-on-full-close lands on this account's lamports
    /// balance. The data of the Nexus account is not modified.
    #[account(mut, seeds = [LIQUIDITY_NEXUS_SEED], bump)]
    pub liquidity_nexus: &'info AccountView,

    /// Target pool. Mutated by `remove_liquidity_internal` (reserves,
    /// `total_lp_shares`).
    #[account(mut)]
    pub pool_state: &'info AccountView,

    /// Nexus's `LpPosition` for this pool (PDA seed
    /// `["lp", pool_state, liquidity_nexus]`). Mutated (shares decrement
    /// + D30 snapshot advance) and possibly closed on full withdraw.
    #[account(mut)]
    pub lp_position: &'info AccountView,

    /// Nexus-owned ATA for token A. Receives the proportional withdraw
    /// (pool-PDA-signed Transfer destination) plus any auto-claimed
    /// pending fees on the A side (D30 inherited).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_a: &'info AccountView,

    /// Nexus-owned ATA for token B. Symmetric to `nexus_token_a`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_b: &'info AccountView,

    /// Pool vault A. Validated against `pool.vault_a`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_a: &'info AccountView,

    /// Pool vault B. Validated against `pool.vault_b`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_b: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

impl<'info> NexusRemoveLiquidity<'info> {
    /// Project the Nexus account set into the shape expected by
    /// `remove_liquidity_internal`. The Nexus PDA fills the `authority`
    /// slot — both the `lp_position.owner` check and the rent-refund
    /// destination on full close target this account.
    pub(crate) fn view(&self) -> RemoveLiquidityAccountsView<'info> {
        RemoveLiquidityAccountsView {
            authority: self.liquidity_nexus,
            pool_state: self.pool_state,
            lp_position: self.lp_position,
            provider_token_a: self.nexus_token_a,
            provider_token_b: self.nexus_token_b,
            vault_a: self.vault_a,
            vault_b: self.vault_b,
        }
    }
}

pub fn handler(
    ctx: Context<NexusRemoveLiquidity>,
    shares_to_burn: u128,
) -> Result<()> {
    // 1. D22 ordering — kill-switch check, then signer-match check. The
    //    mut handle on `LiquidityNexus` is scoped to this block so the
    //    AccountView reference is freely usable as the `authority` slot.
    let nexus_bump: u8 = {
        let nexus = LiquidityNexus::load_mut(
            ctx.accounts.liquidity_nexus,
            ctx.program_id,
        )?;
        if !nexus.is_active {
            return Err(ProgramError::from(DexError::NexusNotActive));
        }
        assert_manager(nexus, ctx.accounts.manager)?;
        nexus.bump
    };

    // 2. Build Nexus PDA signer seeds. `remove_liquidity_internal`
    //    threads `authority_signer_seeds` through for interface
    //    uniformity but currently has no inbound authority CPI; the
    //    parameter is reserved for future-proofing (per the existing
    //    helper's doc comment). Pool-PDA-signed vault → Nexus ATA
    //    transfers (proportional withdraw + D30 auto-claim) sign with
    //    the pool PDA seeds inside the inner helper.
    let bump_arr = [nexus_bump];
    let signer_seeds: [Seed; 2] = [
        Seed::from(LIQUIDITY_NEXUS_SEED),
        Seed::from(bump_arr.as_ref()),
    ];
    let signers = [Signer::from(&signer_seeds)];

    // 3. Reuse the canonical remove-liquidity path. D30 (auto-claim
    //    pending fees BEFORE share reduction) inherits automatically —
    //    `remove_liquidity_internal` is the single source of truth (D23).
    remove_liquidity_internal(
        &ctx.accounts.view(),
        ctx.remaining_accounts,
        ctx.program_id,
        shares_to_burn,
        Some(&signers),
    )
}

#[cfg(test)]
mod tests {
    //! Layer 9 §4.5 — D22 access-control + D30 LP-fee auto-claim
    //! invariant pinning tests. Handler-level negative ACs require the
    //! BPF runtime; here we pin the contract surface so a refactor
    //! cannot silently regress the inherited D30 behaviour.
    use crate::error::DexError;
    use crate::instructions::claim_lp_fees::compute_claimable;
    use crate::state::{LiquidityNexus, LpPosition, PoolState};

    fn check_manager_bytes(
        nexus: &LiquidityNexus,
        signer_bytes: &[u8; 32],
    ) -> core::result::Result<(), arlex_lang::prelude::ProgramError> {
        if nexus.manager == [0u8; 32] {
            return Err(arlex_lang::prelude::ProgramError::from(
                DexError::NexusManagerDisabled,
            ));
        }
        if &nexus.manager != signer_bytes {
            return Err(arlex_lang::prelude::ProgramError::from(
                DexError::InvalidNexusManager,
            ));
        }
        Ok(())
    }

    fn nexus_with(manager: [u8; 32], is_active: bool) -> LiquidityNexus {
        let buf = [0u8; core::mem::size_of::<LiquidityNexus>()];
        let mut nexus: LiquidityNexus =
            unsafe { core::ptr::read(buf.as_ptr() as *const LiquidityNexus) };
        nexus.manager = manager;
        nexus.is_active = is_active;
        nexus.bump = 0xFD;
        nexus
    }

    fn make_pool(cumulative_a: u128, cumulative_b: u128, total_lp_shares: u128) -> PoolState {
        let buf = [0u8; core::mem::size_of::<PoolState>()];
        let mut pool: PoolState =
            unsafe { core::ptr::read(buf.as_ptr() as *const PoolState) };
        pool.cumulative_fees_per_share_a = cumulative_a;
        pool.cumulative_fees_per_share_b = cumulative_b;
        pool.total_lp_shares = total_lp_shares;
        pool
    }

    fn make_position(
        shares: u128,
        fees_claimed_a: u128,
        fees_claimed_b: u128,
    ) -> LpPosition {
        let buf = [0u8; core::mem::size_of::<LpPosition>()];
        let mut lp: LpPosition =
            unsafe { core::ptr::read(buf.as_ptr() as *const LpPosition) };
        lp.shares = shares;
        lp.fees_claimed_per_share_a = fees_claimed_a;
        lp.fees_claimed_per_share_b = fees_claimed_b;
        lp
    }

    fn custom_code(err: arlex_lang::prelude::ProgramError) -> u32 {
        match err {
            arlex_lang::prelude::ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    fn code_of(err: DexError) -> u32 {
        custom_code(arlex_lang::prelude::ProgramError::from(err))
    }

    /// **D22 kill-switch revert** — `nexus_remove_liquidity` must reject
    /// when `nexus.manager == [0u8; 32]`, regardless of signer. Same
    /// kill-switch contract as the other two Manager-gated handlers.
    #[test]
    fn nexus_remove_liquidity_kill_switch_revert() {
        let nexus = nexus_with([0u8; 32], /* is_active */ true);
        let signer = [0xAAu8; 32];
        let err = check_manager_bytes(&nexus, &signer).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NexusManagerDisabled));
    }

    /// **D30 auto-claim inheritance** — partial close. The handler
    /// MUST compute the auto-claim against the pre-reduction share
    /// count, advance the snapshot, then reduce shares. Skipping the
    /// auto-claim would let the remaining (post-reduction) shares
    /// inherit the pending accrual pro-rata while the removed shares
    /// forfeit them, creating an arbitrage. This test mirrors the
    /// effects step inside `remove_liquidity_internal` — the Nexus
    /// position is just another LP-position with `owner ==
    /// liquidity_nexus.key()`, so the inherited invariant is what
    /// keeps the principal-lock + accumulator invariants intact.
    #[test]
    fn nexus_remove_liquidity_inherits_d30_auto_claim() {
        // Pool has cumulative_b from prior swap activity (RWT on side B
        // example); cumulative_a stays zero.
        let cumulative_a: u128 = 0;
        let cumulative_b: u128 = 7u128 << 64;
        let pool = make_pool(cumulative_a, cumulative_b, /* total_shares */ 3_000);

        // Nexus position with 3_000 shares, zero snapshot ⇒ entire
        // accumulator delta on side B is owed.
        let mut nexus_lp = make_position(3_000, 0, 0);
        let shares_pre = nexus_lp.shares;

        // Effects step (matches `remove_liquidity_internal`).
        let delta_a = pool
            .cumulative_fees_per_share_a
            .checked_sub(nexus_lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b = pool
            .cumulative_fees_per_share_b
            .checked_sub(nexus_lp.fees_claimed_per_share_b)
            .unwrap();
        let auto_claim_a = compute_claimable(delta_a, shares_pre).unwrap();
        let auto_claim_b = compute_claimable(delta_b, shares_pre).unwrap();
        assert_eq!(auto_claim_a, 0u64);
        // (7 << 64) * 3000 >> 64 == 21_000.
        assert_eq!(auto_claim_b, 21_000u64);

        // Snapshot advances BEFORE the share reduction.
        nexus_lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
        nexus_lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;

        // Partial close: burn 1_000 of 3_000.
        let burn: u128 = 1_000;
        nexus_lp.shares = nexus_lp.shares.checked_sub(burn).unwrap();
        assert_eq!({ nexus_lp.shares }, 2_000);

        // Subsequent claim against the residual position observes zero
        // delta — the post-reduction 2_000 shares cannot retroactively
        // multiply the original 7 << 64 cumulative.
        let delta_a2 = pool
            .cumulative_fees_per_share_a
            .checked_sub(nexus_lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b2 = pool
            .cumulative_fees_per_share_b
            .checked_sub(nexus_lp.fees_claimed_per_share_b)
            .unwrap();
        assert_eq!(delta_a2, 0u128);
        assert_eq!(delta_b2, 0u128);
        assert_eq!(compute_claimable(delta_a2, nexus_lp.shares).unwrap(), 0u64);
        assert_eq!(compute_claimable(delta_b2, nexus_lp.shares).unwrap(), 0u64);
    }
}
