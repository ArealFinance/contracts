//! withdraw_liquidity_holding — Layer 9 Nexus drain ix (PLACEHOLDER for Layer 8).
//!
//! Layer 8 §2.1 + decisions D4 / D11.1 / R10. Reserves the discriminator
//! (`DISC_YD_WITHDRAW_LIQUIDITY_HOLDING`) and account layout so that Layer 9
//! Nexus can drop in real CPI logic without breaking the wire interface or
//! requiring a YD program upgrade ABI bump.
//!
//! Until the Layer 9 Nexus program is deployed AND
//! `NEXUS_PROGRAM_ID_PLACEHOLDER` is replaced with the canonical program ID,
//! every call reverts with `NexusNotInitialized`. The LiquidityHolding RWT ATA
//! is therefore a one-way sink: `claim_yield` can fund it, nothing can drain
//! it. This is the anti-honeypot guarantee per D4 (a compromised crank
//! keypair cannot extract the 15% liquidity-share parked here).
//!
//! # Layer 9 implementation notes (do NOT implement here)
//!
//! When Layer 9 lands the real ix:
//!   1. Replace `NEXUS_PROGRAM_ID_PLACEHOLDER` in `constants.rs` with the
//!      vanity address bytes.
//!   2. Replace the unconditional revert below with a CPI signer check
//!      (`nexus_authority` PDA derived under the Nexus program ID).
//!   3. Issue a PDA-signed Transfer from `liquidity_holding_ata` →
//!      `recipient_ata` and update `total_withdrawn` + emit
//!      `LiquidityHoldingWithdrawn`.

use arlex_lang::prelude::*;

use crate::constants::*;
use crate::error::YdError;

#[derive(Accounts)]
pub struct WithdrawLiquidityHolding<'info> {
    /// LiquidityHolding PDA singleton — owns the RWT ATA being drained.
    /// Marked `mut` because Layer 9 will bump `total_withdrawn` here.
    #[account(mut, seeds = [b"liq_holding"], bump)]
    pub liquidity_holding: &'info AccountView,

    /// RWT ATA owned by `liquidity_holding`. Layer 9 will Transfer from here.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub liquidity_holding_ata: &'info AccountView,

    /// Layer 9 Nexus authority signer — must match the Nexus program's
    /// canonical PDA. Until Layer 9 is deployed this signer cannot be
    /// satisfied by any real principal (the program ID is all-zeros).
    #[account(signer)]
    pub nexus_authority: &'info AccountView,

    /// Destination RWT ATA — Layer 9 picks (e.g. AMM LP wallet).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub recipient_ata: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(_ctx: Context<WithdrawLiquidityHolding>, _amount: u64) -> Result<()> {
    // Defence-in-depth: verify the placeholder has not been overwritten with
    // a non-zero program ID without the rest of the Layer 9 logic landing.
    // While the placeholder is intact, refuse unconditionally.
    if NEXUS_PROGRAM_ID_PLACEHOLDER == [0u8; 32] {
        return Err(ProgramError::from(YdError::NexusNotInitialized));
    }

    // Once the placeholder is replaced (Layer 9), the gate above falls through
    // and the real implementation must land here. Until then, a stale build
    // with a non-zero placeholder but no implementation should still revert.
    Err(ProgramError::from(YdError::NexusNotInitialized))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Layer 8 invariant: the Nexus program ID is unset. Triggers the
    /// unconditional revert path.
    #[test]
    fn nexus_program_id_placeholder_is_zero_for_layer_8() {
        assert_eq!(NEXUS_PROGRAM_ID_PLACEHOLDER, [0u8; 32]);
    }

    /// Layer 8 acceptance: while the placeholder is intact, the handler must
    /// fail with `NexusNotInitialized`. We exercise the gate logic directly
    /// (the real entrypoint requires a Context which is harness-only).
    #[test]
    fn placeholder_gate_returns_nexus_not_initialized() {
        let err: Result<()> = if NEXUS_PROGRAM_ID_PLACEHOLDER == [0u8; 32] {
            Err(ProgramError::from(YdError::NexusNotInitialized))
        } else {
            unreachable!("placeholder should be all zeros for Layer 8")
        };
        match err {
            Err(e) => {
                let expected: ProgramError = YdError::NexusNotInitialized.into();
                assert_eq!(format!("{:?}", e), format!("{:?}", expected));
            }
            Ok(_) => panic!("expected NexusNotInitialized revert"),
        }
    }
}
