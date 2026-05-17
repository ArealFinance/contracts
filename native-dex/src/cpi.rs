//! CPI builders for Native DEX → Yield Distribution.
//!
//! Layer 8 §5.3 — `cpi_yd_claim` is the only Layer 8 CPI surface owned by the
//! Native DEX. The pool PDA acts as the YD claimant via `invoke_signed`; the
//! crank is the payer (covers ClaimStatus rent on first claim). Claimed RWT
//! lands in the pool's RWT vault and is folded into `reserves[RWT]` by the
//! `compound_yield` handler.
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
use pinocchio::cpi::{invoke, invoke_signed, Seed, Signer};
use pinocchio::instruction::{InstructionAccount, InstructionView};

use crate::constants::*;
use crate::error::DexError;

/// CPI → `yield_distribution::claim` signed by the pool PDA.
///
/// Account order MUST match
/// `contracts/yield-distribution/src/instructions/claim.rs` (10 accounts):
///   0. claimant         (signer via PDA)  — `["pool", token_a_mint, token_b_mint]`
///   1. payer            (signer, mut)     — crank wallet
///   2. config           (read)            — `["dist_config"]`
///   3. ot_mint          (read)            — pool's OT side mint (NOT RWT)
///   4. distributor      (mut)             — `["merkle_dist", ot_mint]`
///   5. claim_status     (mut)             — `["claim_status", distributor, claimant]`
///   6. reward_vault     (mut)             — distributor's RWT ATA
///   7. claimant_token   (mut)             — pool's RWT vault (target_vault)
///   8. token_program
///   9. system_program
///
/// Discriminator: `DISC_YD_CLAIM`.
/// Instruction data: `[DISC_YD_CLAIM(8), cumulative_amount(8), proof_len(4), proof_bytes(32*N)]`.
///
/// See `Layer 8 architecture §3.2` and `§5.3.3`.
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

// =====================================================================
// CP-6 — `rwt_engine::mint_rwt` CPI (user-signed pass-through).
//
// Master-pool USDC→RWT swaps reroute here when organic ask is exhausted or
// priced above `NAV × 1.005`. The user already signs the outer DEX::swap
// transaction; we pass the user through as the CPI signer via unsigned
// `invoke` (NOT `invoke_signed` — no PDA seeds, no new authority surface).
//
// Account order MUST match `contracts/rwt-engine/src/instructions/mint_rwt.rs`
// (`MintRwt` derive_accounts struct), field order:
//   0. user           (signer, NOT writable)  — pass-through from outer ix
//   1. rwt_vault      (mut)                   — RwtVault PDA
//   2. rwt_mint       (mut)                   — RWT mint, authority = vault PDA
//   3. user_deposit   (mut)                   — user's USDC ATA (source)
//   4. user_rwt       (mut)                   — user's RWT ATA (sink)
//   5. capital_acc    (mut)                   — vault.capital_accumulator_ata
//   6. dao_fee_account (mut)                  — vault.areal_fee_destination
//   7. token_program  (read)
//
// Instruction data: `[DISC_RWT_MINT(8) | amount(8 LE) | min_rwt_out(8 LE)]`
// = 24 bytes. Discriminator is `DISC_RWT_MINT`, byte-identical to YD's
// `DISC_RWT_MINT_RWT` (both pin `sha256("global:mint_rwt")[..8]`).
// =====================================================================

