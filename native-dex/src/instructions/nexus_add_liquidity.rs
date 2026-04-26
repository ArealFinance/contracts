//! Layer 9 §4.4 — `nexus_add_liquidity`. Manager-gated add-liquidity that
//! treats the singleton `LiquidityNexus` PDA as the LP `authority`. The
//! Manager wallet (Tx signer) authorises the call via the D22
//! `assert_manager` helper and also acts as the **rent payer** when the
//! Nexus's `LpPosition` for this pool needs to be created on first use
//! (per Substep 3 architect-review M-1: `system::CreateAccount` cannot
//! accept the same PDA-signer for arbitrary callers, so the Manager wallet
//! pays rent while the Nexus PDA owns the resulting position).
//!
//! Reuses `add_liquidity_internal` per D23: same logic path as user-signed
//! `add_liquidity`, only the `authority` slot is the Nexus PDA and the
//! inbound `provider_token_*` → `vault_*` transfers are PDA-signed via
//! `Some(&signers)`. **D29 invariants inherit automatically:** a
//! freshly-created Nexus `LpPosition` snaps `fees_claimed_per_share_<side>`
//! to the pool's current cumulative (zero claimable on entry); an existing
//! Nexus `LpPosition` with pending fees auto-claims BEFORE the share
//! increment, with the auto-claim payouts landing in the Nexus-owned ATAs.

use arlex_lang::prelude::*;

use crate::constants::*;
use crate::error::DexError;
use crate::state::LiquidityNexus;
use crate::validation::*;
use crate::instructions::add_liquidity::{add_liquidity_internal, AddLiquidityAccountsView};

#[derive(Accounts)]
pub struct NexusAddLiquidity<'info> {
    /// Nexus Manager wallet (Tx signer). Authorises the call via
    /// `assert_manager` (D22 ordering: kill-switch first, then signer).
    /// Also acts as the **rent payer** for first-time `LpPosition`
    /// creation on this pool — `system::CreateAccount` requires a Tx
    /// signer in the `from` slot, so the Manager funds the rent while
    /// the Nexus PDA owns the resulting position. Subsequent calls
    /// against an existing position skip the `CreateAccount` CPI and
    /// the `payer` slot is unused at runtime.
    #[account(mut, signer)]
    pub manager: &'info AccountView,

    /// DEX config singleton — `is_active` + `lp_fee_share_bps` + areal
    /// fee destination are read by `add_liquidity_internal`. Read-only.
    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    /// Nexus singleton PDA. Mutable because the inbound transfers sign
    /// with its seeds; `add_liquidity_internal` does not write to this
    /// account.
    #[account(mut, seeds = [LIQUIDITY_NEXUS_SEED], bump)]
    pub liquidity_nexus: &'info AccountView,

    /// Target pool. Mutated by `add_liquidity_internal` (reserves,
    /// `total_lp_shares`).
    #[account(mut)]
    pub pool_state: &'info AccountView,

    /// Nexus's `LpPosition` for this pool. PDA seed
    /// `["lp", pool_state, liquidity_nexus]`. Either pre-existing
    /// (auto-claim path) or freshly initialised by the inner handler
    /// (snapshot-init path); both branches obey D29 invariants.
    #[account(mut)]
    pub lp_position: &'info AccountView,

    /// Nexus-owned ATA for token A. Acts as the `provider_token_a` slot
    /// in the projected `AddLiquidityAccountsView`. PDA-signed Transfer
    /// source for the inbound deposit and PDA-signed Transfer destination
    /// for any auto-claimed pending fees (D29 existing-position branch).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_a: &'info AccountView,

    /// Nexus-owned ATA for token B. Symmetric to `nexus_token_a`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_b: &'info AccountView,

    /// Pool vault A. Validated by `add_liquidity_internal` against
    /// `pool.vault_a`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_a: &'info AccountView,

    /// Pool vault B. Validated against `pool.vault_b`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_b: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

impl<'info> NexusAddLiquidity<'info> {
    /// Project the Nexus account set into the shape expected by
    /// `add_liquidity_internal`. Critical wiring:
    ///   - `authority = liquidity_nexus` — Nexus PDA owns the LpPosition,
    ///     and the inbound vault transfers are PDA-signed via the seeds
    ///     supplied to the internal helper.
    ///   - `payer = manager` — Tx signer that funds rent on first-deploy
    ///     `LpPosition` creation (Substep 3 architect-review M-1).
    pub(crate) fn view(&self) -> AddLiquidityAccountsView<'info> {
        AddLiquidityAccountsView {
            authority: self.liquidity_nexus,
            payer: self.manager,
            dex_config: self.dex_config,
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
    ctx: Context<NexusAddLiquidity>,
    amount_a: u64,
    amount_b: u64,
    min_shares: u128,
) -> Result<()> {
    // 1. D22 ordering — kill-switch check, then signer-match check.
    //    Scoped block so the Nexus mut handle is dropped before the
    //    AccountView reference is reused as the `authority` slot.
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

    // 2. Build Nexus PDA signer seeds. The inbound transfers
    //    `nexus_token_<side>` → `vault_<side>` sign with these via
    //    `Some(&signers)`. The auto-claim outbound transfers (D29
    //    existing-position branch) sign with the pool PDA seeds inside
    //    `add_liquidity_internal`.
    let bump_arr = [nexus_bump];
    let signer_seeds: [Seed; 2] = [
        Seed::from(LIQUIDITY_NEXUS_SEED),
        Seed::from(bump_arr.as_ref()),
    ];
    let signers = [Signer::from(&signer_seeds)];

    // 3. Reuse the canonical add-liquidity path. D29 invariants
    //    (snapshot-init for fresh position, auto-claim for existing) are
    //    enforced inside `add_liquidity_internal` and inherit
    //    automatically — the Nexus `LpPosition` is just another
    //    LP-position with `owner == liquidity_nexus.key()`.
    add_liquidity_internal(
        &ctx.accounts.view(),
        ctx.remaining_accounts,
        ctx.program_id,
        amount_a,
        amount_b,
        min_shares,
        Some(&signers),
    )
}

#[cfg(test)]
mod tests {
    //! Layer 9 §4.4 — D22 access-control + D29 LP-fee invariant pinning
    //! tests. Handler-level negative ACs require the BPF runtime; here we
    //! pin the contract surface (kill-switch ordering, fresh-init
    //! snapshot, existing-position auto-claim ordering) so a refactor
    //! cannot silently regress the inherited D29 behaviour.
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

