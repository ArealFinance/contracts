//! CPI builders for Ownership Token → Yield Distribution.
//!
//! Layer 8 Step 1 — interface stubs only. The body is filled in Step 2
//! (`claim_yd_for_treasury`). The OtTreasury PDA signs the YD::claim CPI
//! as the claimant; the crank acts as payer. Claimed RWT lands in the OT
//! treasury RWT ATA.
//!
//! All cross-program calls use raw `pinocchio::cpi::invoke_signed` with
//! hardcoded discriminators. Account order MUST match the target
//! instruction's `#[derive(Accounts)]` field order.
//!
//! Source of truth for CPI layout:
//!   - YD::claim — contracts/yield-distribution/src/instructions/claim.rs
//!
//! Discriminator is pinned in `crate::constants::DISC_YD_CLAIM` and verified
//! against `sha256("global:claim")[..8]` by the `tests` module below (R7).

extern crate alloc;

use arlex_lang::prelude::*;
use pinocchio::cpi::Seed;

use crate::constants::*;

/// CPI → `yield_distribution::claim` signed by the OtTreasury PDA.
///
/// Account order MUST match
/// `contracts/yield-distribution/src/instructions/claim.rs` (10 accounts):
///   0. claimant         (signer via PDA)  — `["ot_treasury", ot_mint]`
///   1. payer            (signer, mut)     — crank wallet
///   2. config           (read)            — `["dist_config"]`
///   3. ot_mint          (read)
///   4. distributor      (mut)             — `["merkle_dist", ot_mint]`
///   5. claim_status     (mut)             — `["claim_status", distributor, claimant]`
///   6. reward_vault     (mut)             — distributor's RWT ATA
///   7. claimant_token   (mut)             — OtTreasury's RWT ATA
///   8. token_program
///   9. system_program
///
/// Discriminator: `DISC_YD_CLAIM`.
/// Instruction data: `[DISC_YD_CLAIM(8), cumulative_amount(8), proof_len(4), proof_bytes(32*N)]`.
///
/// See `Layer 8 architecture §3.2` and `§5.4.3`.
// TODO(R9): hoist to Arlex framework helper — duplicated across RWT/DEX/OT cpi modules.
#[allow(clippy::too_many_arguments, dead_code)] // dead_code: stub, wired up in Step 2
pub fn cpi_yd_claim<'a>(
    _claimant: &'a AccountView,
    _payer: &'a AccountView,
    _yd_config: &'a AccountView,
    _ot_mint: &'a AccountView,
    _yd_distributor: &'a AccountView,
    _yd_claim_status: &'a AccountView,
    _yd_reward_vault: &'a AccountView,
    _claimant_token: &'a AccountView,
    _token_program: &'a AccountView,
    _system_program: &'a AccountView,
    _yd_program: &'a AccountView,
    _claimant_seeds: &[Seed],
    _cumulative_amount: u64,
    _proof: &[[u8; 32]],
) -> ProgramResult {
    // TODO(Layer 8 Step 2): build instruction data + accounts array,
    // then invoke_signed::<11> with the OtTreasury PDA seeds
    // (`["ot_treasury", ot_mint]`).
    // See `Layer 8 architecture §3.2` and `§5.4.3`.
    unimplemented!("cpi_yd_claim is a Layer 8 Step 1 stub; implement in Step 2")
}

#[cfg(test)]
mod tests {
    //! Discriminator + program-ID parity tests (R7).

    use super::*;
    use sha2::{Digest, Sha256};

    fn disc(name: &str) -> [u8; 8] {
        let mut h = Sha256::new();
        h.update(name.as_bytes());
        let out = h.finalize();
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&out[..8]);
        buf
    }

    #[test]
    fn disc_yd_claim_matches_sha256() {
        assert_eq!(
            DISC_YD_CLAIM,
            disc("global:claim"),
            "DISC_YD_CLAIM out of sync with sha256(\"global:claim\")[..8]"
        );
    }

    #[test]
    fn yd_program_id_matches_vanity() {
        let encoded = bs58::encode(&YD_PROGRAM_ID).into_string();
        assert_eq!(
            encoded, "YLD9EBikcTmVCnVzdx6vuNajrDkp8tyCAgZrqTwmMXF",
            "YD_PROGRAM_ID bytes drifted from canonical vanity address"
        );
    }
}