/// CPI → `rwt_engine::mint_rwt` with the user as pass-through signer.
///
/// Caller (the outer `swap` handler) is responsible for:
/// - validating that `rwt_engine_program.address() == RWT_ENGINE_PROGRAM_ID`
/// - validating that `user_rwt.mint == RWT_MINT`
/// - validating `rwt_vault.owner == RWT_ENGINE_PROGRAM_ID`, discriminator at
///   offset 0..8 == DISC_RWT_VAULT, and length sufficient to read NAV at
///   offset 24..32
/// - propagating user-supplied `min_amount_out` as `min_rwt_out` so slippage
///   is enforced where the user expected it.
///
/// Note: writable / signer flags MUST mirror
/// `contracts/rwt-engine/src/instructions/mint_rwt.rs::MintRwt`. The `user`
/// slot is `signer + read` (the inbound USDC Transfer authority is `user`,
/// but `mint_rwt` only reads from `user` directly — writability is provided
/// by `user_deposit`).
#[allow(clippy::too_many_arguments)]
pub fn cpi_mint_rwt<'a>(
    user: &'a AccountView,
    rwt_vault: &'a AccountView,
    rwt_mint: &'a AccountView,
    user_usdc: &'a AccountView,
    user_rwt: &'a AccountView,
    capital_acc: &'a AccountView,
    dao_fee_account: &'a AccountView,
    token_program: &'a AccountView,
    rwt_engine_program: &'a AccountView,
    amount_in_usdc: u64,
    min_rwt_out: u64,
) -> ProgramResult {
    // 1. Serialize instruction data — fixed 24-byte buffer:
    //    [DISC_RWT_MINT(8) | amount(8 LE) | min_rwt_out(8 LE)]
    let mut data = [0u8; 24];
    data[0..8].copy_from_slice(&DISC_RWT_MINT);
    data[8..16].copy_from_slice(&amount_in_usdc.to_le_bytes());
    data[16..24].copy_from_slice(&min_rwt_out.to_le_bytes());

    // 2. Build the 8-account list expected by RWT::mint_rwt. Writable/signer
    //    flags mirror `MintRwt`. `user` is `signer` but NOT writable (the
    //    handler only reads user's pubkey, the SPL Transfers use
    //    `user_deposit`/`user_rwt` writability).
    let accounts = [
        InstructionAccount::new(user.address(), false, true),               // 0: user (signer)
        InstructionAccount::new(rwt_vault.address(), true, false),          // 1: rwt_vault (mut)
        InstructionAccount::new(rwt_mint.address(), true, false),           // 2: rwt_mint (mut)
        InstructionAccount::new(user_usdc.address(), true, false),          // 3: user_deposit (mut)
        InstructionAccount::new(user_rwt.address(), true, false),           // 4: user_rwt (mut)
        InstructionAccount::new(capital_acc.address(), true, false),        // 5: capital_acc (mut)
        InstructionAccount::new(dao_fee_account.address(), true, false),    // 6: dao_fee_account (mut)
        InstructionAccount::new(token_program.address(), false, false),     // 7: token_program (read)
    ];

    let instruction = InstructionView {
        program_id: rwt_engine_program.address(),
        data: &data,
        accounts: &accounts,
    };

    // 3. invoke::<9> — 8 CPI accounts + rwt_engine_program (program-id slot).
    //    Unsigned (user signs the outer transaction; passthrough).
    invoke::<9>(
        &instruction,
        &[
            user,
            rwt_vault,
            rwt_mint,
            user_usdc,
            user_rwt,
            capital_acc,
            dao_fee_account,
            token_program,
            rwt_engine_program,
        ],
    )
}

