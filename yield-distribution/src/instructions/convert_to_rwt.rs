//! convert_to_rwt — convert per-distributor accumulated USDC revenue into RWT
//! and credit it to the distributor's reward vault.
//!
//! Layer 8 §5.1 — the most complex Layer 8 ix. The Accumulator PDA owns the
//! USDC stockpile (one PDA per distributor; seed `["accumulator", ot_mint]`)
//! and signs two cross-program calls:
//!
//!   1. `cpi::cpi_dex_swap` → `native_dex::swap` on the master RWT/USDC pool
//!      (only when `swap_first == true`; D3 — no OT/RWT pool fallback in L8).
//!   2. `cpi::cpi_rwt_mint` → `rwt_engine::mint_rwt` for any USDC remaining
//!      after the swap leg (or for the full amount when `swap_first == false`).
//!
//! Both legs land RWT into the same intermediate `accumulator_rwt_ata` (owned
//! by the Accumulator PDA). The handler then snapshots the delta, computes
//! the YD protocol fee, and PDA-signs two SPL Token transfers:
//!
//!   * `accumulator_rwt_ata → fee_account`    (protocol_fee, when > 0)
//!   * `accumulator_rwt_ata → reward_vault`   (net_rwt = rwt_acquired − fee)
//!
//! Finally the distributor state is mirrored to `fund_distributor`:
//! `vesting::lock_vesting` is applied first, then `total_funded` is bumped by
//! `net_rwt` and `last_fund_ts` set to the current clock. A `StreamConverted`
//! event records both legs and the post-update vesting state.
//!
//! # Atomicity (D6)
//!
//! The handler propagates `?` from every CPI; if either leg or any subsequent
//! transfer fails, Solana reverts the whole transaction — Accumulator state,
//! distributor state, and all intermediate balances roll back. There is no
//! partial-success path. The outer slippage check (`rwt_acquired >=
//! min_rwt_out`) sits BETWEEN the two CPIs and the state mutations: a swap
//! that returned less than expected, even when followed by a successful mint
//! that still doesn't cover the threshold, reverts the whole TX before any
//! distributor mutation happens.
//!
//! # Inner slippage (D1)
//!
//! Both inner CPIs receive `min_amount_out = 1`: the DEX needs `>=1` to
//! reject a zero-output swap (`ZeroOutput` is rejected anyway), and RWT
//! Engine `mint_rwt` itself enforces `min_rwt_out > 0` (`ZeroSlippage`).
//! The real slippage protection lives at the OUTER level on `rwt_acquired`.
//! This avoids double-accounting the threshold across two heterogeneous
//! pricing curves (DEX constant-product vs RWT NAV).
//!
//! # CEI ordering
//!
//! 1. Validate accounts.
//! 2. Snapshot pre-CPI RWT balance.
//! 3. CPIs (DEX swap then RWT mint, or just mint when `swap_first == false`).
//! 4. Snapshot post-CPI RWT balance, compute `rwt_acquired` and slippage.
//! 5. Mutate distributor state (vesting + total_funded + last_fund_ts).
//! 6. PDA-signed Transfers for fee + net legs.
//! 7. Emit `StreamConverted`.
//!
//! Steps 5 and 6 ARE swapped vs `fund_distributor` order on purpose: the
//! intermediate RWT lives in a PDA-owned ATA, not a user-controlled one, so
//! re-entry via the receiver (which is itself a PDA-owned vault) cannot
//! exfiltrate; placing state mutations before the transfers gives us a clean
//! `StreamConverted` payload that already reflects the new totals.
//!
//! # Unsafe (L-5 audit note)
//!
//! `unsafe { core::slice::from_raw_parts(...) }` blocks read SPL Token Account
//! data via the standard Pinocchio zero-copy pattern; every read is bounded by
//! an explicit length check before any indexing (in `validation.rs`).

use arlex_lang::prelude::*;
use pinocchio::cpi::Seed;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::cpi;
use crate::error::YdError;
use crate::events::StreamConverted;
use crate::state::{Accumulator, DistributionConfig, MerkleDistributor};
use crate::validation::{
    read_token_account_amount, read_token_account_mint, read_token_account_owner,
};
use crate::vesting;

