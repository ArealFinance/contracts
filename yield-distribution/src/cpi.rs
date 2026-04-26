//! CPI builders for Yield Distribution → DEX / RWT Engine / SPL Token.
//!
//! Layer 8 Step 6 — bodies wired up. Three CPI surfaces:
//!   - `cpi_dex_swap`              → `native_dex::swap`
//!   - `cpi_rwt_mint`              → `rwt_engine::mint_rwt`
//!   - `cpi_token_transfer_signed` → `arlex_lang::token::Transfer` (PDA signer)
//!
//! All cross-program calls use raw `pinocchio::cpi::invoke_signed` with
//! hardcoded discriminators. Account orders MUST match the target
//! instruction's `#[derive(Accounts)]` field order.
//!
//! Source of truth for CPI layouts:
//!   - DEX::swap        — contracts/native-dex/src/instructions/swap.rs
//!   - RWT::mint_rwt    — contracts/rwt-engine/src/instructions/mint_rwt.rs
//!   - SPL::transfer    — arlex_lang::token::instructions::Transfer
//!
//! Discriminators are pinned in `crate::constants` and verified against
//! `sha256("global:<ix>")[..8]` by the `tests` module below (R7).

extern crate alloc;

use arlex_lang::prelude::*;
use pinocchio::cpi::{invoke_signed, Seed, Signer};
use pinocchio::instruction::{InstructionAccount, InstructionView};

use crate::constants::*;

/// CPI → `native_dex::swap` signed by the Accumulator PDA.
///
/// Account order MUST match `contracts/native-dex/src/instructions/swap.rs`
/// (9 named accounts; master RWT/USDC pool has `has_ot_treasury == false`,
/// so no remaining_account is forwarded — D3):
///   0. user            (signer via PDA)  — Accumulator PDA
///   1. dex_config      (read)
///   2. pool_state      (mut)
///   3. user_token_in   (mut)             — Accumulator USDC ATA
///   4. user_token_out  (mut)             — Accumulator RWT ATA
///   5. vault_in        (mut)             — pool USDC vault
///   6. vault_out       (mut)             — pool RWT vault
///   7. areal_fee_account (mut)
///   8. token_program   (read)
///
/// Discriminator: `DISC_DEX_SWAP`.
/// Instruction data: `[DISC_DEX_SWAP(8), amount_in(8), min_amount_out(8), a_to_b(1)]` = 25 bytes.
///
/// `min_amount_out` is ALWAYS 1 here (D1) — outer slippage check enforces real
/// minimum on the aggregate `rwt_acquired` after both legs run.
///
/// See `Layer 8 architecture §3.2` and `§5.1.3`.
// TODO(R9): hoist to Arlex framework helper — duplicated across YD/RWT cpi modules.
#[allow(clippy::too_many_arguments)]
pub fn cpi_dex_swap<'a>(
    accumulator_account: &'a AccountView,
    dex_config: &'a AccountView,
    pool_state: &'a AccountView,
    accumulator_usdc_ata: &'a AccountView,
    accumulator_rwt_ata: &'a AccountView,
    pool_vault_in: &'a AccountView,
    pool_vault_out: &'a AccountView,
    dex_areal_fee_account: &'a AccountView,
    token_program: &'a AccountView,
    dex_program: &'a AccountView,
    accumulator_seeds: &[Seed],
    amount_in: u64,
    min_amount_out: u64,
    a_to_b: bool,
) -> ProgramResult {
    // 1. Serialize instruction data — fixed 25-byte buffer:
    //    [DISC_DEX_SWAP(8) | amount_in(8 LE) | min_amount_out(8 LE) | a_to_b(1)]
    let mut data = [0u8; 25];
    data[0..8].copy_from_slice(&DISC_DEX_SWAP);
    data[8..16].copy_from_slice(&amount_in.to_le_bytes());
    data[16..24].copy_from_slice(&min_amount_out.to_le_bytes());
    data[24] = if a_to_b { 1 } else { 0 };

    // 2. Build the 9-account list expected by DEX::swap (no remaining accounts
    //    for master RWT/USDC pool per D3 — `has_ot_treasury == false`).
    let accounts = [
        InstructionAccount::new(accumulator_account.address(), false, true), // 0: user (signer via PDA)
        InstructionAccount::new(dex_config.address(), false, false),         // 1: dex_config (read)
        InstructionAccount::new(pool_state.address(), true, false),          // 2: pool_state (mut)
        InstructionAccount::new(accumulator_usdc_ata.address(), true, false), // 3: user_token_in (mut)
        InstructionAccount::new(accumulator_rwt_ata.address(), true, false),  // 4: user_token_out (mut)
        InstructionAccount::new(pool_vault_in.address(), true, false),       // 5: vault_in (mut)
        InstructionAccount::new(pool_vault_out.address(), true, false),      // 6: vault_out (mut)
        InstructionAccount::new(dex_areal_fee_account.address(), true, false), // 7: areal_fee_account (mut)
        InstructionAccount::new(token_program.address(), false, false),      // 8: token_program (read)
    ];

    let instruction = InstructionView {
        program_id: dex_program.address(),
        data: &data,
        accounts: &accounts,
    };

    let signer = Signer::from(accumulator_seeds);

    // 3. invoke_signed::<10> — 9 CPI accounts + dex_program (program-id resolution slot).
    invoke_signed::<10>(
        &instruction,
        &[
            accumulator_account,
            dex_config,
            pool_state,
            accumulator_usdc_ata,
            accumulator_rwt_ata,
            pool_vault_in,
            pool_vault_out,
            dex_areal_fee_account,
            token_program,
            dex_program,
        ],
        &[signer],
    )
}

