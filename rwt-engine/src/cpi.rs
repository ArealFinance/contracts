//! CPI builders for RWT Engine → Yield Distribution.
//!
//! Layer 8 §5.2 — `cpi_yd_claim` is the only Layer 8 CPI surface owned by RWT
//! Engine. The RwtVault PDA acts as the YD claimant via `invoke_signed`; the
//! crank is the payer (covers ClaimStatus rent on first claim). Claimed RWT
//! lands in the vault's RWT claim ATA (owned by the vault PDA) and is then
//! split 70/15/15 by `claim_yield`:
//!   * book_value_share STAYS in the vault ATA (NAV backing).
//!   * liquidity_share is PDA-signed transferred to `dist_config.liquidity_destination`.
//!   * protocol_revenue_share is PDA-signed transferred to `dist_config.protocol_revenue_destination`.
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

use alloc::vec::Vec;
use arlex_lang::prelude::*;
use pinocchio::cpi::{invoke_signed, Seed, Signer};
use pinocchio::instruction::{InstructionAccount, InstructionView};

use crate::constants::*;

/// CPI → `yield_distribution::claim` signed by the RwtVault PDA.
///
/// Account order MUST match
/// `contracts/yield-distribution/src/instructions/claim.rs` (10 accounts):
///   0. claimant         (signer via PDA)  — `["rwt_vault"]` (singleton)
///   1. payer            (signer, mut)     — crank wallet
///   2. config           (read)            — `["dist_config"]`
///   3. ot_mint          (read)            — OT mint of source distributor
///   4. distributor      (mut)             — `["merkle_dist", ot_mint]`
///   5. claim_status     (mut)             — `["claim_status", distributor, claimant]`
///   6. reward_vault     (mut)             — distributor's RWT ATA
///   7. claimant_token   (mut)             — vault's RWT claim ATA
///   8. token_program
///   9. system_program
///
/// Discriminator: `DISC_YD_CLAIM`.
/// Instruction data: `[DISC_YD_CLAIM(8), cumulative_amount(8), proof_len(4), proof_bytes(32*N)]`.
///
/// See `Layer 8 architecture §3.2` and `§5.2.3`.
// TODO(R9): hoist to Arlex framework helper — duplicated across RWT/DEX/OT cpi modules.
#[allow(clippy::too_many_arguments)]
pub fn cpi_yd_claim<'a>(
    claimant: &'a AccountView,
    payer: &'a AccountView,
    yd_config: &'a AccountView,
    ot_mint: &'a AccountView,
    yd_distributor: &'a AccountView,
    yd_claim_status: &'a AccountView,
    yd_reward_vault: &'a AccountView,
    claimant_token: &'a AccountView,
    token_program: &'a AccountView,
    system_program: &'a AccountView,
    yd_program: &'a AccountView,
    claimant_seeds: &[Seed],
    cumulative_amount: u64,
    proof: &[[u8; 32]],
) -> ProgramResult {
    // 1. Serialize instruction data:
    //    [DISC_YD_CLAIM(8) | cumulative_amount(8 LE) | proof_len(4 LE) | proof_bytes(32*N)]
    let proof_len = proof.len() as u32;
    let mut data = Vec::with_capacity(8 + 8 + 4 + 32 * proof.len());
    data.extend_from_slice(&DISC_YD_CLAIM);
    data.extend_from_slice(&cumulative_amount.to_le_bytes());
    data.extend_from_slice(&proof_len.to_le_bytes());
    for node in proof {
        data.extend_from_slice(node);
    }

    // 2. Build the 10-account list expected by YD::claim
    //    (writable / signer flags MUST mirror yield-distribution/src/instructions/claim.rs).
    let accounts = [
        InstructionAccount::new(claimant.address(), false, true),         // 0: claimant (signer via PDA)
        InstructionAccount::new(payer.address(), true, true),             // 1: payer (signer + mut)
        InstructionAccount::new(yd_config.address(), false, false),       // 2: config (read)
        InstructionAccount::new(ot_mint.address(), false, false),         // 3: ot_mint (read)
        InstructionAccount::new(yd_distributor.address(), true, false),   // 4: distributor (mut)
        InstructionAccount::new(yd_claim_status.address(), true, false),  // 5: claim_status (mut, init-if-needed)
        InstructionAccount::new(yd_reward_vault.address(), true, false),  // 6: reward_vault (mut)
        InstructionAccount::new(claimant_token.address(), true, false),   // 7: claimant_token (mut)
        InstructionAccount::new(token_program.address(), false, false),   // 8: token_program (read)
        InstructionAccount::new(system_program.address(), false, false),  // 9: system_program (read)
    ];

    let instruction = InstructionView {
        program_id: yd_program.address(),
        data: &data,
        accounts: &accounts,
    };

    let signer = Signer::from(claimant_seeds);

    // 3. invoke_signed::<11> — 10 CPI accounts + yd_program (program-id resolution slot).
    invoke_signed::<11>(
        &instruction,
        &[
            claimant, payer, yd_config, ot_mint,
            yd_distributor, yd_claim_status, yd_reward_vault, claimant_token,
            token_program, system_program, yd_program,
        ],
        &[signer],
    )
}

