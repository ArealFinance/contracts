// Crate-level allows: same justification as rwt-engine/native-dex — Arlex
// proc-macros generate field bindings not always read by every handler,
// `declare_id!` / `#[program]` emit `cfg(feature = "solana")` gates that
// the contract crates don't expose.
#![allow(unused_variables, unexpected_cfgs)]

//! # Earn — minimal Book-NAV RWT issuer.
//!
//! Standalone, intentionally small RWT issuer for the `earn.areal.finance`
//! retail product. Not coupled to OT, Futarchy, the native DEX, or
//! yield-distribution — those live in their own programs.
//!
//! ## Economic model
//!
//! Users mint RWT at **Book NAV × (1 + 1%)**:
//!   - **body** (100% at Book NAV) → `basket_vault`, bumps `total_invested_capital`.
//!   - **1% commission** → `dao_fee_destination` (Areal Finance revenue).
//!
//! The mint is value-neutral: supply and capital grow synchronously, so the
//! Book NAV Price is never diluted by minting (**mint invariant**). The mint
//! fee does NOT grow NAV — it is pure DAO revenue, excluded from capital.
//!
//! NAV is derived, not stored:
//!   ```text
//!   Book NAV Price = total_invested_capital × NAV_SCALE / total_rwt_supply
//!   ```
//! with INITIAL_NAV = $1.00 guard when supply == 0.
//!
//! `total_invested_capital` grows via `add_to_basket` (income reinvestment /
//! appreciation → NAV ↑) and shrinks via `writedown_capital` (depreciation →
//! NAV ↓). There is NO redeem — exit is via the secondary market (DEX).
//!
//! ## Built on Arlex (Pinocchio)
//!
//! Same framework as the other Areal programs. Uses classic SPL Token
//! (`TokenkegQ…`), which on mainnet is now the p-token implementation.
//!
//! See `docs/contracts/earn.mdx` for the canonical specification and
//! `docs/architecture/earn-layers.mdx` for the on-chain / off-chain boundary.

extern crate alloc;

use arlex_lang::prelude::*;

pub mod constants;
pub mod error;
pub mod events;
pub mod state;
pub mod nav;
pub mod validation;
pub mod instructions;

use instructions::initialize::Initialize;
use instructions::mint_rwt::MintRwt;
use instructions::add_to_basket::AddToBasket;
use instructions::writedown_capital::WritedownCapital;
use instructions::pause::{PauseEarn, UnpauseEarn};
use instructions::update_config::UpdateConfig;
use instructions::authority_transfer::{ProposeAuthorityTransfer, AcceptAuthorityTransfer};

// Program ID. Devnet uses the keypair at keys/devnet/earn.json; the
// non-devnet (Testnet/mainnet) ID is a placeholder pending a vanity grind.
#[cfg(feature = "devnet")]
declare_id!("HMBZu87F9zTt4JGbQwaL5V6tFXdLBUyLtgeYTsVh1Rzu");
#[cfg(not(feature = "devnet"))]
declare_id!("11111111111111111111111111111111");

#[program]
pub mod earn {
    use super::*;

    /// One-time bootstrap. Creates EarnConfig PDA, pins mints / basket_vault /
    /// dao_fee_destination / pause_authority. Initial NAV is implicit ($1.00).
    pub fn initialize(
        ctx: Context<Initialize>,
        authority: [u8; 32],
        pause_authority: [u8; 32],
    ) -> Result<()> {
        crate::instructions::initialize::handler(ctx, authority, pause_authority)
    }

    /// User deposits USDC at Book NAV (+1% fee), receives earn-RWT.
    /// `min_rwt_out`: slippage protection (revert if output < minimum).
    pub fn mint_rwt(
        ctx: Context<MintRwt>,
        usdc_amount: u64,
        min_rwt_out: u64,
    ) -> Result<()> {
        crate::instructions::mint_rwt::handler(ctx, usdc_amount, min_rwt_out)
    }

    /// Authority / off-chain executor reinvests income into the basket.
    /// Bumps `total_invested_capital` without minting RWT → NAV rises.
    pub fn add_to_basket(ctx: Context<AddToBasket>, amount: u64) -> Result<()> {
        crate::instructions::add_to_basket::handler(ctx, amount)
    }

    /// Authority-only capital writedown. Decreases NAV to reflect real-world
    /// depreciation / amortization of underlying RWA. No token move.
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

    /// Authority-only admin tuning of mint fee, min mint amount, and the
    /// DAO fee destination.
    pub fn update_config(
        ctx: Context<UpdateConfig>,
        mint_fee_bps: u16,
        min_mint_amount: u64,
        dao_fee_destination: [u8; 32],
    ) -> Result<()> {
        crate::instructions::update_config::handler(
            ctx, mint_fee_bps, min_mint_amount, dao_fee_destination,
        )
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
}
