// Arlex `#[derive(Accounts)]` proc-macro generates struct-field bindings
// that aren't always read by every handler — those are purely structural.
// `declare_id!` + `#[program]` emit `cfg(feature = "solana")` gates that
// the contract crates don't expose as a feature. Crate-level allow keeps
// the noise out of `cargo build`. Real unused-var bugs in handler logic
// are caught by `clippy --strict`.
#![allow(unused_variables, unexpected_cfgs)]

//! Futarchy — per-OT governance with CPI to Ownership Token contract.
//!
//! V1: Team authority creates/approves proposals. V2: prediction markets.
//! Proposals control OT minting, treasury spending, and revenue distribution.
//!
//! Built on Arlex framework (Pinocchio). CPI to OT contract for execution.
//! See docs/contracts/futarchy.mdx for full specification.
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
pub mod cpi;
pub mod instructions;

use instructions::initialize_futarchy::InitializeFutarchy;
use instructions::create_proposal::CreateProposal;
use instructions::approve_proposal::ApproveProposal;
use instructions::cancel_proposal::CancelProposal;
use instructions::execute_proposal::ExecuteProposal;
use instructions::claim_ot_governance::ClaimOtGovernance;
use instructions::authority_transfer::{ProposeAuthorityTransfer, AcceptAuthorityTransfer};

#[cfg(feature = "devnet")]
declare_id!("25PqXCUXetwG19HunKEYJ1GE3YKBZkCo5KwWK4VdUTEQ");
#[cfg(not(feature = "devnet"))]
declare_id!("FUTsbsdyJmEWa5LSYHWXMr9hQFyVsrJ1agGvRQGR1ARL");

#[program]
pub mod futarchy {
    use super::*;

    /// Create governance for an OT project. Called once per OT.
    pub fn initialize_futarchy(ctx: Context<InitializeFutarchy>) -> Result<()> {
        crate::instructions::initialize_futarchy::handler(ctx)
    }

    /// Create a new governance proposal. V1: authority creates.
    pub fn create_proposal(
        ctx: Context<CreateProposal>,
        proposal_type: u8,
        amount: u64,
        destination: [u8; 32],
        token_mint: [u8; 32],
        params_hash: [u8; 32],
    ) -> Result<()> {
        crate::instructions::create_proposal::handler(
            ctx, proposal_type, amount, destination, token_mint, params_hash,
        )
    }

    /// Approve a proposal. V1: authority signs.
    pub fn approve_proposal(ctx: Context<ApproveProposal>) -> Result<()> {
        crate::instructions::approve_proposal::handler(ctx)
    }

    /// Cancel an active proposal. Authority only.
    pub fn cancel_proposal(ctx: Context<CancelProposal>) -> Result<()> {
        crate::instructions::cancel_proposal::handler(ctx)
    }

    /// Execute an approved proposal via CPI to OT contract. Permissionless.
    pub fn execute_proposal(ctx: Context<ExecuteProposal>) -> Result<()> {
        crate::instructions::execute_proposal::handler(ctx)
    }

    /// Accept OT governance authority on behalf of Futarchy config PDA.
    pub fn claim_ot_governance(ctx: Context<ClaimOtGovernance>) -> Result<()> {
        crate::instructions::claim_ot_governance::handler(ctx)
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
