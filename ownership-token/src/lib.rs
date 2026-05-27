// Arlex `#[derive(Accounts)]` proc-macro generates struct-field bindings
// that aren't always read by every handler — those are purely structural.
// `declare_id!` + `#[program]` emit `cfg(feature = "solana")` gates that
// the contract crates don't expose as a feature. Crate-level allow keeps
// the noise out of `cargo build`. Real unused-var bugs in handler logic
// are caught by `clippy --strict`.
#![allow(unused_variables, unexpected_cfgs)]

//! Ownership Token (OT) — per-project tokenized ownership with revenue distribution.
//!
//! Each real-world asset project gets its own OT mint. This contract handles:
//! - Token minting (governance-controlled)
//! - Revenue collection (USDC deposits to Revenue ATA)
//! - Automatic revenue distribution (permissionless crank)
//! - Treasury management (governance-controlled spending)
//! - Two-step authority transfer
//!
//! Built on Arlex framework (Pinocchio). Classic SPL Token only (no Token-2022).
//! See docs/contracts/ownership-token.mdx for full specification.
//!
//! # Unsafe (L-5 audit note)
//!
//! `unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) }`
//! blocks throughout this crate are the standard Pinocchio zero-copy pattern.
//! `AccountView` exposes the BPF-loader-provided account region as-is;
//! constructing a slice from it is sound as long as `data_len()` bounds are
//! respected. Every such usage is followed by an explicit length check before
//! any indexing.

extern crate alloc;

use arlex_lang::prelude::*;

pub mod constants;
pub mod error;
pub mod events;
pub mod state;
pub(crate) mod cpi;
pub mod instructions;

// Re-export instruction account structs for #[program] macro access
use instructions::initialize_ot::InitializeOt;
use instructions::mint_ot::MintOt;
use instructions::distribute_revenue::DistributeRevenue;
use instructions::spend_treasury::SpendTreasury;
use instructions::batch_update_destinations::BatchUpdateDestinations;
use instructions::authority_transfer::{ProposeAuthorityTransfer, AcceptAuthorityTransfer};
use instructions::claim_yd_for_treasury::ClaimYdForTreasury;
use state::BatchDestination;

#[cfg(feature = "devnet")]
declare_id!("Eu7hc5gdjVHGff51bQHEDF1fSsWVRvUE15vBsFwu2oaX");
#[cfg(not(feature = "devnet"))]
declare_id!("oWnqbNwmEdjNS5KVbxz8xeuGNjKMd1aiNF89d7qdARL");

#[program]
pub mod ownership_token {
    use super::*;

    /// Register an existing SPL mint as an Ownership Token.
    /// Creates 5 PDAs + Revenue USDC ATA + transfers mint authority to OtConfig PDA.
    pub fn initialize_ot(
        ctx: Context<InitializeOt>,
        name: [u8; 32],
        symbol: [u8; 10],
        uri: [u8; 200],
        initial_authority: [u8; 32],
    ) -> Result<()> {
        crate::instructions::initialize_ot::handler(
            ctx, name, symbol, uri, initial_authority,
        )
    }

    /// Mint OT tokens to any recipient wallet.
    pub fn mint_ot(ctx: Context<MintOt>, amount: u64) -> Result<()> {
        crate::instructions::mint_ot::handler(ctx, amount)
    }

    /// Distribute accumulated USDC revenue to configured destinations.
    /// Permissionless — any wallet can trigger.
    pub fn distribute_revenue(ctx: Context<DistributeRevenue>) -> Result<()> {
        crate::instructions::distribute_revenue::handler(ctx)
    }

    /// Transfer tokens from the treasury to any destination. Governance only.
    pub fn spend_treasury(ctx: Context<SpendTreasury>, amount: u64) -> Result<()> {
        crate::instructions::spend_treasury::handler(ctx, amount)
    }

    /// Atomically replace the entire revenue destination configuration. Governance only.
    pub fn batch_update_destinations(
        ctx: Context<BatchUpdateDestinations>,
        destinations: alloc::vec::Vec<BatchDestination>,
    ) -> Result<()> {
        crate::instructions::batch_update_destinations::handler(ctx, destinations)
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

    /// OtTreasury PDA claims vested RWT from a Yield Distribution distributor.
    /// Permissionless — any wallet can act as crank (pays ClaimStatus rent).
    /// Cross-project yield supported (claim from a different OT's distributor).
    /// Layer 8 §5.4.
    pub fn claim_yd_for_treasury(
        ctx: Context<ClaimYdForTreasury>,
        cumulative_amount: u64,
        proof: alloc::vec::Vec<[u8; 32]>,
    ) -> Result<()> {
        crate::instructions::claim_yd_for_treasury::handler(ctx, cumulative_amount, proof)
    }
}
