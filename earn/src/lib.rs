// Crate-level allows: same justification as rwt-engine/native-dex — Arlex
// proc-macros generate field bindings not always read by every handler,
// `declare_id!` / `#[program]` emit `cfg(feature = "solana")` gates that
// the contract crates don't expose.
#![allow(unused_variables, unexpected_cfgs)]

//! # Earn — minimal NAV-bearing token program.
//!
//! Standalone, intentionally small (~6 instructions) RWT issuer for the
//! `earn.areal.finance` retail product. Not coupled to OT, Futarchy, the
//! native DEX, or yield-distribution — those live in their own programs.
//!
//! ## Economic model
//!
//! Users mint earn-RWT by depositing USDC. The deposit splits three ways:
//!   - **RWA wallet** (~90%) — operator-controlled USDC ATA. Off-chain
//!     buys underlying real-world assets.
//!   - **Liquidity wallet** (~8%) — program-PDA-owned USDC ATA. Counts in
//!     NAV; reserved for future LP / buyback mechanisms.
//!   - **ARL Treasury wallet** (~2%) — Areal revenue. NOT in NAV.
//!
//! RWT minted to user = `rwa_amount × NAV_SCALE / NAV` — i.e. only the
//! RWA share converts to RWT. The Liquidity share boosts NAV for ALL
//! holders (existing + new). The Treasury share is pure protocol revenue.
//!
//! NAV is derived, not stored:
//!   ```text
//!   NAV = total_invested_capital × NAV_SCALE / total_rwt_supply
//!   ```
//! with INITIAL_NAV = $1.00 guard when supply == 0.
//!
//! ## Exit
//!
//! There is NO redeem instruction. Users exit by selling RWT on the
//! native DEX secondary market (a separate earn-RWT/USDC pool must be
//! seeded operationally before sell-side opens — V1 launches mint-only).
//!
//! ## Built on Arlex (Pinocchio)
//!
//! Same framework as the other Areal programs. Uses classic SPL Token
//! (`TokenkegQ…`) which on mainnet is now the p-token implementation.
//!
//! See `docs/contracts/earn.mdx` for the canonical specification.

extern crate alloc;

use arlex_lang::prelude::*;

pub mod constants;
pub mod error;
pub mod events;
pub mod state;
pub mod instructions;

use instructions::initialize::Initialize;
use instructions::mint_rwt::MintRwt;
use instructions::stream_inflow::StreamInflow;
use instructions::writedown_capital::WritedownCapital;
use instructions::pause::{PauseEarn, UnpauseEarn};
use instructions::update_config::UpdateConfig;

// TODO(deploy): replace with a vanity keypair (e.g. `Earnxxxxxxxx...ARL`)
// before mainnet. SystemProgram address used here as a placeholder so
// declare_id! parses; this MUST NOT remain at deploy time.
declare_id!("11111111111111111111111111111111");

#[program]
pub mod earn {
    use super::*;

    /// One-time bootstrap. Creates EarnConfig PDA, pins wallets/mints,
    /// authority and pause-authority. Initial NAV is implicit ($1.00).
    pub fn initialize(
        ctx: Context<Initialize>,
        pause_authority: [u8; 32],
    ) -> Result<()> {
        crate::instructions::initialize::handler(ctx, pause_authority)
    }

    /// User deposits USDC and receives earn-RWT at current NAV.
    /// `min_rwt_out`: slippage protection (revert if output < minimum).
    pub fn mint_rwt(
        ctx: Context<MintRwt>,
        usdc_amount: u64,
        min_rwt_out: u64,
    ) -> Result<()> {
        crate::instructions::mint_rwt::handler(ctx, usdc_amount, min_rwt_out)
    }

    /// Authority-only USDC top-up to liquidity_wallet. Bumps NAV without
    /// minting RWT. Used to channel off-chain RWA revenue back into NAV.
    pub fn stream_inflow(ctx: Context<StreamInflow>, amount: u64) -> Result<()> {
        crate::instructions::stream_inflow::handler(ctx, amount)
    }

    /// Authority-only capital writedown. Decreases NAV to reflect
    /// real-world depreciation / amortization of underlying RWA.
    pub fn writedown_capital(
        ctx: Context<WritedownCapital>,
        amount: u64,
        reason_code: u8,
    ) -> Result<()> {
        crate::instructions::writedown_capital::handler(ctx, amount, reason_code)
    }

    /// Pause mint flow (emergency stop). Pause-authority-only.
    pub fn pause(ctx: Context<PauseEarn>) -> Result<()> {
        crate::instructions::pause::pause_handler(ctx)
    }

    /// Resume mint flow. Pause-authority-only.
    pub fn unpause(ctx: Context<UnpauseEarn>) -> Result<()> {
        crate::instructions::pause::unpause_handler(ctx)
    }

    /// Authority-only admin tuning of split bps + min_mint_amount.
    pub fn update_config(
        ctx: Context<UpdateConfig>,
        split_rwa_bps: u16,
        split_liquidity_bps: u16,
        split_treasury_bps: u16,
        min_mint_amount: u64,
    ) -> Result<()> {
        crate::instructions::update_config::handler(
            ctx,
            split_rwa_bps,
            split_liquidity_bps,
            split_treasury_bps,
            min_mint_amount,
        )
    }
}