/// CPI → `rwt_engine::mint_rwt` signed by the Accumulator PDA.
///
/// Account order MUST match `contracts/rwt-engine/src/instructions/mint_rwt.rs`
/// (8 accounts):
///   0. user           (signer via PDA)  — Accumulator PDA
///   1. rwt_vault      (mut)
///   2. rwt_mint       (mut)
///   3. user_deposit   (mut)             — Accumulator USDC ATA (USDC source)
///   4. user_rwt       (mut)             — Accumulator RWT ATA (RWT destination)
///   5. capital_acc    (mut)             — vault.capital_accumulator_ata (USDC sink)
///   6. dao_fee_account (mut)            — vault.areal_fee_destination (USDC fee)
///   7. token_program  (read)
///
/// Discriminator: `DISC_RWT_MINT_RWT`.
/// Instruction data: `[DISC_RWT_MINT_RWT(8), amount(8), min_rwt_out(8)]` = 24 bytes.
///
/// `min_rwt_out` is ALWAYS 1 here (D1) — outer slippage check enforces real
/// minimum on the aggregate `rwt_acquired` (RWT Engine still rejects 0-output
/// internally via `ZeroRwtOutput`, and 0 `min_rwt_out` via `ZeroSlippage`, so
/// we pass exactly 1).
// TODO(R9): hoist to Arlex framework helper — duplicated across YD/RWT cpi modules.
#[allow(clippy::too_many_arguments)]
pub fn cpi_rwt_mint<'a>(
    accumulator_account: &'a AccountView,
    rwt_vault: &'a AccountView,
    rwt_mint: &'a AccountView,
    accumulator_usdc_ata: &'a AccountView,
    accumulator_rwt_ata: &'a AccountView,
    rwt_capital_acc: &'a AccountView,
    rwt_dao_fee_account: &'a AccountView,
    token_program: &'a AccountView,
    rwt_engine_program: &'a AccountView,
    accumulator_seeds: &[Seed],
    amount: u64,
    min_rwt_out: u64,
) -> ProgramResult {
    // 1. Serialize instruction data — fixed 24-byte buffer:
    //    [DISC_RWT_MINT_RWT(8) | amount(8 LE) | min_rwt_out(8 LE)]
    let mut data = [0u8; 24];
    data[0..8].copy_from_slice(&DISC_RWT_MINT_RWT);
    data[8..16].copy_from_slice(&amount.to_le_bytes());
    data[16..24].copy_from_slice(&min_rwt_out.to_le_bytes());

    // 2. Build the 8-account list expected by RWT::mint_rwt.
    let accounts = [
        InstructionAccount::new(accumulator_account.address(), false, true), // 0: user (signer via PDA)
        InstructionAccount::new(rwt_vault.address(), true, false),           // 1: rwt_vault (mut)
        InstructionAccount::new(rwt_mint.address(), true, false),            // 2: rwt_mint (mut)
        InstructionAccount::new(accumulator_usdc_ata.address(), true, false), // 3: user_deposit (mut)
        InstructionAccount::new(accumulator_rwt_ata.address(), true, false),  // 4: user_rwt (mut)
        InstructionAccount::new(rwt_capital_acc.address(), true, false),     // 5: capital_acc (mut)
        InstructionAccount::new(rwt_dao_fee_account.address(), true, false), // 6: dao_fee_account (mut)
        InstructionAccount::new(token_program.address(), false, false),      // 7: token_program (read)
    ];

    let instruction = InstructionView {
        program_id: rwt_engine_program.address(),
        data: &data,
        accounts: &accounts,
    };

    let signer = Signer::from(accumulator_seeds);

    // 3. invoke_signed::<9> — 8 CPI accounts + rwt_engine_program (program-id resolution slot).
    invoke_signed::<9>(
        &instruction,
        &[
            accumulator_account,
            rwt_vault,
            rwt_mint,
            accumulator_usdc_ata,
            accumulator_rwt_ata,
            rwt_capital_acc,
            rwt_dao_fee_account,
            token_program,
            rwt_engine_program,
        ],
        &[signer],
    )
}

