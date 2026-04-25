//! claim_yd_for_treasury — OtTreasury PDA claims vested RWT from a YD distributor.
//!
//! Layer 8 §5.4. The OtTreasury PDA acts as the YD claimant via `invoke_signed`;
//! the crank is the payer (covers ClaimStatus rent on first claim). Claimed RWT
//! lands in the treasury's RWT ATA. CEI is enforced inside YD::claim itself.
//!
//! Cross-project yield (D8 + architecture §5.4 note): `ot_mint` (this OT, used
//! to derive the treasury PDA) and `yd_ot_mint` (the OT mint of the distributor
//! we claim FROM) MAY differ. Example: RCP Treasury (`ot_mint = RCP`) claims
//! yield from ARL distributor (`yd_ot_mint = ARL`).
//!
//! # Unsafe (L-5 audit note)
//!
//! `unsafe { core::slice::from_raw_parts(...) }` blocks read SPL Token Account
//! data via the standard Pinocchio zero-copy pattern; every read is bounded by
//! an explicit length check before any indexing.

use arlex_lang::prelude::*;
use pinocchio::cpi::Seed;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::cpi;
use crate::error::OtError;
use crate::events::TreasuryYieldClaimed;

#[derive(Accounts)]
pub struct ClaimYdForTreasury<'info> {
    /// Crank wallet — pays ClaimStatus rent on first claim.
    #[account(mut, signer)]
    pub crank: &'info AccountView,

    /// THIS treasury's OT mint — used to re-derive the OtTreasury PDA seed.
    /// MAY differ from `yd_ot_mint` (cross-project yield).
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    /// OtTreasury PDA — signs YD::claim CPI as the claimant.
    /// Validated by seeds; bump re-derived inside the handler.
    #[account(seeds = [b"ot_treasury", ot_mint.address().as_ref()], bump)]
    pub ot_treasury: &'info AccountView,

    /// Treasury's RWT ATA — receives claimed RWT.
    /// Owner MUST equal `ot_treasury`, mint MUST equal `RWT_MINT` (handler-validated).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub treasury_rwt_ata: &'info AccountView,

    // ── YD CPI accounts (mirrors yield-distribution Claim layout) ──
    /// YD `dist_config` PDA (read).
    pub yd_config: &'info AccountView,

    /// OT mint of the source distributor (may differ from `ot_mint`).
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub yd_ot_mint: &'info AccountView,

    /// YD `merkle_dist` PDA for `yd_ot_mint` (mut).
    #[account(mut)]
    pub yd_distributor: &'info AccountView,

    /// YD `claim_status` PDA for (distributor, ot_treasury) (mut, init-if-needed).
    #[account(mut)]
    pub yd_claim_status: &'info AccountView,

    /// Distributor's RWT reward vault (mut).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub yd_reward_vault: &'info AccountView,

    /// YD program — pinned to `YD_PROGRAM_ID`.
    pub yd_program: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

