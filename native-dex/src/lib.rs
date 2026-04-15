//! Native DEX — AMM with constant product pools and fee model with OT Treasury support.
//!
//! Purpose-built AMM for trading OT and RWT tokens. All pools pair with RWT.
//! Fee split: LP (auto-compound) + protocol (Areal) + optional OT Treasury.
//!
//! 12 instructions (Layer 4 scope: StandardCurve only), 4 PDA accounts, 11 events.
//! Concentrated pools (Layer 5), compound_yield (Layer 8), Nexus (Layer 9) deferred.
//!
//! Built on Arlex framework (Pinocchio). Classic SPL Token only.
//! See docs/contracts/native-dex.mdx for full specification.

extern crate alloc;

use arlex_lang::prelude::*;

pub mod constants;
pub mod error;
pub mod events;
pub mod state;
pub mod amm;
pub mod validation;
pub mod instructions;

// Re-export instruction account structs for #[program] macro access
use instructions::initialize_dex::InitializeDex;
use instructions::create_pool::CreatePool;
use instructions::add_liquidity::AddLiquidity;
use instructions::zap_liquidity::ZapLiquidity;
use instructions::remove_liquidity::RemoveLiquidity;
use instructions::swap::Swap;
use instructions::update_dex_config::UpdateDexConfig;
use instructions::update_pool_creators::UpdatePoolCreators;
use instructions::pause::{PausePool, UnpausePool};
use instructions::authority_transfer::{ProposeAuthorityTransfer, AcceptAuthorityTransfer};

declare_id!("11111111111111111111111111111111"); // TODO: set after deploy

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

    /// Add liquidity to a pool. Receive LP shares proportional to deposit.
    pub fn add_liquidity(ctx: Context<AddLiquidity>, amount_a: u64, amount_b: u64) -> Result<()> {
        crate::instructions::add_liquidity::handler(ctx, amount_a, amount_b)
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
    pub fn swap(ctx: Context<Swap>, amount_in: u64, min_amount_out: u64, a_to_b: bool) -> Result<()> {
        crate::instructions::swap::handler(ctx, amount_in, min_amount_out, a_to_b)
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
}
