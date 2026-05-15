//! compound_yield — pool PDA claims vested RWT from a YD distributor and
//! folds the received RWT into `reserve_<rwt_side>` (auto-compound for LPs).
//!
//! Layer 8 §5.3. The pool PDA acts as the YD claimant via `invoke_signed`;
//! the crank is the payer (covers ClaimStatus rent on first claim). Claimed
//! RWT lands directly in the pool's RWT vault (`pool.vault_a` or
//! `pool.vault_b` depending on RWT side); the handler then bumps
//! `pool.reserve_<rwt_side>` by the snapshot delta. The k-invariant rises in
//! the LPs' favour because LP shares are unchanged.
//!
//! `ot_mint` identifies the YD distributor — it is the OT side of the pool
//! pair (the side that is **not** RWT). YD uses it to look up the merkle
//! distributor PDA: `["merkle_dist", ot_mint]`.
//!
//! # Unsafe (L-5 audit note)
//!
//! `unsafe { core::slice::from_raw_parts(...) }` blocks read SPL Token Account
//! data (vault balance) via the standard Pinocchio zero-copy pattern; every
//! read is bounded by an explicit length check before any indexing.

use arlex_lang::prelude::*;
use pinocchio::cpi::Seed;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::cpi;
use crate::error::DexError;
use crate::events::CompoundYieldExecuted;
use crate::state::PoolState;
use crate::validation::{is_rwt_mint, validate_vault};

