// Arlex `#[derive(Accounts)]` proc-macro generates struct-field bindings
// that aren't always read by every handler — those are purely structural.
// `declare_id!` + `#[program]` emit `cfg(feature = "solana")` gates that
// the contract crates don't expose as a feature. Crate-level allow keeps
// the noise out of `cargo build`. Real unused-var bugs in handler logic
// are caught by `clippy --strict`.
#![allow(unused_variables, unexpected_cfgs)]

//! Native DEX — AMM with constant product and concentrated liquidity pools.
//!
//! Purpose-built AMM for trading OT and RWT tokens. All pools pair with RWT.
//! Fee split: LP (auto-compound) + protocol (Areal) + optional OT Treasury.
//!
//! 15 instructions (Layer 4 StandardCurve + Layer 5 Concentrated + Layer 8
//! `compound_yield`), 5 PDA accounts, 13 events. Nexus (Layer 9) deferred.
//!
//! Built on Arlex framework (Pinocchio). Classic SPL Token only.
//! See docs/contracts/native-dex.mdx for full specification.
//!
//! # BPF stack-frame budget
//!
//! Layer 9 Substep 5 narrowed the entrypoint dispatcher's worst-case frame
//! from ~4400B to 4160B (8B over the SBF 4096-byte budget) via
//! `#[inline(never)]` on every handler entry, `token_program` slot removal
//! in the four Nexus handlers, and manual byte-compare for `has_one =
//! authority` on `nexus_withdraw_profits` / `nexus_claim_rewards`. The 8B
//! overshoot proved to be runtime-live, not a static-analysis artefact:
//! Layer 10 SD-32 closure (2026-04-29) observed
//! `Access violation in stack frame 1 at address 0x200001ff8 of size 8`
//! in `add_liquidity` (Phase F + Phase L of the bootstrap chain).
//!
//! Layer 10 R46 closed the overshoot by extracting the LP-fee auto-claim
//! outbound branch into a `#[inline(never)]` child helper
//! (`add_liquidity::auto_claim_lp_fees`) shared between
//! `add_liquidity_internal` and `zap_liquidity_internal`. The helper's
//! `[Seed; 4]` + `Signer` allocations now live in a child stack frame
//! instead of unioning with the parent's fresh-init `Signer` locals. Net
//! frame estimate post-R46: ~3880B (>200B headroom). Future ix or
//! refactors should re-validate via `cargo build-sbf` and grep
//! `'Stack offset'` in the build log.
//!
//! # Unsafe (L-5 audit note)
//!
//! `unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) }`
//! blocks in this crate are the standard Pinocchio zero-copy pattern. Every
//! such usage is followed by an explicit length check before indexing. See
//! `validation.rs` for the full commented reference implementation.

extern crate alloc;

use arlex_lang::prelude::*;

pub mod constants;
pub mod error;
pub mod events;
pub mod state;
pub mod amm;
pub mod concentrated;
pub mod validation;
pub mod pool_creation;
pub(crate) mod cpi;
pub mod instructions;

// Re-export instruction account structs for #[program] macro access
use instructions::initialize_dex::InitializeDex;
use instructions::create_pool::CreatePool;
use instructions::create_concentrated_pool::CreateConcentratedPool;
use instructions::add_liquidity::AddLiquidity;
use instructions::zap_liquidity::ZapLiquidity;
use instructions::remove_liquidity::RemoveLiquidity;
use instructions::swap::Swap;
use instructions::shift_liquidity::ShiftLiquidity;
use instructions::update_dex_config::UpdateDexConfig;
use instructions::update_areal_fee_destination::UpdateArealFeeDestination;
use instructions::update_pool_creators::UpdatePoolCreators;
use instructions::pause::{PausePool, UnpausePool};
use instructions::authority_transfer::{ProposeAuthorityTransfer, AcceptAuthorityTransfer};
use instructions::compound_yield::CompoundYield;
// Layer 9 (Liquidity Nexus)
use instructions::initialize_nexus::InitializeNexus;
use instructions::update_nexus_manager::UpdateNexusManager;
use instructions::nexus_swap::NexusSwap;
use instructions::nexus_add_liquidity::NexusAddLiquidity;
use instructions::nexus_remove_liquidity::NexusRemoveLiquidity;
use instructions::nexus_deposit::NexusDeposit;
use instructions::nexus_record_deposit::NexusRecordDeposit;
use instructions::nexus_withdraw_profits::NexusWithdrawProfits;
use instructions::nexus_claim_rewards::NexusClaimRewards;
// Layer 9 D28 (LP-fee accumulator + claim_lp_fees)
use instructions::claim_lp_fees::ClaimLpFees;

