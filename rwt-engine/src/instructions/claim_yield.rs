//! claim_yield — RwtVault PDA claims vested RWT from a YD distributor and
//! splits the received amount 70/15/15 between book-value backing, the
//! LiquidityHolding PDA, and the ARL Treasury.
//!
//! Layer 8 §5.2. The RwtVault PDA acts as the YD claimant via `invoke_signed`;
//! the crank is the payer (covers ClaimStatus rent on first claim). Claimed
//! RWT lands in `rwt_claim_ata` (vault-PDA-owned) and is then split per
//! `dist_config.{book_value_bps, liquidity_bps, protocol_revenue_bps}`:
//!
//!   * `book_value_share` (70%) STAYS in `rwt_claim_ata` and increments
//!     `vault.total_invested_capital` (USD-valued via current NAV) — this is
//!     the RWT backing supply: NAV per RWT recomputes after the bump.
//!   * `liquidity_share` (15%) is PDA-signed transferred to
//!     `dist_config.liquidity_destination` (singleton LiquidityHolding RWT
//!     ATA per D11.1 — Layer 9 Nexus reads this balance later).
//!   * `protocol_revenue_share` (15%) is PDA-signed transferred to
//!     `dist_config.protocol_revenue_destination` (ARL Treasury RWT ATA).
//!
//! Remainder pattern: `protocol_revenue_share = total - book_value - liquidity`
//! so any rounding leftover from the BPS multiplication is absorbed by the
//! protocol leg (no dust burn). Σ shares == total_yield by construction.
//!
//! NAV math (Layer 8 §5.2.3):
//!     usd_value = book_value_share * nav_before / NAV_SCALE
//!     vault.total_invested_capital += usd_value
//!     vault.nav_book_value = calculate_nav(capital, supply)
//! `total_rwt_supply` is unchanged — RWT was already minted (and YD reward
//! vault was funded via convert_to_rwt earlier); compounding the backing on
//! constant supply mechanically lifts NAV.
//!
//! # CEI ordering
//! State mutations on `vault` (capital + NAV) happen BEFORE the two PDA-signed
//! transfers — the YD::claim CPI itself enforces CEI internally, so the
//! cross-program guarantees hold for the full chain.
//!
//! # Unsafe (L-5 audit note)
//! `unsafe { core::slice::from_raw_parts(...) }` blocks read SPL Token Account
//! data via the standard Pinocchio zero-copy pattern; every read is bounded by
//! an explicit length check before any indexing.

use arlex_lang::prelude::*;
use pinocchio::cpi::{Seed, Signer};
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::cpi;
use crate::error::RwtError;
use crate::events::{LiquidityHoldingFunded, YieldDistributed};
use crate::nav::calculate_nav;
use crate::state::{RwtDistributionConfig, RwtVault};

#[derive(Accounts)]
pub struct ClaimYield<'info> {
    /// Crank wallet — pays ClaimStatus rent on first claim. Permissionless.
    #[account(mut, signer)]
    pub crank: &'info AccountView,

    /// RwtVault PDA — signs YD::claim CPI as the claimant; mutates capital
    /// and NAV book value. Singleton: seeds `["rwt_vault"]`.
    #[account(mut, seeds = [b"rwt_vault"], bump)]
    pub rwt_vault: &'info AccountView,

    /// RWT distribution config PDA — read-only; supplies BPS splits and the
    /// pinned `liquidity_destination` / `protocol_revenue_destination`.
    /// Singleton: seeds `["dist_config_rwt"]`.
    #[account(seeds = [b"dist_config_rwt"], bump)]
    pub dist_config: &'info AccountView,

    /// Vault's RWT claim ATA — receives the full claimed amount from YD;
    /// then transfers 30% out (15% liquidity + 15% protocol).
    /// Owner MUST equal `rwt_vault` PDA, mint MUST equal `RWT_MINT`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_claim_ata: &'info AccountView,

    /// Destination for the liquidity_share leg (15%). MUST equal
    /// `dist_config.liquidity_destination` (per D11.1 — singleton check, no
    /// PDA-shape derivation required at this layer; the deployment script
    /// pinned the LiquidityHolding RWT ATA on this field).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub liquidity_dest: &'info AccountView,

    /// Destination for the protocol_revenue_share leg (15%). MUST equal
    /// `dist_config.protocol_revenue_destination` (ARL Treasury RWT ATA).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub protocol_revenue_dest: &'info AccountView,

    // ── YD CPI accounts (mirror yield-distribution Claim layout) ──
    /// YD `dist_config` PDA (read).
    pub yd_config: &'info AccountView,

    /// OT mint of the source distributor — YD uses it to derive the
    /// distributor PDA (`["merkle_dist", ot_mint]`). NOT the RWT mint.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    /// YD `merkle_dist` PDA for `ot_mint` (mut).
    #[account(mut)]
    pub yd_distributor: &'info AccountView,

    /// YD `claim_status` PDA for (distributor, rwt_vault) (mut, init-if-needed).
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