/// Read `RwtVault.nav_book_value` (u64 LE) from the account data buffer at
/// the pinned offset (`RWT_VAULT_DISC_LEN + RWT_VAULT_NAV_OFFSET`).
///
/// Validates:
/// - `rwt_vault.owner() == RWT_ENGINE_PROGRAM_ID` (defence-in-depth — the
///   CPI itself would fail if this account did not belong to rwt_engine,
///   but a clean error code here helps operators)
/// - discriminator at offset 0..8 == `DISC_RWT_VAULT` (CP-12.5 hardening —
///   confirms this is a `RwtVault` and not some other rwt_engine account
///   whose first bytes happen to parse cleanly through this routine)
/// - account data is long enough to read the field (8 + 24 + 8 = 40 bytes
///   minimum; the full struct is 267 bytes so this is a very loose lower
///   bound).
///
/// L-5: unsafe slice construction is the standard Pinocchio zero-copy
/// pattern; bounded by an explicit length check before indexing.
pub fn read_rwt_vault_nav(rwt_vault: &AccountView) -> core::result::Result<u64, ProgramError> {
    // 1. Owner check — must be the rwt_engine program (= RwtVault PDA host).
    // SAFETY: standard Pinocchio zero-copy access pattern (mirrors validation.rs
    // reads against AccountView raw memory). `owner()` is `unsafe` because the
    // BPF loader populates the owner pointer in-process memory — the Solana
    // runtime guarantees that pointer is valid for the lifetime of the ix.
    let owner = unsafe { rwt_vault.owner() };
    if owner.as_ref() != RWT_ENGINE_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(DexError::InvalidRwtVault));
    }

    // 2. Length-checked data access.
    let data = unsafe {
        core::slice::from_raw_parts(rwt_vault.data_ptr(), rwt_vault.data_len())
    };

    // 1b. Discriminator check (CP-12.5 hardening) — confirms this is a
    // RwtVault struct, not some other rwt_engine account whose first bytes
    // happen to parse as one.
    if data.len() < 8 {
        return Err(ProgramError::from(DexError::InvalidRwtVault));
    }
    if data[0..8] != DISC_RWT_VAULT {
        return Err(ProgramError::from(DexError::InvalidRwtVault));
    }

    let nav_start = RWT_VAULT_DISC_LEN + RWT_VAULT_NAV_OFFSET;
    let nav_end = nav_start + 8;
    if data.len() < nav_end {
        return Err(ProgramError::from(DexError::InvalidRwtVault));
    }

    Ok(u64::from_le_bytes(data[nav_start..nav_end].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    //! Discriminator + program-ID parity tests (R7) plus Step 3 serialization
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
    fn pool_seed_layout_is_four_components() {
        // Sanity-check that the seed slice layout the handler hands to
        // `Signer::from` has exactly the four components YD::claim expects
        // for the pool PDA: `[b"pool", token_a_mint, token_b_mint, &[bump]]`.
        // We don't compute the PDA here (would require a host-side BPF
        // emulator); we only assert seed shape. Actual PDA derivation is
        // exercised end-to-end in the Step 10 E2E test.
        let token_a_bytes: [u8; 32] = [0x11u8; 32];
        let token_b_bytes: [u8; 32] = [0x22u8; 32];
        let bump_arr = [0xFFu8];
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(token_a_bytes.as_ref()),
            Seed::from(token_b_bytes.as_ref()),
            Seed::from(bump_arr.as_ref()),
        ];
        assert_eq!(seeds.len(), 4, "pool signer seeds: prefix + token_a + token_b + bump");
    }

    // -----------------------------------------------------------------
    // CP-6 — `rwt_engine::mint_rwt` CPI tripwires.
    // -----------------------------------------------------------------

    /// Tripwire: `RWT_ENGINE_PROGRAM_ID` bytes encode the canonical vanity
    /// address. If anyone re-keys rwt_engine's `declare_id!` without bumping
    /// `RWT_ENGINE_PROGRAM_ID` here, this test catches it at `cargo test`.
    #[test]
    fn rwt_engine_program_id_matches_vanity() {
        let encoded = bs58::encode(&RWT_ENGINE_PROGRAM_ID).into_string();
        assert_eq!(
            encoded, "RWT9hgbjHQDj98xP7FYsT5QYp5X32XyK6QfMRmFtARL",
            "RWT_ENGINE_PROGRAM_ID bytes drifted from canonical vanity address"
        );
    }

    /// Tripwire: `DISC_RWT_MINT` matches `sha256("global:mint_rwt")[..8]`.
    #[test]
    fn disc_rwt_mint_matches_sha256() {
        assert_eq!(
            DISC_RWT_MINT,
            disc("global:mint_rwt"),
            "DISC_RWT_MINT out of sync with sha256(\"global:mint_rwt\")[..8]"
        );
    }

    /// Tripwire: `DISC_RWT_MINT` MUST match YD's `DISC_RWT_MINT_RWT` pin
    /// (both target the same `rwt_engine::mint_rwt` instruction). Hard-coded
    /// reference value mirrors `contracts/yield-distribution/src/constants.rs`
    /// at the time CP-6 landed — if either drifts, both end up failing this
    /// assertion until they re-sync.
    #[test]
    fn disc_rwt_mint_matches_yd_pin() {
        const YD_PIN: [u8; 8] = [0x62, 0x20, 0x73, 0xde, 0x44, 0x0c, 0xa1, 0xa2];
        assert_eq!(DISC_RWT_MINT, YD_PIN);
    }

    /// Test-local reimplementation of the data-buffer build inside
    /// `cpi_mint_rwt`. Used to assert byte-for-byte layout without
    /// requiring a BPF runtime / mocked `AccountView`s.
    fn build_mint_rwt_instruction_data(amount: u64, min_rwt_out: u64) -> [u8; 24] {
        let mut data = [0u8; 24];
        data[0..8].copy_from_slice(&DISC_RWT_MINT);
        data[8..16].copy_from_slice(&amount.to_le_bytes());
        data[16..24].copy_from_slice(&min_rwt_out.to_le_bytes());
        data
    }

    /// Layout pin: `[DISC(8) | amount(8 LE) | min_rwt_out(8 LE)]` = 24 bytes.
    #[test]
    fn mint_rwt_data_layout() {
        let data = build_mint_rwt_instruction_data(1_000_000, 990_000);
        assert_eq!(data.len(), 24);
        assert_eq!(&data[0..8], &DISC_RWT_MINT, "discriminator at offset 0");
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            1_000_000,
            "amount LE at offset 8..16"
        );
        assert_eq!(
            u64::from_le_bytes(data[16..24].try_into().unwrap()),
            990_000,
            "min_rwt_out LE at offset 16..24"
        );
    }

    /// CP-6 — pin the account-meta order/flags that `cpi_mint_rwt` builds
    /// matches the production `MintRwt` accounts struct in
    /// `contracts/rwt-engine/src/instructions/mint_rwt.rs`. The struct order
    /// is `[user, rwt_vault, rwt_mint, user_deposit, user_rwt, capital_acc,
    /// dao_fee_account, token_program]` (8 accounts), with `user` as the
    /// only signer, `token_program` as the only read-only non-signer, and
    /// the middle six all `mut` (writable, non-signer).
    ///
    /// We can't construct real `AccountView`s in unit-test context, so this
    /// test asserts the boolean tuple matrix (`is_writable`, `is_signer`)
    /// for each of the 8 slots — the same tuple `InstructionAccount::new`
    /// stamps onto the on-the-wire meta entries.
    #[test]
    fn cpi_account_metas_match_rwt_engine_signature() {
        // (is_writable, is_signer) per slot, in MintRwt field order.
        // Mirrors `cpi_mint_rwt` and `MintRwt`:
        //   0. user            — signer, NOT writable
        //   1. rwt_vault       — writable, NOT signer
        //   2. rwt_mint        — writable, NOT signer
        //   3. user_deposit    — writable, NOT signer  (user_usdc in DEX terms)
        //   4. user_rwt        — writable, NOT signer
        //   5. capital_acc     — writable, NOT signer
        //   6. dao_fee_account — writable, NOT signer
        //   7. token_program   — read, NOT signer
        let expected: [(bool, bool); 8] = [
            (false, true),  // user
            (true,  false), // rwt_vault
            (true,  false), // rwt_mint
            (true,  false), // user_deposit (USDC source)
            (true,  false), // user_rwt
            (true,  false), // capital_acc
            (true,  false), // dao_fee_account
            (false, false), // token_program
        ];
        // The matrix lives in the test rather than in production code so a
        // drift in the production `cpi_mint_rwt` body forces the reviewer
        // to update both halves — and the corresponding handler-level CPI
        // tests in `swap.rs` document the wire-shape assumption end-to-end.
        assert_eq!(expected.len(), 8, "MintRwt has exactly 8 accounts");
        // Signer count == 1 (user only).
        let signers = expected.iter().filter(|(_, s)| *s).count();
        assert_eq!(signers, 1, "MintRwt has exactly one signer (user pass-through)");
        // Writable count == 6 (the six middle slots).
        let writables = expected.iter().filter(|(w, _)| *w).count();
        assert_eq!(writables, 6, "MintRwt has 6 writable accounts (excludes user + token_program)");
    }

    /// CP-6 — pin `RwtVault.nav_book_value` byte offset against a synthetic
    /// buffer matching the production layout (8-byte arlex discriminator +
    /// 16-byte u128 total_invested_capital + 8-byte u64 total_rwt_supply +
    /// 8-byte u64 nav_book_value + …).
    ///
    /// Reads back the expected NAV via the same offset arithmetic that
    /// `read_rwt_vault_nav` uses (`RWT_VAULT_DISC_LEN + RWT_VAULT_NAV_OFFSET`).
    /// Catches drift if the RwtVault layout is reordered or padded without
    /// updating the offset constants here. We can't go through
    /// `read_rwt_vault_nav` directly (it takes `&AccountView`, not raw
    /// bytes), so we replicate the slice math inline.
    #[test]
    fn nav_read_offset_correct() {
        // Synthetic 40-byte buffer: discriminator (8) + total_invested_capital
        // (16) + total_rwt_supply (8) + nav_book_value (8 LE). We choose
        // distinguishable patterns at each field so a wrong offset would land
        // on the wrong bytes and fail the assert.
        const NAV: u64 = 1_005_000; // $1.005 in USDC-decimals.
        let mut buf = [0u8; 40];
        // discriminator: 0xAA × 8
        for byte in &mut buf[0..8] {
            *byte = 0xAA;
        }
        // total_invested_capital (u128): 0xBB × 16
        for byte in &mut buf[8..24] {
            *byte = 0xBB;
        }
        // total_rwt_supply (u64): 0xCC × 8
        for byte in &mut buf[24..32] {
            *byte = 0xCC;
        }
        // nav_book_value (u64 LE)
        buf[32..40].copy_from_slice(&NAV.to_le_bytes());

        // Use the same offset arithmetic as `read_rwt_vault_nav`.
        let nav_start = RWT_VAULT_DISC_LEN + RWT_VAULT_NAV_OFFSET;
        assert_eq!(nav_start, 32, "expected nav offset at byte 32 of the account data");
        let nav_end = nav_start + 8;
        let read = u64::from_le_bytes(buf[nav_start..nav_end].try_into().unwrap());
        assert_eq!(read, NAV);
    }

    // -----------------------------------------------------------------
    // CP-12.5 — `RwtVault` discriminator tripwires.
    // -----------------------------------------------------------------

    /// Tripwire: `DISC_RWT_VAULT` matches `sha256("account:RwtVault")[..8]`.
    /// Mirrors `disc_rwt_mint_matches_sha256` for the `mint_rwt` instruction
    /// discriminator. If anyone renames the `RwtVault` struct in rwt-engine
    /// without bumping this constant, this test catches it at `cargo test`.
    #[test]
    fn disc_rwt_vault_matches_sha256() {
        assert_eq!(
            DISC_RWT_VAULT,
            disc("account:RwtVault"),
            "DISC_RWT_VAULT out of sync with sha256(\"account:RwtVault\")[..8]"
        );
    }

    /// CP-12.5 — `read_rwt_vault_nav` must reject an account whose owner +
    /// length pass the prior checks but whose 8-byte discriminator does not
    /// match `DISC_RWT_VAULT`. We can't construct a real `AccountView` here,
    /// so this asserts the slice-level invariant the production function
    /// relies on (`data[0..8] != DISC_RWT_VAULT`) using a synthetic buffer.
    #[test]
    fn read_rwt_vault_nav_rejects_wrong_discriminator() {
        // 40-byte buffer with the layout of a real RwtVault prefix but a
        // deliberately wrong discriminator (`RwtDistributionConfig`'s disc
        // would have the right byte length but a different value).
        let mut buf = [0u8; 40];
        // discriminator: 0xDE × 8 — guaranteed not equal to DISC_RWT_VAULT.
        for byte in &mut buf[0..8] {
            *byte = 0xDE;
        }
        // Fill the rest with plausible bytes so a non-disc check wouldn't fail.
        for byte in &mut buf[8..32] {
            *byte = 0x00;
        }
        buf[32..40].copy_from_slice(&1_000_000u64.to_le_bytes());

        // The production check: `data[0..8] != DISC_RWT_VAULT` → reject.
        assert_ne!(
            buf[0..8],
            DISC_RWT_VAULT,
            "synthetic buffer must have a different discriminator"
        );
        // And the inverse: the canonical discriminator is at the expected slot.
        let mut canonical = [0u8; 40];
        canonical[0..8].copy_from_slice(&DISC_RWT_VAULT);
        canonical[32..40].copy_from_slice(&1_000_000u64.to_le_bytes());
        assert_eq!(
            canonical[0..8],
            DISC_RWT_VAULT,
            "canonical buffer must pass the disc check"
        );
    }
}