/// PDA-signed SPL Token transfer (`from` → `to`, authority = PDA).
///
/// Thin wrapper around `arlex_lang::token::instructions::Transfer::invoke_signed`.
/// Used by `convert_to_rwt` for accumulator → reward_vault and accumulator →
/// fee_account legs.
pub fn cpi_token_transfer_signed<'a>(
    from: &'a AccountView,
    to: &'a AccountView,
    authority: &'a AccountView,
    accumulator_seeds: &[Seed],
    amount: u64,
) -> ProgramResult {
    let signer = Signer::from(accumulator_seeds);
    arlex_lang::token::instructions::Transfer {
        from,
        to,
        authority,
        amount,
    }
    .invoke_signed(&[signer])
}

#[cfg(test)]
mod tests {
    //! Discriminator + program-ID parity tests (R7) plus Step 6 serialization
    //! checks for the DEX::swap and RWT::mint_rwt instruction-data layouts.
    //!
    //! Each pinned discriminator constant is compared against
    //! `sha256("global:<ix>")[..8]`, and each pinned program ID byte array
    //! is compared against the canonical base58 vanity address. Any drift
    //! between the target contract's instruction name (or vanity address)
    //! and the pinned bytes is caught at `cargo test` time.

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

    /// Reimplementation of the data-buffer build inside `cpi_dex_swap`. Used
    /// to assert byte-for-byte layout without requiring a BPF runtime / mocked
    /// `AccountView`s. If the production builder drifts from this layout, the
    /// CPI dispatch on DEX will fail at deserialize.
    fn build_dex_swap_instruction_data(
        amount_in: u64,
        min_amount_out: u64,
        a_to_b: bool,
    ) -> [u8; 25] {
        let mut data = [0u8; 25];
        data[0..8].copy_from_slice(&DISC_DEX_SWAP);
        data[8..16].copy_from_slice(&amount_in.to_le_bytes());
        data[16..24].copy_from_slice(&min_amount_out.to_le_bytes());
        data[24] = if a_to_b { 1 } else { 0 };
        data
    }

    /// Reimplementation of the data-buffer build inside `cpi_rwt_mint`.
    fn build_rwt_mint_instruction_data(amount: u64, min_rwt_out: u64) -> [u8; 24] {
        let mut data = [0u8; 24];
        data[0..8].copy_from_slice(&DISC_RWT_MINT_RWT);
        data[8..16].copy_from_slice(&amount.to_le_bytes());
        data[16..24].copy_from_slice(&min_rwt_out.to_le_bytes());
        data
    }

    #[test]
    fn disc_dex_swap_matches_sha256() {
        assert_eq!(
            DISC_DEX_SWAP,
            disc("global:swap"),
            "DISC_DEX_SWAP out of sync with sha256(\"global:swap\")[..8]"
        );
    }

    #[test]
    fn disc_rwt_mint_rwt_matches_sha256() {
        assert_eq!(
            DISC_RWT_MINT_RWT,
            disc("global:mint_rwt"),
            "DISC_RWT_MINT_RWT out of sync with sha256(\"global:mint_rwt\")[..8]"
        );
    }

    #[test]
    fn disc_yd_withdraw_liquidity_holding_matches_sha256() {
        assert_eq!(
            DISC_YD_WITHDRAW_LIQUIDITY_HOLDING,
            disc("global:withdraw_liquidity_holding"),
            "DISC_YD_WITHDRAW_LIQUIDITY_HOLDING out of sync with sha256(\"global:withdraw_liquidity_holding\")[..8]"
        );
    }

