// Crate-level allows: same justification as earn / rwt-engine / native-dex —
// Arlex proc-macros generate field bindings not always read by every handler,
// and `declare_id!` / `#[program]` emit `cfg(feature = "solana")` gates that
// the contract crates don't expose.
#![allow(unused_variables, unexpected_cfgs)]

//! # Staking — stRWT share pool.
//!
//! Users stake **RWT** and receive **stRWT** (a share token). Rewards are
//! delivered not as payouts but through a rising `stRWT → RWT` exchange rate.
//!
//! Deliberately dumb (per the Areal earn-layer separation principle): this
//! contract accepts ready RWT rewards via `deposit_rewards` and grows the
//! rate. It knows nothing about prices, oracles, or where reward RWT came
//! from (buyback vs mint). A bug in the off-chain reward-sourcing logic
//! cannot compromise funds here.
//!
//! ## Rate model (single virtual-offset, OpenZeppelin ERC4626 style)
//!   strwt_out = rwt_in × (strwt_supply + V_SHARES) / (active + V_ASSETS)
//!   rwt_out   = strwt × (active + V_ASSETS) / (strwt_supply + V_SHARES)
//! No stored `initial_rate`: `V_ASSETS / V_SHARES = 10` is the empty-pool
//! bootstrap rate. `total_rwt_active` is an explicit counter (NOT the raw
//! vault balance) — a bare token transfer into the vault cannot move the rate.
//!
//! ## Cooldown
//! Unstake is two-step: `initiate_unstake` burns stRWT and moves the fixed RWT
//! amount active → reserved with a 21-day unlock; `complete_unstake` claims it
//! after the unlock and closes the ticket (rent → owner).
//!
//! ## Built on Arlex (Pinocchio), classic SPL Token.
//!
//! See `docs/contracts/staking.mdx` for the canonical specification.

extern crate alloc;

use arlex_lang::prelude::*;

pub mod constants;
pub mod error;
pub mod events;
pub mod instructions;
pub mod rate;
pub mod state;
pub mod token;

use instructions::authority_transfer::{AcceptAuthorityTransfer, ProposeAuthorityTransfer};
use instructions::complete_unstake::CompleteUnstake;
use instructions::create_strwt_metadata::CreateStrwtMetadata;
use instructions::deposit_rewards::DepositRewards;
use instructions::initialize::Initialize;
use instructions::initiate_unstake::InitiateUnstake;
use instructions::stake::Stake;
use instructions::update_config::UpdateConfig;

// Program ID. Devnet uses the keypair at keys/devnet/staking-v2.json; the
// non-devnet (mainnet) ID is the vanity key at keys/mainnet/staking-program.json.
#[cfg(feature = "devnet")]
declare_id!("CmKXHk3u6pDUC6Q11Le6gmhCgENQSFvduisXb7guUGoL");
#[cfg(not(feature = "devnet"))]
declare_id!("9tEKvDwkqkveBvmQfEzgPKWSNCDTGSSqYz4ZE6pP5DGY");

#[program]
pub mod staking {
    use super::*;

    /// One-time bootstrap. Creates StakingConfig PDA, the stRWT mint (auth =
    /// config PDA), the RWT pool vault (config-PDA-owned RWT ATA), and pins the
    /// reward depositor.
    pub fn initialize(ctx: Context<Initialize>, reward_depositor: [u8; 32]) -> Result<()> {
        crate::instructions::initialize::handler(ctx, reward_depositor)
    }

    /// Deposit RWT, mint stRWT at the current rate. `min_strwt_out` is
    /// slippage protection.
    pub fn stake(ctx: Context<Stake>, rwt_amount: u64, min_strwt_out: u64) -> Result<()> {
        crate::instructions::stake::handler(ctx, rwt_amount, min_strwt_out)
    }

    /// reward_depositor-only RWT inflow. Raises the rate; mints no stRWT.
    pub fn deposit_rewards(ctx: Context<DepositRewards>, rwt_amount: u64) -> Result<()> {
        crate::instructions::deposit_rewards::handler(ctx, rwt_amount)
    }

    /// Burn stRWT, fix RWT at the current rate, start the 21-day cooldown.
    /// `nonce` is client-supplied; reusing a live nonce fails ticket creation.
    pub fn initiate_unstake(
        ctx: Context<InitiateUnstake>,
        strwt_amount: u64,
        nonce: u64,
    ) -> Result<()> {
        crate::instructions::initiate_unstake::handler(ctx, strwt_amount, nonce)
    }

    /// After the cooldown elapses, claim the reserved RWT and close the ticket.
    pub fn complete_unstake(ctx: Context<CompleteUnstake>, nonce: u64) -> Result<()> {
        crate::instructions::complete_unstake::handler(ctx, nonce)
    }

    /// Authority-only tuning of reward_depositor / min_stake_amount / cooldown.
    pub fn update_config(
        ctx: Context<UpdateConfig>,
        reward_depositor: [u8; 32],
        min_stake_amount: u64,
        cooldown_seconds: i64,
    ) -> Result<()> {
        crate::instructions::update_config::handler(
            ctx,
            reward_depositor,
            min_stake_amount,
            cooldown_seconds,
        )
    }

    /// Step 1 of 2-step authority rotation. Authority-only.
    pub fn propose_authority_transfer(
        ctx: Context<ProposeAuthorityTransfer>,
        new_authority: [u8; 32],
    ) -> Result<()> {
        crate::instructions::authority_transfer::propose_handler(ctx, new_authority)
    }

    /// Step 2 of 2-step authority rotation. Pending-authority-only.
    pub fn accept_authority_transfer(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
        crate::instructions::authority_transfer::accept_handler(ctx)
    }

    /// Bootstrap-gated: create the Metaplex Token Metadata account for the
    /// stRWT (Staked RWT) share mint (name / symbol / uri) so wallets display a
    /// proper token instead of a nameless SPL Token. The StakingConfig PDA signs
    /// the Metaplex CPI as mint_authority; the metadata update_authority is set
    /// to `config.authority` (the multisig vault). Available on BOTH builds.
    pub fn create_strwt_metadata(
        ctx: Context<CreateStrwtMetadata>,
        name: [u8; 32],
        name_len: u8,
        symbol: [u8; 10],
        symbol_len: u8,
        uri: [u8; 200],
        uri_len: u8,
    ) -> Result<()> {
        crate::instructions::create_strwt_metadata::handler(
            ctx, name, name_len, symbol, symbol_len, uri, uri_len,
        )
    }
}