declare_id!("DEX8LmvJpjefPS1cGS9zWB9ybxN24vNjTTrusBeqyARL");

#[program]
pub mod native_dex {
    use super::*;

    /// Create global DEX configuration and pool creators whitelist. Called once.
    #[inline(never)]
    pub fn initialize_dex(
        ctx: Context<InitializeDex>,
        areal_fee_destination: [u8; 32],
        pause_authority: [u8; 32],
        rebalancer: [u8; 32],
    ) -> Result<()> {
        crate::instructions::initialize_dex::handler(ctx, areal_fee_destination, pause_authority, rebalancer)
    }

    /// Create a StandardCurve (constant product) pool for a token pair.
    #[inline(never)]
    pub fn create_pool(ctx: Context<CreatePool>) -> Result<()> {
        crate::instructions::create_pool::handler(ctx)
    }

    /// Create a concentrated liquidity pool with BinArray.
    #[inline(never)]
    pub fn create_concentrated_pool(
        ctx: Context<CreateConcentratedPool>,
        bin_step_bps: u16,
        initial_active_bin: i32,
    ) -> Result<()> {
        crate::instructions::create_concentrated_pool::handler(ctx, bin_step_bps, initial_active_bin)
    }

    /// Add liquidity to a pool. Receive LP shares proportional to deposit.
    /// For concentrated pools, pass BinArray as last remaining_account.
    #[inline(never)]
    pub fn add_liquidity(ctx: Context<AddLiquidity>, amount_a: u64, amount_b: u64, min_shares: u128) -> Result<()> {
        crate::instructions::add_liquidity::handler(ctx, amount_a, amount_b, min_shares)
    }

    /// Atomic single-token or imbalanced deposit → LP. Swaps excess internally.
    #[inline(never)]
    pub fn zap_liquidity(ctx: Context<ZapLiquidity>, amount_a: u64, amount_b: u64, min_shares: u128) -> Result<()> {
        crate::instructions::zap_liquidity::handler(ctx, amount_a, amount_b, min_shares)
    }

    /// Remove liquidity. Proportional withdrawal. Works even when pool is paused.
    #[inline(never)]
    pub fn remove_liquidity(ctx: Context<RemoveLiquidity>, shares_to_burn: u128) -> Result<()> {
        crate::instructions::remove_liquidity::handler(ctx, shares_to_burn)
    }

    /// Swap tokens through a pool. Fee direction depends on RWT side.
    /// For concentrated pools, pass BinArray as remaining_account (after OT treasury if present).
    #[inline(never)]
    pub fn swap(ctx: Context<Swap>, amount_in: u64, min_amount_out: u64, a_to_b: bool) -> Result<()> {
        crate::instructions::swap::handler(ctx, amount_in, min_amount_out, a_to_b)
    }

    /// Redistribute bin liquidity to track NAV price. Rebalancer only.
    #[inline(never)]
    pub fn shift_liquidity(
        ctx: Context<ShiftLiquidity>,
        nav_bin: i32,
        target_bin_count: u16,
    ) -> Result<()> {
        crate::instructions::shift_liquidity::handler(ctx, nav_bin, target_bin_count)
    }

    /// Update DEX config: fees, rebalancer, active status. Authority only.
    #[inline(never)]
    pub fn update_dex_config(
        ctx: Context<UpdateDexConfig>,
        base_fee_bps: u16,
        lp_fee_share_bps: u16,
        rebalancer: [u8; 32],
        is_active: bool,
    ) -> Result<()> {
        crate::instructions::update_dex_config::handler(ctx, base_fee_bps, lp_fee_share_bps, rebalancer, is_active)
    }

    /// Rotate `DexConfig.areal_fee_destination` to a new RWT ATA. Authority only.
    /// Validates on-chain that the new destination's mint == `RWT_MINT` so
    /// misconfiguration cannot brick `swap` / `zap_liquidity` (P0-2 follow-up).
    /// Idempotent — passing the current destination is a no-op (no event).
    #[inline(never)]
    pub fn update_areal_fee_destination(
        ctx: Context<UpdateArealFeeDestination>,
    ) -> Result<()> {
        crate::instructions::update_areal_fee_destination::handler(ctx)
    }

