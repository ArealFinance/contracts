//! CPI builders for Yield Distribution → DEX / RWT Engine / SPL Token.
//!
//! Layer 8 Step 1 — interface stubs only. Bodies are filled in subsequent
//! steps:
//!   - Step 6 (`convert_to_rwt`): `cpi_dex_swap`, `cpi_rwt_mint`,
//!     `cpi_token_transfer_signed`.
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
use pinocchio::cpi::Seed;

use crate::constants::*;

/// CPI → `native_dex::swap` signed by the Accumulator PDA.
///
/// Account order MUST match `contracts/native-dex/src/instructions/swap.rs`.
/// Discriminator: `DISC_DEX_SWAP`.
///
/// See `Layer 8 architecture §3.2` and `§5.1.3` for the full
/// account list and handler logic.
#[allow(clippy::too_many_arguments, dead_code)] // dead_code: stub, wired up in Step 6
pub fn cpi_dex_swap<'a>(
    _accumulator_account: &'a AccountView,
    _dex_config: &'a AccountView,
    _pool_state: &'a AccountView,
    _accumulator_usdc_ata: &'a AccountView,
    _accumulator_rwt_ata: &'a AccountView,
    _pool_vault_in: &'a AccountView,
    _pool_vault_out: &'a AccountView,
    _dex_areal_fee_account: &'a AccountView,
    _token_program: &'a AccountView,
    _dex_program: &'a AccountView,
    _accumulator_seeds: &[Seed],
    _amount_in: u64,
    _min_amount_out: u64,
    _a_to_b: bool,
) -> ProgramResult {
    // TODO(Layer 8 Step 6): build instruction data
    //   [DISC_DEX_SWAP(8), amount_in(8), min_amount_out(8), a_to_b(1)]
    // and invoke_signed with `dex_program` and accumulator PDA seeds.
    // See `Layer 8 architecture §3.2` and `§5.1.3`.
    unimplemented!("cpi_dex_swap is a Layer 8 Step 1 stub; implement in Step 6")
}

/// CPI → `rwt_engine::mint_rwt` signed by the Accumulator PDA.
///
/// Account order MUST match `contracts/rwt-engine/src/instructions/mint_rwt.rs`.
/// Discriminator: `DISC_RWT_MINT_RWT`.
///
/// `min_rwt_out` is ALWAYS 1 here (D1) — outer slippage check enforces real
/// minimum on the aggregate `rwt_acquired`.
#[allow(clippy::too_many_arguments, dead_code)] // dead_code: stub, wired up in Step 6
pub fn cpi_rwt_mint<'a>(
    _accumulator_account: &'a AccountView,
    _rwt_vault: &'a AccountView,
    _rwt_mint: &'a AccountView,
    _accumulator_usdc_ata: &'a AccountView,
    _accumulator_rwt_ata: &'a AccountView,
    _rwt_capital_acc: &'a AccountView,
    _rwt_dao_fee_account: &'a AccountView,
    _token_program: &'a AccountView,
    _rwt_engine_program: &'a AccountView,
    _accumulator_seeds: &[Seed],
    _amount: u64,
    _min_rwt_out: u64,
) -> ProgramResult {
    // TODO(Layer 8 Step 6): build instruction data
    //   [DISC_RWT_MINT_RWT(8), amount(8), min_rwt_out(8)]
    // and invoke_signed with `rwt_engine_program` and accumulator PDA seeds.
    // See `Layer 8 architecture §3.2` and `§5.1.3`.
    unimplemented!("cpi_rwt_mint is a Layer 8 Step 1 stub; implement in Step 6")
}

/// PDA-signed SPL Token transfer (`from` → `to`, authority = PDA).
///
/// Thin wrapper around `arlex_lang::token::instructions::Transfer::invoke_signed`.
/// Used by `convert_to_rwt` for accumulator → reward_vault and accumulator →
/// fee_account legs.
#[allow(dead_code)] // stub, wired up in Step 6
pub fn cpi_token_transfer_signed<'a>(
    _from: &'a AccountView,
    _to: &'a AccountView,
    _authority: &'a AccountView,
    _accumulator_seeds: &[Seed],
    _amount: u64,
) -> ProgramResult {
    // TODO(Layer 8 Step 6): wrap arlex_lang::token::instructions::Transfer
    // and invoke_signed with the accumulator PDA seeds.
    // See `Layer 8 architecture §3.2`.
    unimplemented!(
        "cpi_token_transfer_signed is a Layer 8 Step 1 stub; implement in Step 6"
    )
}

#[cfg(test)]
mod tests {
    //! Discriminator + program-ID parity tests (R7).
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
}
