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
// Layer 9 D28 (LP-fee accumulator + claim_lp_fees)
use instructions::claim_lp_fees::ClaimLpFees;

declare_id!("DEX8LmvJpjefPS1cGS9zWB9ybxN24vNjTTrusBeqyARL");

#[program]
pub mod native_dex {
    use super::*;

    /// Create global DEX configuration and pool creators whitelist. Called once.
    pub fn initialize_dex(
        ctx: Context<InitializeDex>,
        areal_fee_destination: [u8; 32],
        pause_authority: [u8; 32],
        rebalancer: [u8; 32],
    ) -> Result<()> {
        crate::instructions::initialize_dex::handler(ctx, areal_fee_destination, pause_authority, rebalancer)
    }

    /// Create a StandardCurve (constant product) pool for a token pair.
    pub fn create_pool(ctx: Context<CreatePool>) -> Result<()> {
        crate::instructions::create_pool::handler(ctx)
    }

    /// Create a concentrated liquidity pool with BinArray.
    pub fn create_concentrated_pool(
        ctx: Context<CreateConcentratedPool>,
        bin_step_bps: u16,
        initial_active_bin: i32,
    ) -> Result<()> {
        crate::instructions::create_concentrated_pool::handler(ctx, bin_step_bps, initial_active_bin)
    }

    /// Add liquidity to a pool. Receive LP shares proportional to deposit.
    /// For concentrated pools, pass BinArray as last remaining_account.
    pub fn add_liquidity(ctx: Context<AddLiquidity>, amount_a: u64, amount_b: u64, min_shares: u128) -> Result<()> {
        crate::instructions::add_liquidity::handler(ctx, amount_a, amount_b, min_shares)
    }

    /// Atomic single-token or imbalanced deposit → LP. Swaps excess internally.
    pub fn zap_liquidity(ctx: Context<ZapLiquidity>, amount_a: u64, amount_b: u64, min_shares: u128) -> Result<()> {
        crate::instructions::zap_liquidity::handler(ctx, amount_a, amount_b, min_shares)
    }

    /// Remove liquidity. Proportional withdrawal. Works even when pool is paused.
    pub fn remove_liquidity(ctx: Context<RemoveLiquidity>, shares_to_burn: u128) -> Result<()> {
        crate::instructions::remove_liquidity::handler(ctx, shares_to_burn)
    }

    /// Swap tokens through a pool. Fee direction depends on RWT side.
    /// For concentrated pools, pass BinArray as remaining_account (after OT treasury if present).
    pub fn swap(ctx: Context<Swap>, amount_in: u64, min_amount_out: u64, a_to_b: bool) -> Result<()> {
        crate::instructions::swap::handler(ctx, amount_in, min_amount_out, a_to_b)
    }

    /// Redistribute bin liquidity to track NAV price. Rebalancer only.
    pub fn shift_liquidity(
        ctx: Context<ShiftLiquidity>,
        nav_bin: i32,
        target_bin_count: u16,
    ) -> Result<()> {
        crate::instructions::shift_liquidity::handler(ctx, nav_bin, target_bin_count)
    }

    /// Update DEX config: fees, rebalancer, active status. Authority only.
    pub fn update_dex_config(
        ctx: Context<UpdateDexConfig>,
        base_fee_bps: u16,
        lp_fee_share_bps: u16,
        rebalancer: [u8; 32],
        is_active: bool,
    ) -> Result<()> {
        crate::instructions::update_dex_config::handler(ctx, base_fee_bps, lp_fee_share_bps, rebalancer, is_active)
    }

    /// Add or remove a pool creator from the whitelist. Authority only.
    /// action: 0 = Add, 1 = Remove.
    pub fn update_pool_creators(
        ctx: Context<UpdatePoolCreators>,
        wallet: [u8; 32],
        action: u8,
    ) -> Result<()> {
        crate::instructions::update_pool_creators::handler(ctx, wallet, action)
    }

    /// Emergency pause a specific pool. Pause authority only.
    pub fn pause_pool(ctx: Context<PausePool>) -> Result<()> {
        crate::instructions::pause::pause_handler(ctx)
    }

    /// Unpause a specific pool. Pause authority only.
    pub fn unpause_pool(ctx: Context<UnpausePool>) -> Result<()> {
        crate::instructions::pause::unpause_handler(ctx)
    }

    /// Step 1: Current authority proposes a new authority.
    pub fn propose_authority_transfer(
        ctx: Context<ProposeAuthorityTransfer>,
        new_authority: [u8; 32],
    ) -> Result<()> {
        crate::instructions::authority_transfer::propose_handler(ctx, new_authority)
    }

    /// Step 2: Proposed authority accepts. Updates both dex_config + pool_creators.
    pub fn accept_authority_transfer(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
        crate::instructions::authority_transfer::accept_handler(ctx)
    }

    /// Pool PDA claims vested RWT from a Yield Distribution distributor and
    /// folds the received RWT into `reserve_<rwt_side>` (auto-compound for
    /// LPs). Permissionless — any wallet can act as crank (pays ClaimStatus
    /// rent on first claim). Layer 8 §5.3.
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
    pub fn nexus_remove_liquidity(
        ctx: Context<NexusRemoveLiquidity>,
        shares_to_burn: u128,
    ) -> Result<()> {
        crate::instructions::nexus_remove_liquidity::handler(ctx, shares_to_burn)
    }

    // ----- Layer 9 D28 (LP-fee accumulator + claim_lp_fees) -----

    /// Realise accumulator-tracked LP fees on both sides of a pool for the
    /// signer's `LpPosition`. Permissionless — any LP can claim their own
    /// position. PDA-signed Transfers from the pool vaults to the
    /// recipient's ATAs are skipped per side when the side's claimable is
    /// zero. Layer 9 D28 — companion ix to the swap-time
    /// `cumulative_fees_per_share_<side>` accumulator update.
    pub fn claim_lp_fees(ctx: Context<ClaimLpFees>) -> Result<()> {
        crate::instructions::claim_lp_fees::handler(ctx)
    }
}