    /// Add or remove a pool creator from the whitelist. Authority only.
    /// action: 0 = Add, 1 = Remove.
    #[inline(never)]
    pub fn update_pool_creators(
        ctx: Context<UpdatePoolCreators>,
        wallet: [u8; 32],
        action: u8,
    ) -> Result<()> {
        crate::instructions::update_pool_creators::handler(ctx, wallet, action)
    }

    /// Emergency pause a specific pool. Pause authority only.
    #[inline(never)]
    pub fn pause_pool(ctx: Context<PausePool>) -> Result<()> {
        crate::instructions::pause::pause_handler(ctx)
    }

    /// Unpause a specific pool. Pause authority only.
    #[inline(never)]
    pub fn unpause_pool(ctx: Context<UnpausePool>) -> Result<()> {
        crate::instructions::pause::unpause_handler(ctx)
    }

    /// Step 1: Current authority proposes a new authority.
    #[inline(never)]
    pub fn propose_authority_transfer(
        ctx: Context<ProposeAuthorityTransfer>,
        new_authority: [u8; 32],
    ) -> Result<()> {
        crate::instructions::authority_transfer::propose_handler(ctx, new_authority)
    }

    /// Step 2: Proposed authority accepts. Updates both dex_config + pool_creators.
    #[inline(never)]
    pub fn accept_authority_transfer(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
        crate::instructions::authority_transfer::accept_handler(ctx)
    }

    /// Pool PDA claims vested RWT from a Yield Distribution distributor and
    /// folds the received RWT into `reserve_<rwt_side>` (auto-compound for
    /// LPs). Permissionless — any wallet can act as crank (pays ClaimStatus
    /// rent on first claim). Layer 8 §5.3.
    #[inline(never)]
    pub fn compound_yield(
        ctx: Context<CompoundYield>,
        cumulative_amount: u64,
        proof: alloc::vec::Vec<[u8; 32]>,
    ) -> Result<()> {
        crate::instructions::compound_yield::handler(ctx, cumulative_amount, proof)
    }

    // ----- Layer 9 (Liquidity Nexus) -----

    /// Bootstrap the singleton `LiquidityNexus` PDA + initial Manager wallet.
    /// Authority-gated. Single-init enforced by Arlex `init` constraint.
    /// Layer 9 §4.1.
    #[inline(never)]
    pub fn initialize_nexus(
        ctx: Context<InitializeNexus>,
        manager: [u8; 32],
    ) -> Result<()> {
        crate::instructions::initialize_nexus::handler(ctx, manager)
    }

    /// Rotate the Nexus Manager wallet. Authority-gated. Setting
    /// `new_manager == [0u8; 32]` is the documented on-chain kill-switch
    /// (D22) — Manager-gated ix revert with `NexusManagerDisabled` until
    /// Authority rotates back to a non-zero key. Layer 9 §4.8.
    #[inline(never)]
    pub fn update_nexus_manager(
        ctx: Context<UpdateNexusManager>,
        new_manager: [u8; 32],
    ) -> Result<()> {
        crate::instructions::update_nexus_manager::handler(ctx, new_manager)
    }

    /// Manager-gated swap with the Nexus PDA acting as swap authority.
    /// Layer 9 §4.3. Revert order: `NexusNotActive` (Nexus disabled) →
    /// `NexusManagerDisabled` (kill-switch sentinel) → `InvalidNexusManager`
    /// (signer mismatch) → swap-internal reverts (slippage, math, etc.).
    /// Reuses `swap_internal` per D23 — same code path as user-signed
    /// `swap`, only the inbound transfer is PDA-signed.
    #[inline(never)]
    pub fn nexus_swap(
        ctx: Context<NexusSwap>,
        amount_in: u64,
        min_amount_out: u64,
        a_to_b: bool,
    ) -> Result<()> {
        crate::instructions::nexus_swap::handler(ctx, amount_in, min_amount_out, a_to_b)
    }

    /// Manager-gated add-liquidity with the Nexus PDA acting as LP
    /// authority. Layer 9 §4.4. Manager wallet additionally pays rent on
    /// first `LpPosition` creation (Substep 3 architect-review M-1).
    /// Reuses `add_liquidity_internal` per D23 — D29 invariants
    /// (snapshot init for fresh position, auto-claim for existing
    /// position) inherit automatically.
    #[inline(never)]
    pub fn nexus_add_liquidity(
        ctx: Context<NexusAddLiquidity>,
        amount_a: u64,
        amount_b: u64,
        min_shares: u128,
    ) -> Result<()> {
        crate::instructions::nexus_add_liquidity::handler(ctx, amount_a, amount_b, min_shares)
    }