/// Read SPL Token Account `amount` field (bytes 64..72 LE) via zero-copy.
/// SPL Token Account layout: `[mint: 32][owner: 32][amount: u64][...]`.
/// Length-checked before any indexing.
fn read_token_account_amount(account: &AccountView) -> core::result::Result<u64, ProgramError> {
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

/// Read SPL Token Account `mint` (bytes 0..32) and `owner` (bytes 32..64).
fn read_token_account_mint_and_owner(
    account: &AccountView,
) -> core::result::Result<([u8; 32], [u8; 32]), ProgramError> {
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 64 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut mint = [0u8; 32];
    mint.copy_from_slice(&data[0..32]);
    let mut owner = [0u8; 32];
    owner.copy_from_slice(&data[32..64]);
    Ok((mint, owner))
}

/// Handler.
///
/// # Pre-CPI validation
/// 1. `yd_program == YD_PROGRAM_ID` (defence-in-depth; surfaces a clean code
///    instead of the cross-program PrivilegeEscalation that would otherwise
///    fire from `invoke_signed`).
/// 2. `liquidity_dest == dist_config.liquidity_destination` (D11.1 singleton
///    check — pinned at deployment time via `update_distribution_config`).
/// 3. `protocol_revenue_dest == dist_config.protocol_revenue_destination`.
/// 4. `rwt_claim_ata.mint == RWT_MINT` and `rwt_claim_ata.owner == rwt_vault`.
///
/// # CPI
/// `cpi::cpi_yd_claim` invokes YD::claim with the RwtVault PDA as signer. All
/// vesting math, merkle proof verification, and CEI guards live inside
/// YD::claim — this wrapper does not duplicate them.
///
/// # Effects (ordered)
/// 1. Snapshot `rwt_claim_ata.amount` pre-CPI.
/// 2. CPI YD::claim (atomic — D6: any failure reverts the whole tx).
/// 3. Snapshot post-CPI; compute `claimed = after - before`. Early-exit `Ok(())`
///    on zero-delta (already-claimed at this `cumulative_amount`).
/// 4. Compute split (book/liquidity/protocol) using the remainder pattern.
/// 5. Mutate vault: `total_invested_capital += book_value_share * NAV_before`
///    (USD-valued); recompute `nav_book_value`.
/// 6. PDA-signed Transfer of `liquidity_share` and `protocol_revenue_share`.
/// 7. Emit `YieldDistributed` (always when `claimed > 0`); emit
///    `LiquidityHoldingFunded` when the liquidity leg actually moved.
pub fn handler(
    ctx: Context<ClaimYield>,
    cumulative_amount: u64,
    proof: alloc::vec::Vec<[u8; 32]>,
) -> Result<()> {
    // 1. Pin the YD program ID — refuse foreign program impostors.
    if ctx.accounts.yd_program.address().as_ref() != YD_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidYdProgram).into());
    }

    // 2. Validate distribution destinations (D11.1 — direct equality).
    let dist_cfg = RwtDistributionConfig::load(ctx.accounts.dist_config, ctx.program_id)?;
    let book_value_bps = dist_cfg.book_value_bps as u64;
    let liquidity_bps = dist_cfg.liquidity_bps as u64;
    let liquidity_destination_pinned = dist_cfg.liquidity_destination;
    let protocol_revenue_destination_pinned = dist_cfg.protocol_revenue_destination;

    if ctx.accounts.liquidity_dest.address().as_ref() != liquidity_destination_pinned.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidLiquidityDest).into());
    }
    if ctx.accounts.protocol_revenue_dest.address().as_ref()
        != protocol_revenue_destination_pinned.as_ref()
    {
        return Err(ProgramError::from(RwtError::InvalidProtocolRevenueDest).into());
    }

    // 3. Validate `rwt_claim_ata`: mint == RWT_MINT and owner == rwt_vault PDA.
    let (claim_ata_mint, claim_ata_owner) =
        read_token_account_mint_and_owner(ctx.accounts.rwt_claim_ata)?;
    if claim_ata_mint != RWT_MINT {
        return Err(ProgramError::from(RwtError::InvalidRwtClaimAta).into());
    }
    if claim_ata_owner.as_ref() != ctx.accounts.rwt_vault.address().as_ref() {
        return Err(ProgramError::from(RwtError::InvalidRwtClaimAta).into());
    }

    // 4. Snapshot vault fields needed for signing + NAV math BEFORE the CPI.
    //    We borrow only briefly; the &mut reload after CPI is what writes the
    //    new capital + NAV.
    let vault_bump: u8 = {
        let vault = RwtVault::load(ctx.accounts.rwt_vault, ctx.program_id)?;
        vault.bump
    };

    // 5. Snapshot RWT claim ATA balance pre-CPI.
    let before = read_token_account_amount(ctx.accounts.rwt_claim_ata)?;

    // 6. Build vault signer seeds: `["rwt_vault", &[bump]]` (singleton).
    let bump_arr = [vault_bump];
    let vault_seeds = [
        Seed::from(b"rwt_vault" as &[u8]),
        Seed::from(bump_arr.as_ref()),
    ];

    // 7. CPI → YD::claim. Atomic: any failure in YD reverts this handler too
    //    (D6 / D8 — Layer 8 cross-program calls are atomic; no partial state).
    cpi::cpi_yd_claim(
        ctx.accounts.rwt_vault,
        ctx.accounts.crank,
        ctx.accounts.yd_config,
        ctx.accounts.ot_mint,
        ctx.accounts.yd_distributor,
        ctx.accounts.yd_claim_status,
        ctx.accounts.yd_reward_vault,
        ctx.accounts.rwt_claim_ata,
        ctx.accounts.token_program,
        ctx.accounts.system_program,
        ctx.accounts.yd_program,
        &vault_seeds,
        cumulative_amount,
        &proof,
    )?;

    // 8. Snapshot delta — YD::claim is allowed to return Ok with 0 transfer
    //    when nothing further has vested (idempotent re-claim at the same
    //    cumulative_amount). Skip split + event in that case.
    let after = read_token_account_amount(ctx.accounts.rwt_claim_ata)?;
    let claimed = after
        .checked_sub(before)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    if claimed == 0 {
        return Ok(());
    }

    // 9. Split (remainder pattern — protocol leg absorbs rounding leftover so
    //    Σ shares == claimed exactly, regardless of BPS rounding).
    let book_value_share = arlex_lang::math::mul_div_u64(claimed, book_value_bps, BPS_DENOMINATOR)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    let liquidity_share = arlex_lang::math::mul_div_u64(claimed, liquidity_bps, BPS_DENOMINATOR)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    let protocol_revenue_share = claimed
        .checked_sub(book_value_share)
        .and_then(|v| v.checked_sub(liquidity_share))
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;

    // 10. Effects: mutate vault state BEFORE the SPL Transfer CPIs.
    //
    //     `book_value_share` is RWT in the vault claim ATA — convert it to
    //     USD via the **current** (pre-bump) NAV and add to capital. The RWT
    //     itself stays where it is (no transfer needed for the book leg).
    //     `total_rwt_supply` is unchanged: RWT was already minted upstream
    //     (convert_to_rwt funded the YD reward vault); compounding the
    //     backing on a constant supply lifts NAV mechanically.
    let (nav_before, nav_after) = {
        let vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

        // Recompute NAV from current state (defence-in-depth — never trust the
        // cached `vault.nav_book_value` field for math; mirror the pattern
        // used in mint_rwt.rs).
        let nav_before = calculate_nav(vault.total_invested_capital, vault.total_rwt_supply)?;

        // book_value_share (RWT, 6 decimals) × nav_before (USD per RWT, 6 decimals)
        // / NAV_SCALE (1e6) → USD lamports added to capital.
        // Use u128 to avoid overflow on large yields; output fits u128 trivially
        // (u64::MAX × u64::MAX / 1e6 < u128::MAX).
        let usd_value: u128 = (book_value_share as u128)
            .checked_mul(nav_before as u128)
            .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?
            .checked_div(NAV_SCALE as u128)
            .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;

        vault.total_invested_capital = vault
            .total_invested_capital
            .checked_add(usd_value)
            .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
        vault.nav_book_value =
            calculate_nav(vault.total_invested_capital, vault.total_rwt_supply)?;

        (nav_before, vault.nav_book_value)
    };

    // 11. PDA-signed Transfers for the liquidity + protocol legs.
    //     book_value_share STAYS in rwt_claim_ata as NAV backing — no transfer.
    let signer = Signer::from(&vault_seeds);

    if liquidity_share > 0 {
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.rwt_claim_ata,
            to: ctx.accounts.liquidity_dest,
            authority: ctx.accounts.rwt_vault,
            amount: liquidity_share,
        }
        .invoke_signed(&[signer.clone()])?;
    }

    if protocol_revenue_share > 0 {
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.rwt_claim_ata,
            to: ctx.accounts.protocol_revenue_dest,
            authority: ctx.accounts.rwt_vault,
            amount: protocol_revenue_share,
        }
        .invoke_signed(&[signer])?;
    }

    // 12. Events.
    let mut vault_addr = [0u8; 32];
    vault_addr.copy_from_slice(ctx.accounts.rwt_vault.address().as_ref());
    let mut ot_mint_arr = [0u8; 32];
    ot_mint_arr.copy_from_slice(ctx.accounts.ot_mint.address().as_ref());

    let clock = Clock::get()?;
    emit!(YieldDistributed {
        vault: vault_addr,
        ot_mint: ot_mint_arr,
        total_yield: claimed,
        book_value_share,
        liquidity_share,
        protocol_revenue_share,
        nav_before,
        nav_after,
        timestamp: clock.unix_timestamp,
    });

    if liquidity_share > 0 {
        let mut liquidity_holding_addr = [0u8; 32];
        liquidity_holding_addr.copy_from_slice(ctx.accounts.liquidity_dest.address().as_ref());
        emit!(LiquidityHoldingFunded {
            liquidity_holding: liquidity_holding_addr,
            ot_mint: ot_mint_arr,
            amount: liquidity_share,
            timestamp: clock.unix_timestamp,
        });
    }

    arlex_lang::log("RWT yield distributed");
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for the 70/15/15 split math (remainder pattern). The
    //! handler itself requires a BPF runtime + mocked `AccountView`s for an
    //! end-to-end test (Step 10 integration suite); these unit tests pin the
    //! pure arithmetic invariant `Σ shares == claimed` exactly so any
    //! rounding-leftover regression is caught at `cargo test` time.

    use crate::constants::{BPS_DENOMINATOR, DEFAULT_BOOK_VALUE_BPS, DEFAULT_LIQUIDITY_BPS};

    /// Reference implementation of the on-chain split. Must stay
    /// byte-for-byte identical to the handler body (steps 9 above) so that
    /// asserting on this proxy is equivalent to asserting on the handler.
    fn split(claimed: u64) -> (u64, u64, u64) {
        // mul_div semantics: floor(a * b / c). For the BPS arithmetic at hand
        // (b ≤ 10_000, c == 10_000) plain u128 multiply-then-divide matches
        // arlex_lang::math::mul_div_u64 exactly without overflow.
        let book_value_share = ((claimed as u128) * (DEFAULT_BOOK_VALUE_BPS as u128)
            / (BPS_DENOMINATOR as u128)) as u64;
        let liquidity_share = ((claimed as u128) * (DEFAULT_LIQUIDITY_BPS as u128)
            / (BPS_DENOMINATOR as u128)) as u64;
        let protocol_revenue_share = claimed - book_value_share - liquidity_share;
        (book_value_share, liquidity_share, protocol_revenue_share)
    }

    #[test]
    fn split_clean_100() {
        // 100 RWT → 70 / 15 / 15 exactly (no rounding leftover).
        let (b, l, p) = split(100);
        assert_eq!(b, 70);
        assert_eq!(l, 15);
        assert_eq!(p, 15);
        assert_eq!(b + l + p, 100, "shares must sum to total");
    }

    #[test]
    fn split_with_rounding_99() {
        // 99 RWT × 7000bps = 693000 / 10000 = 69 (floor).
        // 99 RWT × 1500bps = 148500 / 10000 = 14 (floor).
        // protocol = 99 - 69 - 14 = 16 (absorbs 1+1=2 rounding leftover).
        let (b, l, p) = split(99);
        assert_eq!(b, 69);
        assert_eq!(l, 14);
        assert_eq!(p, 16);
        assert_eq!(b + l + p, 99, "shares must sum to total");
    }

    #[test]
    fn split_dust_1() {
        // 1 RWT × 7000bps = 7000 / 10000 = 0 (floor).
        // 1 RWT × 1500bps = 1500 / 10000 = 0 (floor).
        // protocol = 1 - 0 - 0 = 1 (entire dust goes to protocol leg).
        // No NAV backing growth on dust, but invariant holds.
        let (b, l, p) = split(1);
        assert_eq!(b, 0);
        assert_eq!(l, 0);
        assert_eq!(p, 1);
        assert_eq!(b + l + p, 1, "shares must sum to total");
    }

    #[test]
    fn split_zero() {
        // claimed == 0 is the early-exit branch in the handler — split is
        // never called. The math is still well-defined: all shares zero.
        let (b, l, p) = split(0);
        assert_eq!(b, 0);
        assert_eq!(l, 0);
        assert_eq!(p, 0);
        assert_eq!(b + l + p, 0);
    }

    #[test]
    fn split_invariant_random_sweep() {
        // Σ shares == total must hold for all u64 inputs.
        // Sweep a deterministic 4096-point grid covering small/medium/large
        // ranges; protocol absorbs ALL rounding leftover so the invariant
        // is trivially satisfied — but we pin it here against future edits.
        for k in 0..4096u64 {
            let claimed = (k * 1_234_567) ^ (k.wrapping_mul(0xDEAD_BEEF));
            let (b, l, p) = split(claimed);
            assert_eq!(
                b.checked_add(l).and_then(|s| s.checked_add(p)),
                Some(claimed),
                "Σ shares != claimed at input {claimed}"
            );
        }
    }

    #[test]
    fn split_max_input() {
        // u64::MAX is the upper bound. Verify mul_div doesn't overflow under
        // the u128 widening we use (matches arlex_lang::math::mul_div_u64).
        let (b, l, p) = split(u64::MAX);
        let sum = (b as u128) + (l as u128) + (p as u128);
        assert_eq!(sum, u64::MAX as u128, "Σ shares == u64::MAX at upper bound");
    }


    /// CU-hotfix regression (2026-05-18). Eagerly-evaluated
    /// `Option::ok_or(ProgramError::from(E))` calls invoke the
    /// arlex-derive `From<E>` impl on the success path, which calls
    /// `arlex_lang::log(msg)` — burning ~100 CUs per call site and
    /// emitting a spurious "Arithmetic overflow" log line on every
    /// instruction. See `rwt-engine/src/instructions/mint_rwt.rs`
    /// (`mint_rwt_has_no_eager_ok_or_program_error`) for the full
    /// background and the smoke-3 trace that first exposed this.
    ///
    /// The detection key is reassembled from two halves so this
    /// test's own definition of it does not match.
    #[test]
    fn no_eager_ok_or_program_error() {
        const SRC: &str = include_str!("claim_yield.rs");
        const HALF_1: &str = ".ok_or(ProgramError";
        const HALF_2: &str = "::from(";
        let bad_needle = alloc::format!("{HALF_1}{HALF_2}");
        let mut hits = 0usize;
        for raw_line in SRC.lines() {
            let line = match raw_line.find("//") {
                Some(idx) => &raw_line[..idx],
                None => raw_line,
            };
            if let Some(needle_pos) = line.find(&bad_needle) {
                if line[..needle_pos].contains('"') {
                    continue;
                }
                hits += 1;
            }
        }
        assert_eq!(
            hits, 0,
            "found {hits} eager .ok_or(ProgramError-from(...)) calls — \
             use .ok_or_else(|| ...) closure form to keep the error \
             construction (and its arlex_lang::log syscall) off the \
             success path (CU-hotfix 2026-05-18)",
        );
    }
}
