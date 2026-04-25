//! RWT Engine — index token backed by OT positions with NAV-based pricing.
//!
//! RWT (Real World Token) is a single index token backed by a diversified portfolio
//! of OT positions. Users deposit USDC to mint RWT at the current NAV price.
//! A manager actively trades OT positions to grow the portfolio.
//!
//! 11 instructions (Layer 3 + Layer 6), 2 PDA accounts, 9 events.
//! claim_yield (Layer 8) deferred.
//!
//! Built on Arlex framework (Pinocchio). Classic SPL Token only.
//! See docs/contracts/rwt-engine.mdx for full specification.
//!
//! # Unsafe (L-5 audit note)
//!
//! `unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) }`
//! blocks in this crate are the standard Pinocchio zero-copy pattern. Every
//! such usage is followed by an explicit length check before indexing.

extern crate alloc;

use arlex_lang::prelude::*;

pub mod constants;
pub mod error;
pub mod events;
pub mod state;
pub mod nav;
pub mod validation;
pub(crate) mod cpi;
pub mod instructions;

// Re-export instruction account structs for #[program] macro access
use instructions::initialize_vault::InitializeVault;
use instructions::mint_rwt::MintRwt;
use instructions::admin_mint_rwt::AdminMintRwt;
use instructions::adjust_capital::AdjustCapital;
use instructions::update_vault_manager::UpdateVaultManager;
use instructions::update_distribution_config::UpdateDistributionConfig;
use instructions::pause::{PauseMint, UnpauseMint};
use instructions::authority_transfer::{ProposeAuthorityTransfer, AcceptAuthorityTransfer};
use instructions::vault_swap::VaultSwap;

declare_id!("RWT9hgbjHQDj98xP7FYsT5QYp5X32XyK6QfMRmFtARL");

#[program]
pub mod rwt_engine {
    use super::*;

    /// Create the RWT vault with all required accounts. Called once.
    pub fn initialize_vault(
        ctx: Context<InitializeVault>,
        initial_authority: [u8; 32],
        pause_authority: [u8; 32],
        liquidity_destination: [u8; 32],
        protocol_revenue_destination: [u8; 32],
    ) -> Result<()> {
        crate::instructions::initialize_vault::handler(
            ctx, initial_authority, pause_authority,
            liquidity_destination, protocol_revenue_destination,
        )
    }

    /// User deposits USDC and receives RWT at current NAV price.
    /// `min_rwt_out`: slippage protection — tx reverts if output < minimum.
    pub fn mint_rwt(ctx: Context<MintRwt>, amount: u64, min_rwt_out: u64) -> Result<()> {
        crate::instructions::mint_rwt::handler(ctx, amount, min_rwt_out)
    }

    /// Mint RWT backed by OT positions. No USDC deposit required.
    pub fn admin_mint_rwt(
        ctx: Context<AdminMintRwt>,
        rwt_amount: u64,
        backing_capital_usd: u64,
    ) -> Result<()> {
        crate::instructions::admin_mint_rwt::handler(ctx, rwt_amount, backing_capital_usd)
    }

    /// Decrease total_invested_capital (writedown). NAV decreases proportionally.
    pub fn adjust_capital(ctx: Context<AdjustCapital>, writedown_amount: u64) -> Result<()> {
        crate::instructions::adjust_capital::handler(ctx, writedown_amount)
    }

    /// Change the manager wallet that can execute vault_swap.
    pub fn update_vault_manager(
        ctx: Context<UpdateVaultManager>,
        new_manager: [u8; 32],
    ) -> Result<()> {
        crate::instructions::update_vault_manager::handler(ctx, new_manager)
    }

    /// Change yield split ratios and/or destinations.
    pub fn update_distribution_config(
        ctx: Context<UpdateDistributionConfig>,
        book_value_bps: u16,
        liquidity_bps: u16,
        protocol_revenue_bps: u16,
        liquidity_destination: [u8; 32],
        protocol_revenue_destination: [u8; 32],
    ) -> Result<()> {
        crate::instructions::update_distribution_config::handler(
            ctx, book_value_bps, liquidity_bps, protocol_revenue_bps,
            liquidity_destination, protocol_revenue_destination,
        )
    }

    /// Emergency pause — stops all user minting.
    pub fn pause_mint(ctx: Context<PauseMint>) -> Result<()> {
        crate::instructions::pause::pause_handler(ctx)
    }

    /// Resume minting after emergency pause.
    pub fn unpause_mint(ctx: Context<UnpauseMint>) -> Result<()> {
        crate::instructions::pause::unpause_handler(ctx)
    }

    /// Step 1: Current authority proposes a new authority.
    pub fn propose_authority_transfer(
        ctx: Context<ProposeAuthorityTransfer>,
        new_authority: [u8; 32],
    ) -> Result<()> {
        crate::instructions::authority_transfer::propose_handler(ctx, new_authority)
    }

    /// Step 2: Proposed authority accepts the transfer.
    pub fn accept_authority_transfer(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
        crate::instructions::authority_transfer::accept_handler(ctx)
    }

    /// CPI to DEX::swap with vault PDA as signer. Manager only.
    pub fn vault_swap(
        ctx: Context<VaultSwap>,
        amount_in: u64,
        min_amount_out: u64,
        a_to_b: bool,
    ) -> Result<()> {
        crate::instructions::vault_swap::handler(ctx, amount_in, min_amount_out, a_to_b)
    }
}
