//! Yield Distribution — perpetual merkle-based RWT yield streams with per-claimant vesting.
//!
//! 10 instructions (convert_to_rwt deferred to Layer 8), 4 state accounts, 10 events.
//!
//! Built on the Arlex framework (Pinocchio). Classic SPL Token only.
//! See docs/contracts/yield-distribution.mdx for the full specification.

extern crate alloc;

use arlex_lang::prelude::*;

pub mod constants;
pub mod error;
pub mod events;
pub mod state;
pub mod validation;
pub mod merkle;
pub mod vesting;
pub mod instructions;

// Re-export instruction account structs for the #[program] macro.
use instructions::authority_transfer::{AcceptAuthorityTransfer, ProposeAuthorityTransfer};
use instructions::claim::Claim;
use instructions::close_distributor::CloseDistributor;
use instructions::create_distributor::CreateDistributor;
use instructions::fund_distributor::FundDistributor;
use instructions::initialize_config::InitializeConfig;
use instructions::publish_root::PublishRoot;
use instructions::update_config::UpdateConfig;
use instructions::update_publish_authority::UpdatePublishAuthority;

declare_id!("YDisT7m1epqXqQ9HexkqjqNBv5FauqYksRLfmeTpLbX");

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
}