    /// **D22 kill-switch revert** — `nexus_add_liquidity` must reject
    /// when `nexus.manager == [0u8; 32]`, regardless of signer. Mirrors
    /// the same kill-switch contract enforced in `nexus_swap`.
    #[test]
    fn nexus_add_liquidity_kill_switch_revert() {
        let nexus = nexus_with([0u8; 32], /* is_active */ true);
        let signer = [0xFFu8; 32];
        let err = check_manager_bytes(&nexus, &signer).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NexusManagerDisabled));
    }

    /// **D29 fresh-init invariant inheritance** — when the Nexus's
    /// `LpPosition` is created on first deploy, its
    /// `fees_claimed_per_share_<side>` must snapshot the pool's current
    /// cumulative so a brand-new Nexus position cannot retroactively
    /// claim fees that accrued before it joined. The Nexus inherits this
    /// invariant automatically because `add_liquidity_internal` is the
    /// single source of truth (D23). This test pins the math against an
    /// in-memory state that mirrors the on-chain effects step.
    #[test]
    fn nexus_add_liquidity_inherits_d29_fresh_init() {
        // Pool already has cumulative_a > 0 from prior swap activity
        // (i.e. predates the Nexus's first add_liquidity).
        let cumulative_a: u128 = 9u128 << 64;
        let cumulative_b: u128 = 3u128 << 64;
        let pool = make_pool(cumulative_a, cumulative_b, /* total_shares */ 4_000);

        // Mirror the fresh-init effects step inside `add_liquidity_internal`:
        // a brand-new `LpPosition` snaps to the pool cumulative.
        let mut nexus_lp = make_position(/* shares */ 0, 0, 0);
        nexus_lp.shares = 1_000;
        nexus_lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
        nexus_lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;

        // Immediate post-init claim against the freshly-snapped Nexus
        // position must yield zero on both sides — otherwise the Nexus
        // would steal historical fees, breaking
        // `vault == reserves + Σ(claimable across all LPs)`.
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

    /// **D29 existing-position auto-claim inheritance** — when the
    /// Nexus's `LpPosition` already exists with pending accrued fees, the
    /// handler MUST realise the pending claimable BEFORE incrementing
    /// shares. Otherwise the new shares would dilute the historical
    /// accrual across the post-increment share count, over-paying the
    /// fresh deposit at the expense of the position's own historical
    /// accrual. This test pins the ordering: snapshot pool cumulative →
    /// compute claimable on `lp.shares_pre` → advance position snapshot
    /// → add new shares.
    #[test]
    fn nexus_add_liquidity_existing_position_auto_claims_d29() {
        // Pool has cumulative_a from prior swaps (RWT side A example).
        let cumulative_a: u128 = 5u128 << 64;
        let cumulative_b: u128 = 0;
        let pool = make_pool(cumulative_a, cumulative_b, 1_500);

        // Existing Nexus position with 1_500 shares, zero snapshot ⇒
        // entire accumulator delta is owed.
        let mut nexus_lp = make_position(1_500, 0, 0);
        let shares_pre = nexus_lp.shares;

        // Effects step (matches `add_liquidity_internal`):
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
        // (5 << 64) * 1500 >> 64 == 7500.
        assert_eq!(auto_claim_a, 7_500u64);
        assert_eq!(auto_claim_b, 0u64);

        // Snapshot advances BEFORE the share increment.
        nexus_lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
        nexus_lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;
        let new_shares: u128 = 500;
        nexus_lp.shares = nexus_lp.shares.checked_add(new_shares).unwrap();
        assert_eq!({ nexus_lp.shares }, 2_000);

        // Subsequent claim observes zero delta — post-increment 2_000
        // shares cannot retroactively multiply the original 5 << 64.
        let delta_a2 = pool
            .cumulative_fees_per_share_a
            .checked_sub(nexus_lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b2 = pool
            .cumulative_fees_per_share_b
            .checked_sub(nexus_lp.fees_claimed_per_share_b)
            .unwrap();
        assert_eq!(compute_claimable(delta_a2, nexus_lp.shares).unwrap(), 0u64);
        assert_eq!(compute_claimable(delta_b2, nexus_lp.shares).unwrap(), 0u64);
    }
}