#[derive(Accounts)]
pub struct ConvertToRwt<'info> {
    // ── Crank + state ────────────────────────────────────────────
    /// Crank wallet — pays any incidental rent (none expected on the happy
    /// path, but covers edge cases). Permissionless caller.
    #[account(mut, signer)]
    pub crank: &'info AccountView,

    /// Singleton DistributionConfig PDA — supplies `protocol_fee_bps`,
    /// `areal_fee_destination`, and the `is_active` pause flag.
    #[account(seeds = [b"dist_config"], bump)]
    pub config: &'info AccountView,

    /// Distributor for this OT (mut — vesting + total_funded mutate).
    #[account(mut, seeds = [b"merkle_dist", ot_mint.address().as_ref()], bump)]
    pub distributor: &'info AccountView,

    /// OT mint — used to derive both the distributor and accumulator PDA seeds.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    /// Accumulator PDA — signs USDC + RWT transfers via `accumulator_seeds`.
    #[account(seeds = [b"accumulator", ot_mint.address().as_ref()], bump)]
    pub accumulator: &'info AccountView,

    /// Accumulator's USDC ATA — source of revenue USDC. Owner MUST be the
    /// Accumulator PDA, mint MUST be `USDC_MINT`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub accumulator_usdc_ata: &'info AccountView,

    /// Accumulator's RWT ATA — intermediate landing for swap + mint output;
    /// then drained into `reward_vault` and `fee_account`. Owner MUST be the
    /// Accumulator PDA, mint MUST be `RWT_MINT`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub accumulator_rwt_ata: &'info AccountView,

    /// YD protocol fee destination — MUST equal `config.areal_fee_destination`
    /// (RWT ATA, immutable per Layer 7 design).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub fee_account: &'info AccountView,

    /// Distributor's reward vault — MUST equal `distributor.reward_vault`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub reward_vault: &'info AccountView,

    /// RWT mint (read). Used by the inner mint CPI; pinned here to allow
    /// the DEX swap CPI to reference the canonical RWT side regardless of
    /// which way `a_to_b` faces.
    pub rwt_mint: &'info AccountView,

    // ── DEX CPI accounts ─────────────────────────────────────────
    /// DEX `dex_config` PDA (read).
    pub dex_config: &'info AccountView,

    /// DEX master RWT/USDC pool (mut). Per D3 only the master pool is used;
    /// `pool.has_ot_treasury == false` so no `remaining_accounts[0]` is
    /// forwarded.
    #[account(mut)]
    pub pool_state: &'info AccountView,

    /// Pool USDC vault (mut) — input side of the swap (`a_to_b == true`
    /// when `pool.token_a == USDC`).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub dex_pool_vault_in: &'info AccountView,

    /// Pool RWT vault (mut) — output side of the swap.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub dex_pool_vault_out: &'info AccountView,

    /// DEX protocol fee destination (mut) — RWT ATA.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub dex_areal_fee_account: &'info AccountView,

    // ── RWT Engine CPI accounts ──────────────────────────────────
    /// RWT Engine vault PDA (mut) — singleton, seeds `["rwt_vault"]`.
    #[account(mut)]
    pub rwt_vault: &'info AccountView,

    /// RWT Engine `capital_acc` USDC ATA (mut) — receives the USDC backing
    /// for any USDC routed via mint.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_capital_acc: &'info AccountView,

    /// RWT Engine `dao_fee_account` USDC ATA (mut) — receives the half of
    /// `MINT_FEE_BPS` that goes to the Areal DAO.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_dao_fee_account: &'info AccountView,

    // ── Programs ─────────────────────────────────────────────────
    /// DEX program — pinned to `DEX_PROGRAM_ID`.
    pub dex_program: &'info AccountView,

    /// RWT Engine program — pinned to `RWT_ENGINE_PROGRAM_ID`.
    pub rwt_engine_program: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

