//! Yield Distribution — perpetual merkle-based RWT yield streams with per-claimant vesting.
//!
//! 13 instructions (Layer 8 added `convert_to_rwt` + Layer 9 placeholders),
//! 4 state accounts, 12 events.
//!
//! Built on the Arlex framework (Pinocchio). Classic SPL Token only.
//! See docs/contracts/yield-distribution.mdx for the full specification.
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
pub mod validation;
pub mod merkle;
pub mod vesting;
pub(crate) mod cpi;
pub mod instructions;

// Re-export instruction account structs for the #[program] macro.
use instructions::authority_transfer::{AcceptAuthorityTransfer, ProposeAuthorityTransfer};
use instructions::claim::Claim;
use instructions::close_distributor::CloseDistributor;
use instructions::convert_to_rwt::ConvertToRwt;
use instructions::create_distributor::CreateDistributor;
use instructions::fund_distributor::FundDistributor;
use instructions::initialize_config::InitializeConfig;
use instructions::initialize_liquidity_holding::InitializeLiquidityHolding;
use instructions::publish_root::PublishRoot;
use instructions::update_config::UpdateConfig;
use instructions::update_publish_authority::UpdatePublishAuthority;
use instructions::withdraw_liquidity_holding::WithdrawLiquidityHolding;

declare_id!("YLD9EBikcTmVCnVzdx6vuNajrDkp8tyCAgZrqTwmMXF");

#[program]
pub mod yield_distribution {
    use super::*;

    /// Create the global DistributionConfig PDA (one-time, deployer).
    pub fn initialize_config(
        ctx: Context<InitializeConfig>,
        publish_authority: [u8; 32],
        protocol_fee_bps: u16,
        min_distribution_amount: u64,
    ) -> Result<()> {
        crate::instructions::initialize_config::handler(
            ctx,
            publish_authority,
            protocol_fee_bps,
            min_distribution_amount,
        )
    }

    /// Authority creates a perpetual distributor for an OT project.
    /// Creates MerkleDistributor + Reward Vault RWT ATA + Accumulator + Accumulator USDC ATA.
    pub fn create_distributor(
        ctx: Context<CreateDistributor>,
        vesting_period_secs: i64,
    ) -> Result<()> {
        crate::instructions::create_distributor::handler(ctx, vesting_period_secs)
    }

    /// Permissionless RWT deposit — locks prior vesting then updates totals.
    pub fn fund_distributor(ctx: Context<FundDistributor>, amount: u64) -> Result<()> {
        crate::instructions::fund_distributor::handler(ctx, amount)
    }

    /// Publish authority publishes a new merkle root and bumps epoch.
    pub fn publish_root(
        ctx: Context<PublishRoot>,
        merkle_root: [u8; 32],
        max_total_claim: u64,
    ) -> Result<()> {
        crate::instructions::publish_root::handler(ctx, merkle_root, max_total_claim)
    }

    /// Holder (or PDA) claims vested RWT after proving merkle inclusion.
    pub fn claim(
        ctx: Context<Claim>,
        cumulative_amount: u64,
        proof: alloc::vec::Vec<[u8; 32]>,
    ) -> Result<()> {
        crate::instructions::claim::handler(ctx, cumulative_amount, proof)
    }

    /// Authority closes a distributor, sweeping remaining RWT to unclaimed_destination.
    pub fn close_distributor(ctx: Context<CloseDistributor>) -> Result<()> {
        crate::instructions::close_distributor::handler(ctx)
    }

    /// Authority rotates fee_bps / min_distribution / is_active. Fee destination immutable.
    pub fn update_config(
        ctx: Context<UpdateConfig>,
        protocol_fee_bps: u16,
        min_distribution_amount: u64,
        is_active: bool,
    ) -> Result<()> {
        crate::instructions::update_config::handler(
            ctx,
            protocol_fee_bps,
            min_distribution_amount,
            is_active,
        )
    }

    /// Authority rotates the publish_authority wallet.
    pub fn update_publish_authority(
        ctx: Context<UpdatePublishAuthority>,
        new_publish_authority: [u8; 32],
    ) -> Result<()> {
        crate::instructions::update_publish_authority::handler(ctx, new_publish_authority)
    }

    /// Step 1: current authority proposes a new authority.
    pub fn propose_authority_transfer(
        ctx: Context<ProposeAuthorityTransfer>,
        new_authority: [u8; 32],
    ) -> Result<()> {
        crate::instructions::authority_transfer::propose_handler(ctx, new_authority)
    }

    /// Step 2: proposed authority accepts the transfer.
    pub fn accept_authority_transfer(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
        crate::instructions::authority_transfer::accept_handler(ctx)
    }

    /// Permissionless one-time init of the singleton LiquidityHolding PDA + RWT
    /// ATA (Layer 8 §2.1 / D11.1). Re-running after a successful init reverts.
    pub fn initialize_liquidity_holding(
        ctx: Context<InitializeLiquidityHolding>,
    ) -> Result<()> {
        crate::instructions::initialize_liquidity_holding::handler(ctx)
    }

    /// Layer 9 Nexus drain ix — placeholder for Layer 8: every call reverts
    /// with `NexusNotInitialized` until the Nexus program ID is pinned.
    pub fn withdraw_liquidity_holding(
        ctx: Context<WithdrawLiquidityHolding>,
        amount: u64,
    ) -> Result<()> {
        crate::instructions::withdraw_liquidity_holding::handler(ctx, amount)
    }

    /// Permissionless convert-and-fund: converts per-distributor accumulated
    /// USDC revenue into RWT (DEX swap and/or RWT Engine mint, atomic) and
    /// credits the distributor's reward vault. Layer 8 §5.1.
    pub fn convert_to_rwt(
        ctx: Context<ConvertToRwt>,
        usdc_amount: u64,
        min_rwt_out: u64,
        swap_first: bool,
    ) -> Result<()> {
        crate::instructions::convert_to_rwt::handler(
            ctx,
            usdc_amount,
            min_rwt_out,
            swap_first,
        )
    }
}