/// Handler.
///
/// # Pre-CPI validation
/// 1. `yd_program == YD_PROGRAM_ID` (defence-in-depth; the CPI itself would
///    fail otherwise but we surface a clean error code).
/// 2. `ot_treasury` PDA re-derives from `["ot_treasury", ot_mint]` under THIS
///    program's id — refuses to sign on behalf of a foreign treasury.
/// 3. `treasury_rwt_ata.owner == ot_treasury` and `mint == RWT_MINT` — refuses
///    to redirect claimed RWT to a foreign ATA.
///
/// # CPI
/// `cpi::cpi_yd_claim` invokes YD::claim with the OtTreasury PDA as signer.
/// All vesting math, merkle proof verification, and CEI guards live inside
/// YD::claim — this wrapper does not duplicate them.
///
/// # Effects
/// Snapshots the treasury RWT ATA balance before/after to compute the actual
/// amount delivered (YD::claim returns Ok with `claimable == 0` for already-
/// vested-and-claimed proofs; we do not emit an event for zero-deltas).
pub fn handler(
    ctx: Context<ClaimYdForTreasury>,
    cumulative_amount: u64,
    proof: alloc::vec::Vec<[u8; 32]>,
) -> Result<()> {
    // 1. Pin the YD program ID — refuse foreign program impostors.
    if ctx.accounts.yd_program.address().as_ref() != YD_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(OtError::InvalidYdProgram));
    }

    // 2. Re-derive the OtTreasury PDA and capture its bump for the signer slice.
    let ot_mint_address = ctx.accounts.ot_mint.address();
    let (expected_treasury, treasury_bump) = arlex_lang::find_program_address(
        &[b"ot_treasury", ot_mint_address.as_ref()],
        ctx.program_id,
    );
    if ctx.accounts.ot_treasury.address() != &expected_treasury {
        return Err(ProgramError::from(OtError::InvalidOtTreasuryPda));
    }

    // 3. Validate treasury_rwt_ata: mint and owner.
    //    SPL Token Account layout: [mint: 32][owner: 32][amount: u64][...].
    let ata_data = unsafe {
        core::slice::from_raw_parts(
            ctx.accounts.treasury_rwt_ata.data_ptr(),
            ctx.accounts.treasury_rwt_ata.data_len(),
        )
    };
    if ata_data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    if &ata_data[0..32] != RWT_MINT.as_ref() {
        return Err(ProgramError::from(OtError::InvalidTreasuryAtaMint));
    }
    if &ata_data[32..64] != ctx.accounts.ot_treasury.address().as_ref() {
        return Err(ProgramError::from(OtError::InvalidTreasuryAtaOwner));
    }
    let before = u64::from_le_bytes(ata_data[64..72].try_into().unwrap());

    // 4. Build OtTreasury signer seeds: ["ot_treasury", ot_mint, &[bump]].
    let bump_arr = [treasury_bump];
    let treasury_seeds = [
        Seed::from(b"ot_treasury" as &[u8]),
        Seed::from(ot_mint_address.as_ref()),
        Seed::from(bump_arr.as_ref()),
    ];

    // 5. CPI → YD::claim. Atomic: any failure in YD reverts this handler too (D8/D9).
    cpi::cpi_yd_claim(
        ctx.accounts.ot_treasury,
        ctx.accounts.crank,
        ctx.accounts.yd_config,
        ctx.accounts.yd_ot_mint,
        ctx.accounts.yd_distributor,
        ctx.accounts.yd_claim_status,
        ctx.accounts.yd_reward_vault,
        ctx.accounts.treasury_rwt_ata,
        ctx.accounts.token_program,
        ctx.accounts.system_program,
        ctx.accounts.yd_program,
        &treasury_seeds,
        cumulative_amount,
        &proof,
    )?;

    // 6. Snapshot delta — YD::claim is allowed to return Ok with 0 transfer
    //    when nothing further has vested. Skip event in that case.
    let after_data = unsafe {
        core::slice::from_raw_parts(
            ctx.accounts.treasury_rwt_ata.data_ptr(),
            ctx.accounts.treasury_rwt_ata.data_len(),
        )
    };
    if after_data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    let after = u64::from_le_bytes(after_data[64..72].try_into().unwrap());
    let amount = after
        .checked_sub(before)
        .ok_or(ProgramError::from(OtError::MathOverflow))?;
    if amount == 0 {
        return Ok(());
    }

    // 7. Event.
    let mut ot_mint_arr = [0u8; 32];
    ot_mint_arr.copy_from_slice(ot_mint_address.as_ref());
    let mut yd_ot_mint_arr = [0u8; 32];
    yd_ot_mint_arr.copy_from_slice(ctx.accounts.yd_ot_mint.address().as_ref());

    let clock = Clock::get()?;
    emit!(TreasuryYieldClaimed {
        ot_mint: ot_mint_arr,
        yd_ot_mint: yd_ot_mint_arr,
        amount,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Treasury yield claimed");
    Ok(())
}