#[derive(Accounts)]
pub struct CompoundYield<'info> {
    /// Crank wallet — pays ClaimStatus rent on first claim. Permissionless.
    #[account(mut, signer)]
    pub crank: &'info AccountView,

    /// Pool PDA — signs YD::claim CPI as the claimant; reserve on the RWT
    /// side is bumped after the CPI delta is observed.
    /// Validated by `["pool", token_a_mint, token_b_mint]` derivation.
    #[account(mut)]
    pub pool_state: &'info AccountView,

    /// Pool's RWT vault (must equal `pool.vault_a` if token_a is RWT, else
    /// `pool.vault_b`). YD transfers claimed RWT directly into this vault.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub target_vault: &'info AccountView,

    // ── YD CPI accounts (mirror yield-distribution Claim layout) ──
    /// YD `dist_config` PDA (read).
    pub yd_config: &'info AccountView,

    /// OT mint of the source distributor — equals the pool's OT side mint
    /// (the side that is NOT RWT). YD uses it to derive the distributor PDA.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    /// YD `merkle_dist` PDA for `ot_mint` (mut).
    #[account(mut)]
    pub yd_distributor: &'info AccountView,

    /// YD `claim_status` PDA for (distributor, pool_state) (mut, init-if-needed).
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
///
/// SPL Token Account layout: `[mint: 32][owner: 32][amount: u64][...]`.
/// Length-checked before any indexing.
fn read_token_account_amount(account: &AccountView) -> Result<u64> {
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 72 {
        return Err(ProgramError::InvalidAccountData.into());
    }
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

/// Handler.
///
/// # Pre-CPI validation
/// 1. `yd_program == YD_PROGRAM_ID` (defence-in-depth; CPI itself would fail
///    otherwise but we surface a clean error code).
/// 2. `pool_state` PDA re-derives from `["pool", token_a_mint, token_b_mint]`
///    under THIS program's id — refuses to sign on behalf of a foreign pool.
/// 3. Pool is active (`pool.is_active`) — paused pools do not compound.
/// 4. RWT side is identifiable (`token_a` OR `token_b == RWT_MINT`); else
///    `TargetVaultNotRwt`. Pure-OT pools can't compound (no RWT vault to
///    receive yield).
/// 5. `target_vault == pool.vault_<rwt_side>` (handler-validated against
///    stored vault address).
/// 6. `ot_mint == pool.<other_side>_mint` (the non-RWT side; identifies the
///    YD distributor).
///
/// # CPI
/// `cpi::cpi_yd_claim` invokes YD::claim with the pool PDA as signer. All
/// vesting math, merkle proof verification, and CEI guards live inside
/// YD::claim — this wrapper does not duplicate them.
///
/// # Effects
/// Snapshots `target_vault.amount` before/after to compute the actual RWT
/// delivered (YD::claim returns Ok with `claimable == 0` for already-vested-
/// and-claimed proofs; we early-exit silently in that case). On non-zero
/// delta, increments `pool.reserve_<rwt_side>` by the delta and emits
/// `CompoundYieldExecuted`. LP shares are unchanged → k-invariant rises in
/// LPs' favour (auto-compound).
pub fn handler(
    ctx: Context<CompoundYield>,
    cumulative_amount: u64,
    proof: alloc::vec::Vec<[u8; 32]>,
) -> Result<()> {
    // 1. Pin the YD program ID — refuse foreign program impostors.
    if ctx.accounts.yd_program.address().as_ref() != YD_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(DexError::InvalidYdProgram).into());
    }

    // 2. Snapshot pool fields (token_a_mint, token_b_mint, bump, vault_a,
    //    vault_b, is_active) and validate RWT side BEFORE the CPI. We hold
    //    the borrow only briefly; the &mut reload after CPI is what writes
    //    the new reserve.
    let (
        token_a_mint,
        token_b_mint,
        vault_a,
        vault_b,
        pool_bump,
        rwt_side, // 0 = side A, 1 = side B
    ) = {
        let pool = PoolState::load(ctx.accounts.pool_state, ctx.program_id)?;

        if !pool.is_active {
            return Err(ProgramError::from(DexError::PoolNotActive).into());
        }

        // TODO(P1): consider gating compound_yield on `dex_config.is_active` for
        // symmetry with swap/add/zap (which check BOTH pool and global pause).
        // Spec (docs/contracts/native-dex.mdx compound_yield Accordion) currently
        // lists only `pool.is_active` as the validation gate and does not
        // include `dex_config` in the Accounts. Promoting global pause to a
        // true kill-switch here is a docs-first decision: spec must add
        // `dex_config` to compound_yield's Accounts before code follows, since
        // it is a breaking interface change for SDK builders and the
        // yield-claim crank. Deferred — see P0-1/P0-2 review notes.

        // Determine RWT side. is_rwt_mint compares against constants::RWT_MINT.
        let rwt_side: u8 = if is_rwt_mint(&pool.token_a_mint) {
            0
        } else if is_rwt_mint(&pool.token_b_mint) {
            1
        } else {
            return Err(ProgramError::from(DexError::TargetVaultNotRwt).into());
        };

        (
            pool.token_a_mint,
            pool.token_b_mint,
            pool.vault_a,
            pool.vault_b,
            pool.bump,
            rwt_side,
        )
    };

    // 3. Re-derive pool PDA from `["pool", token_a_mint, token_b_mint]` and
    //    cross-check against the supplied account address. Combined with the
    //    `load`/`load_mut` owner+discriminator checks, this prevents foreign
    //    pools from signing.
    let (expected_pool, derived_bump) = arlex_lang::find_program_address(
        &[b"pool", token_a_mint.as_ref(), token_b_mint.as_ref()],
        ctx.program_id,
    );
    if ctx.accounts.pool_state.address() != &expected_pool {
        return Err(ProgramError::from(DexError::InvalidPoolPda).into());
    }
    // Defensive: stored bump must match the canonical derivation. If a future
    // migration ever drifts these, signing would fail in YD with a confusing
    // PrivilegeEscalation; surface a clean code instead.
    if derived_bump != pool_bump {
        return Err(ProgramError::from(DexError::InvalidPoolPda).into());
    }

    // 4. Validate target_vault and ot_mint against pool fields.
    let (expected_vault, expected_ot_mint) = if rwt_side == 0 {
        (&vault_a, &token_b_mint) // RWT = A, OT side = B
    } else {
        (&vault_b, &token_a_mint) // RWT = B, OT side = A
    };
    if ctx.accounts.target_vault.address().as_ref() != expected_vault.as_ref() {
        return Err(ProgramError::from(DexError::InvalidTargetVault).into());
    }
    if ctx.accounts.ot_mint.address().as_ref() != expected_ot_mint.as_ref() {
        return Err(ProgramError::from(DexError::InvalidOtMint).into());
    }
    // Reuse validate_vault for parity with swap.rs (defence-in-depth; same
    // check as above but routes through the established helper).
    validate_vault(ctx.accounts.target_vault, expected_vault)?;

    // 5. Snapshot RWT vault balance pre-CPI.
    let before = read_token_account_amount(ctx.accounts.target_vault)?;

    // 6. Build pool signer seeds: ["pool", token_a_mint, token_b_mint, &[bump]].
    let bump_arr = [pool_bump];
    let pool_seeds = [
        Seed::from(b"pool" as &[u8]),
        Seed::from(token_a_mint.as_ref()),
        Seed::from(token_b_mint.as_ref()),
        Seed::from(bump_arr.as_ref()),
    ];

    // 7. CPI → YD::claim. Atomic: any failure in YD reverts this handler too
    //    (D6 / D8 — Layer 8 cross-program calls are atomic; no partial state).
    cpi::cpi_yd_claim(
        ctx.accounts.pool_state,
        ctx.accounts.crank,
        ctx.accounts.yd_config,
        ctx.accounts.ot_mint,
        ctx.accounts.yd_distributor,
        ctx.accounts.yd_claim_status,
        ctx.accounts.yd_reward_vault,
        ctx.accounts.target_vault,
        ctx.accounts.token_program,
        ctx.accounts.system_program,
        ctx.accounts.yd_program,
        &pool_seeds,
        cumulative_amount,
        &proof,
    )?;

    // 8. Snapshot delta — YD::claim is allowed to return Ok with 0 transfer
    //    when nothing further has vested. Skip reserve update + event.
    let after = read_token_account_amount(ctx.accounts.target_vault)?;
    let received = after
        .checked_sub(before)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    if received == 0 {
        return Ok(());
    }

    // 9. Update pool.reserve_<rwt_side>. Re-borrow pool_state mut now that
    //    the CPI has returned (no live mut borrow conflicts with invoke_signed).
    let reserve_after = {
        let pool = PoolState::load_mut(ctx.accounts.pool_state, ctx.program_id)?;
        if rwt_side == 0 {
            pool.reserve_a = pool
                .reserve_a
                .checked_add(received)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_a
        } else {
            pool.reserve_b = pool
                .reserve_b
                .checked_add(received)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_b
        }
    };

    // 10. Event.
    let mut pool_addr = [0u8; 32];
    pool_addr.copy_from_slice(ctx.accounts.pool_state.address().as_ref());
    let mut ot_mint_arr = [0u8; 32];
    ot_mint_arr.copy_from_slice(ctx.accounts.ot_mint.address().as_ref());

    let clock = Clock::get()?;
    emit!(CompoundYieldExecuted {
        pool: pool_addr,
        ot_mint: ot_mint_arr,
        rwt_claimed: received,
        rwt_side,
        reserve_after,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Pool yield compounded");
    Ok(())
}