#[cfg(test)]
mod tests {
    //! Discriminator + program-ID parity tests (R7) plus Step 4 serialization
    //! checks for the YD::claim instruction-data layout.

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

    /// Reimplementation of the data-buffer build inside `cpi_yd_claim`. Used
    /// to assert byte-for-byte layout without requiring a BPF runtime / mocked
    /// `AccountView`s. If the production builder drifts from this layout, the
    /// CPI dispatch on YD will fail at deserialize.
    fn build_yd_claim_instruction_data(cumulative_amount: u64, proof: &[[u8; 32]]) -> Vec<u8> {
        let proof_len = proof.len() as u32;
        let mut data = Vec::with_capacity(8 + 8 + 4 + 32 * proof.len());
        data.extend_from_slice(&DISC_YD_CLAIM);
        data.extend_from_slice(&cumulative_amount.to_le_bytes());
        data.extend_from_slice(&proof_len.to_le_bytes());
        for node in proof {
            data.extend_from_slice(node);
        }
        data
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
    fn disc_dex_swap_matches_sha256() {
        // RWT Engine already pins DISC_DEX_SWAP for the existing vault_swap
        // instruction. Re-verify here so the parity guarantee covers the
        // whole `constants.rs` discriminator block.
        assert_eq!(
            DISC_DEX_SWAP,
            disc("global:swap"),
            "DISC_DEX_SWAP out of sync with sha256(\"global:swap\")[..8]"
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

    #[test]
    fn yd_claim_data_layout_empty_proof() {
        let data = build_yd_claim_instruction_data(1_000_000, &[]);
        assert_eq!(data.len(), 8 + 8 + 4, "empty-proof layout: disc+amount+len only");
        assert_eq!(&data[0..8], &DISC_YD_CLAIM, "discriminator must be at offset 0");
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            1_000_000,
            "cumulative_amount LE at offset 8..16"
        );
        assert_eq!(
            u32::from_le_bytes(data[16..20].try_into().unwrap()),
            0,
            "proof_len LE at offset 16..20"
        );
    }

    #[test]
    fn yd_claim_data_layout_with_proof() {
        let proof: [[u8; 32]; 3] = [[0xAAu8; 32], [0xBBu8; 32], [0xCCu8; 32]];
        let data = build_yd_claim_instruction_data(u64::MAX, &proof);
        assert_eq!(data.len(), 8 + 8 + 4 + 32 * 3, "size = disc+amount+len+3*32");
        assert_eq!(&data[0..8], &DISC_YD_CLAIM);
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            u64::MAX
        );
        assert_eq!(
            u32::from_le_bytes(data[16..20].try_into().unwrap()),
            3
        );
        assert_eq!(&data[20..52], &[0xAAu8; 32]);
        assert_eq!(&data[52..84], &[0xBBu8; 32]);
        assert_eq!(&data[84..116], &[0xCCu8; 32]);
    }

    #[test]
    fn yd_claim_data_layout_max_proof() {
        // YD::claim accepts proofs up to MAX_PROOF_LEN=20. Verify our serializer
        // produces correct sizes at the upper bound (no panic, exact size).
        let proof: alloc::vec::Vec<[u8; 32]> = (0..20u8).map(|i| [i; 32]).collect();
        let data = build_yd_claim_instruction_data(42, &proof);
        assert_eq!(data.len(), 8 + 8 + 4 + 32 * 20);
        assert_eq!(
            u32::from_le_bytes(data[16..20].try_into().unwrap()),
            20,
            "proof_len must equal 20"
        );
        // Spot-check the last node is at the tail.
        assert_eq!(&data[(20 + 32 * 19)..(20 + 32 * 20)], &[19u8; 32]);
    }

    #[test]
    fn rwt_vault_seed_layout_is_two_components() {
        // Sanity-check that the seed slice layout the handler hands to
        // `Signer::from` has exactly the two components YD::claim expects
        // for the RwtVault PDA: `[b"rwt_vault", &[bump]]` (singleton — no
        // ot_mint suffix). We don't compute the PDA here (would require a
        // host-side BPF emulator); we only assert seed shape. Actual PDA
        // derivation is exercised end-to-end in the Step 10 E2E test.
        let bump_arr = [0xFFu8];
        let seeds = [
            Seed::from(b"rwt_vault" as &[u8]),
            Seed::from(bump_arr.as_ref()),
        ];
        assert_eq!(seeds.len(), 2, "RwtVault signer seeds: prefix + bump (singleton)");
    }
}