    /// Manager-gated remove-liquidity with the Nexus PDA acting as LP
    /// authority. Layer 9 §4.5. Reuses `remove_liquidity_internal` per
    /// D23 — D30 (auto-claim pending fees BEFORE share reduction)
    /// inherits automatically. Rent refund on full close goes to the
    /// Nexus PDA, per Substep 3 architect-review M-2.
    #[inline(never)]
    pub fn nexus_remove_liquidity(
        ctx: Context<NexusRemoveLiquidity>,
        shares_to_burn: u128,
    ) -> Result<()> {
        crate::instructions::nexus_remove_liquidity::handler(ctx, shares_to_burn)
    }

    /// Permissionless deposit of USDC or RWT into the Nexus singleton's token
    /// ATA. Bumps the matching `total_deposited_<token>` principal counter
    /// (monotonically non-decreasing) and emits `NexusDeposited` with
    /// `source_kind = SOURCE_DIRECT`. Layer 9 §4.2.
    ///
    /// `#[inline(never)]` on every Layer 9 Substep 5 entry keeps the
    /// dispatcher's per-arm frame small enough that the union frame stays
    /// within the SBF stack budget (see crate-level "BPF stack-frame
    /// budget" note).
    #[inline(never)]
    pub fn nexus_deposit(
        ctx: Context<NexusDeposit>,
        amount: u64,
        token_kind: u8,
    ) -> Result<()> {
        crate::instructions::nexus_deposit::handler(ctx, amount, token_kind)
    }

    /// CPI-only counter-bump invoked by Yield Distribution's
    /// `withdraw_liquidity_holding`. Three D25 caller-validation checks
    /// (program-id ownership, PDA derivation, signer flag) gate the call;
    /// the actual SPL Transfer is performed upstream in YD. Layer 9 §4.9.
    #[inline(never)]
    pub fn nexus_record_deposit(
        ctx: Context<NexusRecordDeposit>,
        amount: u64,
        token_kind: u8,
    ) -> Result<()> {
        crate::instructions::nexus_record_deposit::handler(ctx, amount, token_kind)
    }

    /// Authority-gated sweep of the Nexus's "above-principal" balance to
    /// the Treasury (typically `dex_config.areal_fee_destination`).
    /// Enforces the principal-lock invariant — `ata_balance` below
    /// `total_deposited_<token>` reverts; the counter is NOT decremented
    /// on success. Layer 9 §4.6.
    #[inline(never)]
    pub fn nexus_withdraw_profits(
        ctx: Context<NexusWithdrawProfits>,
        amount: u64,
        token_kind: u8,
    ) -> Result<()> {
        crate::instructions::nexus_withdraw_profits::handler(ctx, amount, token_kind)
    }

    /// Authority-gated realisation of LP fees on the Nexus's `LpPosition`
    /// for a given pool. Composes `claim_lp_fees_internal` (D28) with the
    /// Nexus PDA filling the `authority` slot; claimed fees route back
    /// into the Nexus-owned token A and token B ATAs and accumulate as
    /// withdrawable profit above the principal floor. Layer 9 §4.7 / SD-1.
    #[inline(never)]
    pub fn nexus_claim_rewards(ctx: Context<NexusClaimRewards>) -> Result<()> {
        crate::instructions::nexus_claim_rewards::handler(ctx)
    }

    // ----- Layer 9 D28 (LP-fee accumulator + claim_lp_fees) -----

    /// Realise accumulator-tracked LP fees on both sides of a pool for the
    /// signer's `LpPosition`. Permissionless — any LP can claim their own
    /// position. PDA-signed Transfers from the pool vaults to the
    /// recipient's ATAs are skipped per side when the side's claimable is
    /// zero. Layer 9 D28 — companion ix to the swap-time
    /// `cumulative_fees_per_share_<side>` accumulator update.
    #[inline(never)]
    pub fn claim_lp_fees(ctx: Context<ClaimLpFees>) -> Result<()> {
        crate::instructions::claim_lp_fees::handler(ctx)
    }
}