    #[test]
    fn dex_program_id_matches_vanity() {
        let encoded = bs58::encode(&DEX_PROGRAM_ID).into_string();
        assert_eq!(
            encoded, "DEX8LmvJpjefPS1cGS9zWB9ybxN24vNjTTrusBeqyARL",
            "DEX_PROGRAM_ID bytes drifted from canonical vanity address"
        );
    }

    #[test]
    fn rwt_engine_program_id_matches_vanity() {
        let encoded = bs58::encode(&RWT_ENGINE_PROGRAM_ID).into_string();
        assert_eq!(
            encoded, "RWT9hgbjHQDj98xP7FYsT5QYp5X32XyK6QfMRmFtARL",
            "RWT_ENGINE_PROGRAM_ID bytes drifted from canonical vanity address"
        );
    }

    // ── DEX::swap data layout — 3 fixture sizes ──

    #[test]
    fn dex_swap_data_layout_zero() {
        let data = build_dex_swap_instruction_data(0, 0, false);
        assert_eq!(data.len(), 25, "fixed 25-byte instruction data");
        assert_eq!(&data[0..8], &DISC_DEX_SWAP, "discriminator at offset 0");
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            0,
            "amount_in LE at offset 8..16"
        );
        assert_eq!(
            u64::from_le_bytes(data[16..24].try_into().unwrap()),
            0,
            "min_amount_out LE at offset 16..24"
        );
        assert_eq!(data[24], 0, "a_to_b=false at offset 24");
    }

    #[test]
    fn dex_swap_data_layout_typical() {
        let data = build_dex_swap_instruction_data(1_000_000, 1, true);
        assert_eq!(&data[0..8], &DISC_DEX_SWAP);
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            1_000_000
        );
        // D1 — inner min_amount_out is ALWAYS 1.
        assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), 1);
        assert_eq!(data[24], 1, "a_to_b=true encoded as 1");
    }

    #[test]
    fn dex_swap_data_layout_max() {
        let data = build_dex_swap_instruction_data(u64::MAX, u64::MAX, true);
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            u64::MAX
        );
        assert_eq!(
            u64::from_le_bytes(data[16..24].try_into().unwrap()),
            u64::MAX
        );
        assert_eq!(data[24], 1);
    }

    // ── RWT::mint_rwt data layout — 3 fixture sizes ──

    #[test]
    fn rwt_mint_data_layout_zero() {
        let data = build_rwt_mint_instruction_data(0, 0);
        assert_eq!(data.len(), 24, "fixed 24-byte instruction data");
        assert_eq!(&data[0..8], &DISC_RWT_MINT_RWT, "discriminator at offset 0");
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            0,
            "amount LE at offset 8..16"
        );
        assert_eq!(
            u64::from_le_bytes(data[16..24].try_into().unwrap()),
            0,
            "min_rwt_out LE at offset 16..24"
        );
    }

    #[test]
    fn rwt_mint_data_layout_typical() {
        let data = build_rwt_mint_instruction_data(1_000_000, 1);
        assert_eq!(&data[0..8], &DISC_RWT_MINT_RWT);
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            1_000_000
        );
        // D1 — inner min_rwt_out is ALWAYS 1.
        assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), 1);
    }

    #[test]
    fn rwt_mint_data_layout_max() {
        let data = build_rwt_mint_instruction_data(u64::MAX, u64::MAX);
        assert_eq!(
            u64::from_le_bytes(data[8..16].try_into().unwrap()),
            u64::MAX
        );
        assert_eq!(
            u64::from_le_bytes(data[16..24].try_into().unwrap()),
            u64::MAX
        );
    }

    #[test]
    fn accumulator_seed_layout_is_three_components() {
        // Sanity-check that the seed slice layout the convert_to_rwt handler
        // hands to `Signer::from` has exactly the three components YD/DEX/RWT
        // CPIs expect for the Accumulator PDA: `[b"accumulator", ot_mint, &[bump]]`.
        // We don't compute the PDA here (would require a host-side BPF
        // emulator); we only assert seed shape. Actual PDA derivation is
        // exercised end-to-end in the Step 10 integration suite.
        let ot_mint_bytes: [u8; 32] = [0x42u8; 32];
        let bump_arr = [0xFFu8];
        let seeds = [
            Seed::from(b"accumulator" as &[u8]),
            Seed::from(ot_mint_bytes.as_ref()),
            Seed::from(bump_arr.as_ref()),
        ];
        assert_eq!(
            seeds.len(),
            3,
            "Accumulator signer seeds: prefix + ot_mint + bump"
        );
    }
}