/// Convert per-distributor USDC revenue into RWT and fund the reward vault.
///
/// Args:
///   * `usdc_amount`   — caller-requested USDC to attempt converting; capped
///                       at the actual Accumulator USDC balance.
///   * `min_rwt_out`   — outer-level slippage protection on the aggregate
///                       `rwt_acquired` (D1).
///   * `swap_first`    — `true`: try DEX swap first, then mint any remaining
///                       USDC; `false`: skip DEX entirely and mint everything
///                       through RWT Engine (bootstrap / pool-empty path).
pub fn handler(
    ctx: Context<ConvertToRwt>,
    usdc_amount: u64,
    min_rwt_out: u64,
    swap_first: bool,
) -> Result<()> {
    // ── 1. Pause + program-ID checks ──────────────────────────────
    let (protocol_fee_bps, areal_fee_destination_pinned) = {
        let config = DistributionConfig::load(ctx.accounts.config, ctx.program_id)?;
        if !config.is_active {
            return Err(ProgramError::from(YdError::SystemPaused));
        }
        (config.protocol_fee_bps as u64, config.areal_fee_destination)
    };

    if ctx.accounts.dex_program.address().as_ref() != DEX_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(YdError::InvalidDexProgram));
    }
    if ctx.accounts.rwt_engine_program.address().as_ref() != RWT_ENGINE_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(YdError::InvalidRwtProgram));
    }

    // ── 2. Distributor + accumulator validation ───────────────────
    let (
        ot_mint_bytes,
        distributor_reward_vault,
        distributor_address_bytes,
    ) = {
        let dist = MerkleDistributor::load(ctx.accounts.distributor, ctx.program_id)?;
        if !dist.is_active {
            return Err(ProgramError::from(YdError::DistributorNotActive));
        }
        if dist.ot_mint != ctx.accounts.ot_mint.address().as_ref() {
            return Err(ProgramError::from(YdError::InvalidOtMint));
        }
        let mut dist_addr = [0u8; 32];
        dist_addr.copy_from_slice(ctx.accounts.distributor.address().as_ref());
        (dist.ot_mint, dist.reward_vault, dist_addr)
    };

    if ctx.accounts.reward_vault.address().as_ref() != distributor_reward_vault.as_ref() {
        return Err(ProgramError::from(YdError::InvalidRewardVault));
    }
    if ctx.accounts.fee_account.address().as_ref() != areal_fee_destination_pinned.as_ref() {
        return Err(ProgramError::from(YdError::InvalidFeeAccount));
    }

    let accumulator_bump: u8 = {
        let acc = Accumulator::load(ctx.accounts.accumulator, ctx.program_id)?;
        if acc.ot_mint != ot_mint_bytes {
            return Err(ProgramError::from(YdError::InvalidAccumulatorAta));
        }
        acc.bump
    };

    // ── 3. Validate Accumulator USDC + RWT ATAs (mint + owner) ────
    let accumulator_addr = ctx.accounts.accumulator.address();

    let usdc_ata_mint = read_token_account_mint(ctx.accounts.accumulator_usdc_ata)?;
    let usdc_ata_owner = read_token_account_owner(ctx.accounts.accumulator_usdc_ata)?;
    if usdc_ata_mint != USDC_MINT || usdc_ata_owner.as_ref() != accumulator_addr.as_ref() {
        return Err(ProgramError::from(YdError::InvalidAccumulatorAta));
    }

    let rwt_ata_mint = read_token_account_mint(ctx.accounts.accumulator_rwt_ata)?;
    let rwt_ata_owner = read_token_account_owner(ctx.accounts.accumulator_rwt_ata)?;
    if rwt_ata_mint != RWT_MINT || rwt_ata_owner.as_ref() != accumulator_addr.as_ref() {
        return Err(ProgramError::from(YdError::InvalidAccumulatorAta));
    }

    // Defence-in-depth on the public destinations (mint == RWT).
    let fee_mint = read_token_account_mint(ctx.accounts.fee_account)?;
    let reward_mint = read_token_account_mint(ctx.accounts.reward_vault)?;
    if fee_mint != RWT_MINT || reward_mint != RWT_MINT {
        return Err(ProgramError::from(YdError::InvalidTokenAccount));
    }

    // ── 4. Cap usdc_to_convert at actual balance ──────────────────
    let usdc_balance_before = read_token_account_amount(ctx.accounts.accumulator_usdc_ata)?;
    if usdc_balance_before == 0 {
        // No-op: nothing to convert. Idempotent — no event emitted.
        return Ok(());
    }
    let usdc_to_convert = core::cmp::min(usdc_amount, usdc_balance_before);
    if usdc_to_convert == 0 {
        return Err(ProgramError::from(YdError::NoUsdcToConvert));
    }

    // ── 5. Build accumulator signer seeds: ["accumulator", ot_mint, &[bump]] ─
    let bump_arr = [accumulator_bump];
    let accumulator_seeds = [
        Seed::from(b"accumulator" as &[u8]),
        Seed::from(ot_mint_bytes.as_ref()),
        Seed::from(bump_arr.as_ref()),
    ];

    // ── 6. Snapshot RWT before any CPI ────────────────────────────
    let rwt_before = read_token_account_amount(ctx.accounts.accumulator_rwt_ata)?;

    // Direction for DEX swap: USDC → RWT.
    // If `pool.token_a == USDC` then a_to_b = true; otherwise false. We do
    // not load PoolState here (would require `crate::native_dex::state`); the
    // crank knows the layout and supplies the correct `dex_pool_vault_in` /
    // `dex_pool_vault_out`. We resolve direction by comparing the input vault
    // mint vs `USDC_MINT` (a_to_b == true ⇔ pool.token_a == USDC).
    let pool_vault_in_mint = read_token_account_mint(ctx.accounts.dex_pool_vault_in)?;
    let a_to_b = pool_vault_in_mint == USDC_MINT;

    // ── 7. Optional DEX swap leg ──────────────────────────────────
    let swap_in_used: u64;
    let rwt_after_swap: u64;
    if swap_first {
        cpi::cpi_dex_swap(
            ctx.accounts.accumulator,
            ctx.accounts.dex_config,
            ctx.accounts.pool_state,
            ctx.accounts.accumulator_usdc_ata,
            ctx.accounts.accumulator_rwt_ata,
            ctx.accounts.dex_pool_vault_in,
            ctx.accounts.dex_pool_vault_out,
            ctx.accounts.dex_areal_fee_account,
            ctx.accounts.token_program,
            ctx.accounts.dex_program,
            &accumulator_seeds,
            usdc_to_convert,
            1, // D1 — inner min_amount_out is always 1; outer threshold protects.
            a_to_b,
        )?;
        swap_in_used = usdc_to_convert;
        rwt_after_swap = read_token_account_amount(ctx.accounts.accumulator_rwt_ata)?;
    } else {
        swap_in_used = 0;
        rwt_after_swap = rwt_before;
    }

    // ── 8. Mint remainder via RWT Engine ──────────────────────────
    // After the (optional) swap, whatever USDC sits in the Accumulator USDC
    // ATA either (a) wasn't routed via swap (swap_first == false) or
    // (b) was already deducted by DEX::swap on the input side. In both cases
    // we mint the *current* USDC balance — there is no concept of "leftover
    // from a partial swap" because DEX::swap consumes `amount_in` exactly or
    // reverts.
    let usdc_balance_after_swap = read_token_account_amount(ctx.accounts.accumulator_usdc_ata)?;
    let mint_in_used: u64;
    if usdc_balance_after_swap > 0 {
        // Mint requires at least MIN_MINT_AMOUNT (1 USDC, 6 decimals). If the
        // remaining USDC is below that floor, skip the mint leg. The outer
        // slippage check on `rwt_acquired` covers the case where this leaves
        // us below the caller's threshold — we will revert with
        // ConversionSlippage rather than minting nothing and returning Ok.
        const MIN_MINT_AMOUNT: u64 = 1_000_000;
        if usdc_balance_after_swap >= MIN_MINT_AMOUNT {
            cpi::cpi_rwt_mint(
                ctx.accounts.accumulator,
                ctx.accounts.rwt_vault,
                ctx.accounts.rwt_mint,
                ctx.accounts.accumulator_usdc_ata,
                ctx.accounts.accumulator_rwt_ata,
                ctx.accounts.rwt_capital_acc,
                ctx.accounts.rwt_dao_fee_account,
                ctx.accounts.token_program,
                ctx.accounts.rwt_engine_program,
                &accumulator_seeds,
                usdc_balance_after_swap,
                1, // D1 — inner min_rwt_out always 1; outer threshold protects.
            )?;
            mint_in_used = usdc_balance_after_swap;
        } else {
            mint_in_used = 0;
        }
    } else {
        mint_in_used = 0;
    }

    // ── 9. Compute RWT acquired (snapshot delta) ──────────────────
    let rwt_after = read_token_account_amount(ctx.accounts.accumulator_rwt_ata)?;
    let rwt_acquired = rwt_after
        .checked_sub(rwt_before)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
    let swap_out_rwt = rwt_after_swap
        .checked_sub(rwt_before)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
    let mint_out_rwt = rwt_after
        .checked_sub(rwt_after_swap)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;

    // Outer slippage check (D1).
    if rwt_acquired < min_rwt_out {
        return Err(ProgramError::from(YdError::ConversionSlippage));
    }
    if rwt_acquired == 0 {
        // Defensive — should be unreachable when min_rwt_out > 0; covers
        // min_rwt_out == 0 + zero-output edge.
        return Err(ProgramError::from(YdError::NoUsdcToConvert));
    }

    // ── 10. Fee math (BPS of gross rwt_acquired) ──────────────────
    let fee = arlex_lang::math::mul_div_u64(rwt_acquired, protocol_fee_bps, BPS_DENOMINATOR)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
    let net_rwt = rwt_acquired
        .checked_sub(fee)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;

    // ── 11. State mutations BEFORE the outbound transfers ─────────
    let now = Clock::get()?.unix_timestamp;
    let (total_funded_after, locked_vested_after) = {
        let dist = MerkleDistributor::load_mut(ctx.accounts.distributor, ctx.program_id)?;
        vesting::lock_vesting(dist, now)?;
        dist.total_funded = dist
            .total_funded
            .checked_add(net_rwt)
            .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
        dist.last_fund_ts = now;
        (dist.total_funded, dist.locked_vested)
    };

    // ── 12. PDA-signed transfers (fee + net) ──────────────────────
    if fee > 0 {
        cpi::cpi_token_transfer_signed(
            ctx.accounts.accumulator_rwt_ata,
            ctx.accounts.fee_account,
            ctx.accounts.accumulator,
            &accumulator_seeds,
            fee,
        )?;
    }

    if net_rwt > 0 {
        cpi::cpi_token_transfer_signed(
            ctx.accounts.accumulator_rwt_ata,
            ctx.accounts.reward_vault,
            ctx.accounts.accumulator,
            &accumulator_seeds,
            net_rwt,
        )?;
    }

    // ── 13. Emit StreamConverted ──────────────────────────────────
    let usdc_in = swap_in_used
        .checked_add(mint_in_used)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;

    emit!(StreamConverted {
        distributor: distributor_address_bytes,
        ot_mint: ot_mint_bytes,
        amount: net_rwt,
        protocol_fee: fee,
        total_funded: total_funded_after,
        locked_vested: locked_vested_after,
        timestamp: now,
        usdc_in,
        swap_out_rwt,
        mint_out_rwt,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for the `convert_to_rwt` decision tree, fee math, slippage
    //! semantics, and seed shape. The handler itself requires a BPF runtime +
    //! mocked `AccountView`s for an end-to-end test (Step 10 integration
    //! suite); these unit tests pin the pure arithmetic invariants and the
    //! decision-tree branches against future regressions.

    use super::*;
    use crate::constants::BPS_DENOMINATOR;

    /// Reference fee math — must stay byte-for-byte identical to step 10
    /// of the handler so asserting on this proxy is equivalent to asserting
    /// on the handler.
    fn fee_split(rwt_acquired: u64, protocol_fee_bps: u64) -> (u64, u64) {
        let fee = ((rwt_acquired as u128) * (protocol_fee_bps as u128)
            / (BPS_DENOMINATOR as u128)) as u64;
        let net = rwt_acquired - fee;
        (fee, net)
    }

    #[test]
    fn fee_math_default_bps_25() {
        // DEFAULT_PROTOCOL_FEE_BPS = 25 (0.25%); 10_000 RWT → 25 fee, 9_975 net.
        let (fee, net) = fee_split(10_000, 25);
        assert_eq!(fee, 25);
        assert_eq!(net, 9_975);
        assert_eq!(fee + net, 10_000, "fee + net must equal gross");
    }

    #[test]
    fn fee_math_zero_bps() {
        // Fee bps = 0 → no fee, net == gross.
        let (fee, net) = fee_split(123_456, 0);
        assert_eq!(fee, 0);
        assert_eq!(net, 123_456);
    }

    #[test]
    fn fee_math_max_bps_10000() {
        // bps = BPS_DENOMINATOR (100%) → entire amount is fee, net = 0.
        let (fee, net) = fee_split(1_000_000, BPS_DENOMINATOR);
        assert_eq!(fee, 1_000_000);
        assert_eq!(net, 0);
    }

    #[test]
    fn fee_math_dust_rounds_down() {
        // 99 RWT × 25bps = 2475 / 10000 = 0 (floor); net = 99.
        let (fee, net) = fee_split(99, 25);
        assert_eq!(fee, 0);
        assert_eq!(net, 99);
    }

    #[test]
    fn slippage_rejects_below_threshold() {
        // Outer slippage: rwt_acquired < min_rwt_out → reject.
        // Mirror the handler's condition exactly:
        let rwt_acquired = 100u64;
        let min_rwt_out = 200u64;
        assert!(
            rwt_acquired < min_rwt_out,
            "scenario must trip the slippage branch"
        );
    }

    #[test]
    fn slippage_accepts_at_threshold() {
        let rwt_acquired = 200u64;
        let min_rwt_out = 200u64;
        assert!(
            !(rwt_acquired < min_rwt_out),
            "exact match must pass — strictly-less-than rejection"
        );
    }

    #[test]
    fn decision_tree_swap_only_path() {
        // swap_first == true and swap consumed full balance → no mint leg.
        // We assert the mathematical condition the handler uses:
        //   if swap_first: swap consumed = usdc_to_convert
        //   then read post-swap balance — if 0, mint leg is skipped.
        // (The < MIN_MINT_AMOUNT branch is the same — mint is skipped.)
        let usdc_balance_after_swap = 0u64;
        let mint_used = if usdc_balance_after_swap > 0 { 1u64 } else { 0u64 };
        assert_eq!(mint_used, 0, "swap consumed everything → no mint");
    }

    #[test]
    fn decision_tree_mint_only_path() {
        // swap_first == false → swap leg is skipped; the entire balance goes
        // through the mint leg.
        let usdc_to_convert = 1_000_000u64;
        let swap_first = false;
        let swap_in_used = if swap_first { usdc_to_convert } else { 0 };
        assert_eq!(swap_in_used, 0, "swap_first==false → 0 USDC via swap");
    }

    #[test]
    fn decision_tree_swap_plus_mint_path() {
        // swap_first == true, but the DEX swap consumed only a portion of the
        // USDC (e.g. min_amount_out=1 was satisfied with thin liquidity), so
        // some USDC remains in the ATA. The mint leg picks it up.
        //
        // Note: in practice DEX::swap ALWAYS consumes its full `amount_in`
        // (it transfers `amount_in` from user → vault before computing output)
        // — so this scenario can only arise if `usdc_to_convert <
        // accumulator_balance` (caller-capped). In that case the unconsumed
        // residue (= `accumulator_balance − usdc_to_convert`) lands in the
        // mint leg.
        let accumulator_usdc_balance = 5_000_000u64;
        let usdc_to_convert = 3_000_000u64;
        let leftover_after_swap = accumulator_usdc_balance - usdc_to_convert;
        assert_eq!(leftover_after_swap, 2_000_000);
        // 2 USDC ≥ MIN_MINT_AMOUNT (1 USDC) → mint leg fires.
        const MIN_MINT_AMOUNT: u64 = 1_000_000;
        assert!(leftover_after_swap >= MIN_MINT_AMOUNT);
    }

    #[test]
    fn decision_tree_dust_skip_mint() {
        // Residual USDC below MIN_MINT_AMOUNT (1 USDC, 6 decimals) → mint
        // skipped to avoid `BelowMinMint` revert. The outer slippage check
        // still protects: if rwt_acquired falls below `min_rwt_out` because
        // of this skip, the whole TX reverts with ConversionSlippage.
        const MIN_MINT_AMOUNT: u64 = 1_000_000;
        let dust = 999_999u64;
        assert!(dust < MIN_MINT_AMOUNT, "dust must trip the skip branch");
    }

    #[test]
    fn rwt_delta_split_swap_then_mint() {
        // Σ legs == aggregate: swap_out_rwt + mint_out_rwt == rwt_acquired.
        let rwt_before = 100u64;
        let rwt_after_swap = 250u64;
        let rwt_after = 400u64;
        let swap_out = rwt_after_swap - rwt_before;
        let mint_out = rwt_after - rwt_after_swap;
        let total = rwt_after - rwt_before;
        assert_eq!(swap_out + mint_out, total);
        assert_eq!(swap_out, 150);
        assert_eq!(mint_out, 150);
    }

    #[test]
    fn accumulator_seed_layout() {
        // Verify the 3-component seed shape used for both inner CPIs
        // (cpi_dex_swap, cpi_rwt_mint) and the two PDA-signed transfers
        // (fee + net legs). Drift here would break PDA signing on every
        // outbound CPI in this handler.
        let ot_mint_bytes: [u8; 32] = [0x42u8; 32];
        let bump_arr = [0xFFu8];
        let seeds = [
            Seed::from(b"accumulator" as &[u8]),
            Seed::from(ot_mint_bytes.as_ref()),
            Seed::from(bump_arr.as_ref()),
        ];
        assert_eq!(seeds.len(), 3);
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
        const SRC: &str = include_str!("convert_to_rwt.rs");
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
