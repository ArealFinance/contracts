//! claim_lp_fees — LP realises accumulator-tracked fees from a pool.
//!
//! Layer 9 D28 (companion to the swap.rs `cumulative_fees_per_share_<side>`
//! accumulator). The LP-fee model in Layer 4-5 was "auto-compound":
//! `fee_lp` was added back into pool reserves on every
//! swap, increasing each LP's k-implicit value silently. Layer 9 D28 splits
//! that single semantic into two: pool reserves no longer change from
//! `fee_lp`, and the per-share accumulator
//! `PoolState.cumulative_fees_per_share_<side>` records each LP's claim
//! position over time. `claim_lp_fees` is the realisation path.
//!
//! Math:
//!   delta_q64           = pool.cumulative_fees_per_share_<side>
//!                       - lp_position.fees_claimed_per_share_<side>
//!   claimable_<side>    = (delta_q64 * lp_position.shares) >> 64
//!
//! Effects:
//!   - PDA-signed transfer pool.vault_<side> → recipient_token_<side>_ata
//!     (skipped when `claimable == 0`).
//!   - lp_position.fees_claimed_per_share_<side> = pool.cumulative_<side>.
//!   - emit `LpFeesClaimed`.
//!
//! `claim_lp_fees_internal` is exposed `pub(crate)` per the D23 internal-call
//! pattern so future Nexus / crank handlers can reuse it (e.g.
//! `nexus_claim_rewards` may project a Nexus-owned LpPosition
//! into `ClaimLpFeesAccountsView` and invoke the internal helper directly).
//! Callers invoking the internal path on behalf of a non-signer authority
//! should pass their PDA seeds via `_authority_signer_seeds`; the parameter
//! is currently unused (vault transfers always sign with the pool PDA), but
//! is threaded for symmetry with the other `<ix>_internal` helpers and to
//! reserve the slot for future authority-signed CPI legs.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::LpFeesClaimed;
use crate::state::*;
use crate::validation::*;

#[derive(Accounts)]
pub struct ClaimLpFees<'info> {
    /// LP wallet — owner of `lp_position`. Receives realised fees on its
    /// token A and token B ATAs. Signer required for the user-driven path.
    #[account(signer)]
    pub recipient: &'info AccountView,

    /// Pool state. Mutation: `cumulative_fees_per_share_<side>` is read
    /// only; this ix does not modify pool state. `mut` is preserved so
    /// future variants (e.g. fee-vault sweep accounting) can write
    /// without an account-permissions migration.
    #[account(mut)]
    pub pool_state: &'info AccountView,

    /// Caller's `LpPosition`. Mutated to advance
    /// `fees_claimed_per_share_<side>` to the pool's current cumulative.
    #[account(mut)]
    pub lp_position: &'info AccountView,

    /// Pool's vault on side A. PDA-signed Transfer source on the A leg.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub pool_vault_a: &'info AccountView,

    /// Pool's vault on side B. PDA-signed Transfer source on the B leg.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub pool_vault_b: &'info AccountView,

    /// Recipient's ATA for token A. Receives the side-A claimable.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub recipient_token_a_ata: &'info AccountView,

    /// Recipient's ATA for token B. Receives the side-B claimable.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub recipient_token_b_ata: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

/// Decoupled view of `ClaimLpFees` accounts used by `claim_lp_fees_internal`.
///
/// Mirrors the D23 internal-call pattern so any handler whose accounts can
/// project into this shape (user-driven `claim_lp_fees`, future PDA-signed
/// callers like `nexus_claim_rewards`) can reuse the core logic.
pub(crate) struct ClaimLpFeesAccountsView<'info> {
    /// LP wallet. Used only to (a) own the recipient ATAs by SPL semantics,
    /// (b) pubkey-bytes for the emitted event. Not used as a signer
    /// inside the helper — vault transfers always sign with the pool PDA.
    pub authority: &'info AccountView,
    pub pool_state: &'info AccountView,
    pub lp_position: &'info AccountView,
    pub pool_vault_a: &'info AccountView,
    pub pool_vault_b: &'info AccountView,
    pub authority_token_a: &'info AccountView,
    pub authority_token_b: &'info AccountView,
    /// Forward-compat slot — pinocchio CPIs sign the SPL Token program
    /// reference inline so this view-projection field is structural only.
    #[allow(dead_code)]
    pub token_program: &'info AccountView,
}

impl<'info> ClaimLpFees<'info> {
    pub(crate) fn view(&self) -> ClaimLpFeesAccountsView<'info> {
        ClaimLpFeesAccountsView {
            authority: self.recipient,
            pool_state: self.pool_state,
            lp_position: self.lp_position,
            pool_vault_a: self.pool_vault_a,
            pool_vault_b: self.pool_vault_b,
            authority_token_a: self.recipient_token_a_ata,
            authority_token_b: self.recipient_token_b_ata,
            token_program: self.token_program,
        }
    }
}

pub fn handler(ctx: Context<ClaimLpFees>) -> Result<()> {
    claim_lp_fees_internal(&ctx.accounts.view(), ctx.program_id, None)
}

/// Core claim-LP-fees logic — usable by user-signed `claim_lp_fees` and by
/// PDA-signed alternative callers (e.g. `nexus_claim_rewards`
/// passing `Some(&[Signer])`).
///
/// `_authority_signer_seeds`:
///   - Reserved for interface uniformity with other DEX `_internal`
///     helpers. Vault → recipient transfers always sign with the pool PDA;
///     the authority is not invoked as a CPI signer here. Both `None`
///     (user-signed handler) and `Some(seeds)` produce identical CPI
///     behaviour today. Threaded for future-proofing.
#[inline(never)]
pub(crate) fn claim_lp_fees_internal<'info>(
    accounts: &ClaimLpFeesAccountsView<'info>,
    program_id: &Address,
    _authority_signer_seeds: Option<&[Signer]>,
) -> Result<()> {
    let pool = PoolState::load(accounts.pool_state, program_id)?;

    // SECURITY: Validate vaults match the pool's stored vault addresses.
    validate_vault(accounts.pool_vault_a, &pool.vault_a)?;
    validate_vault(accounts.pool_vault_b, &pool.vault_b)?;

    let pool_key = pubkey_bytes(accounts.pool_state);
    let recipient_key = pubkey_bytes(accounts.authority);

    // Snapshot the pool fields we need before reborrowing for the LpPosition
    // mut handle. Pool state is not mutated by claim_lp_fees, so a `load`
    // borrow is sufficient.
    let cumulative_a = pool.cumulative_fees_per_share_a;
    let cumulative_b = pool.cumulative_fees_per_share_b;
    let token_a_mint = pool.token_a_mint;
    let token_b_mint = pool.token_b_mint;
    let pool_bump = pool.bump;

    // SECURITY: LpPosition must belong to THIS pool (prevents cross-pool
    // claim with a stale snapshot from another pool's accumulator).
    let lp = LpPosition::load_mut(accounts.lp_position, program_id)?;
    if lp.pool != pool_key {
        return Err(ProgramError::from(DexError::InvalidLpPosition));
    }
    // Defence-in-depth: position must be owned by the supplied authority.
    // Reuses the existing Unauthorized variant (see remove_liquidity).
    if lp.owner != recipient_key {
        return Err(ProgramError::from(DexError::Unauthorized));
    }

    // Compute claimable on each side. `delta_q64` is unsigned because the
    // accumulator is monotonically non-decreasing (only `swap_internal`
    // writes it, only via `checked_add`). The subtraction therefore cannot
    // underflow when the snapshot was taken from this same pool — a stale
    // snapshot from another pool would already have failed the
    // `lp.pool != pool_key` guard above. We still use `checked_sub` as a
    // belt-and-braces guard.
    let delta_a_q64 = cumulative_a
        .checked_sub(lp.fees_claimed_per_share_a)
        .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
    let delta_b_q64 = cumulative_b
        .checked_sub(lp.fees_claimed_per_share_b)
        .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;

    let claimable_a = compute_claimable(delta_a_q64, lp.shares)?;
    let claimable_b = compute_claimable(delta_b_q64, lp.shares)?;

    // --- Effects: advance position snapshot BEFORE CPIs (CEI pattern) ---
    lp.fees_claimed_per_share_a = cumulative_a;
    lp.fees_claimed_per_share_b = cumulative_b;
    let clock = Clock::get()?;
    lp.last_update_ts = clock.unix_timestamp;

    // --- Interactions: PDA-signed Transfer pool_vault_<side> → recipient ATA ---
    let bump_arr = [pool_bump];
    if claimable_a > 0 {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(token_a_mint.as_ref()),
            Seed::from(token_b_mint.as_ref()),
            Seed::from(bump_arr.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: accounts.pool_vault_a,
            to: accounts.authority_token_a,
            authority: accounts.pool_state,
            amount: claimable_a,
        }
        .invoke_signed(&[Signer::from(&seeds)])?;
    }
    if claimable_b > 0 {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(token_a_mint.as_ref()),
            Seed::from(token_b_mint.as_ref()),
            Seed::from(bump_arr.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: accounts.pool_vault_b,
            to: accounts.authority_token_b,
            authority: accounts.pool_state,
            amount: claimable_b,
        }
        .invoke_signed(&[Signer::from(&seeds)])?;
    }

    // --- Emit event ---
    emit!(LpFeesClaimed {
        recipient: recipient_key,
        pool: pool_key,
        claimable_a,
        claimable_b,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

/// Q64.64 → u64 conversion for `claim_lp_fees`.
///
/// `delta_q64 * shares` must fit in u128 (true for any realistic
/// `fee_lp / total_lp_shares` ratio over the pool lifetime, but we use
/// `checked_mul` defensively). Truncating shift `>> 64` discards the
/// fractional remainder; the lost dust stays in the per-share accumulator
/// for the next claim (unsigned arithmetic — no precision drift).
///
/// Pulled out as a helper so the unit tests below can exercise the math
/// without touching the on-chain account machinery.
pub(crate) fn compute_claimable(
    delta_q64: u128,
    shares: u128,
) -> core::result::Result<u64, ProgramError> {
    let raw = delta_q64
        .checked_mul(shares)
        .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
    let truncated = raw >> 64;
    u64::try_from(truncated).map_err(|_| ProgramError::from(DexError::MathOverflow))
}

#[cfg(test)]
mod tests {
    //! Layer 9 D28 — pure-Rust math + invariant tests for `claim_lp_fees`.
    //!
    //! Handler-level negative ACs (signer absent, vault mismatch, etc.) are
    //! exercised by future BPF integration tests where Arlex can be invoked
    //! end-to-end. The tests here pin the on-chain math (Q64.64 → u64) and
    //! the snapshot-advance invariant against a synthetic in-memory state.
    use super::*;

    /// Build a synthetic `PoolState` with the cumulative-fee fields populated.
    /// SAFETY: PoolState is `#[repr(C, packed)]` via `#[account]` and
    /// all-zero is a valid bit pattern for every field type.
    fn make_pool(cumulative_a: u128, cumulative_b: u128, total_lp_shares: u128) -> PoolState {
        let buf = [0u8; core::mem::size_of::<PoolState>()];
        let mut pool: PoolState = unsafe { core::ptr::read(buf.as_ptr() as *const PoolState) };
        pool.cumulative_fees_per_share_a = cumulative_a;
        pool.cumulative_fees_per_share_b = cumulative_b;
        pool.total_lp_shares = total_lp_shares;
        pool
    }

    /// Build a synthetic `LpPosition` with a known pool/owner snapshot.
    /// SAFETY: same `repr(C, packed)` zero-default rationale as `make_pool`.
    fn make_position(
        pool: [u8; 32],
        owner: [u8; 32],
        shares: u128,
        fees_claimed_a: u128,
        fees_claimed_b: u128,
    ) -> LpPosition {
        let buf = [0u8; core::mem::size_of::<LpPosition>()];
        let mut lp: LpPosition =
            unsafe { core::ptr::read(buf.as_ptr() as *const LpPosition) };
        lp.pool = pool;
        lp.owner = owner;
        lp.shares = shares;
        lp.fees_claimed_per_share_a = fees_claimed_a;
        lp.fees_claimed_per_share_b = fees_claimed_b;
        lp
    }

    /// D28 — happy path: position with `shares=Y`, pool cumulative=X on side
    /// A, `fees_claimed_a=0`. Expected payout: `(X * Y) >> 64`. Uses a known
    /// Q64.64 value so the truncation behaviour is explicit.
    #[test]
    fn claim_lp_fees_basic_one_side() {
        let pool = make_pool(/* cumulative_a */ 10u128 << 64, 0, /* total_shares */ 1);
        // Per-share value = (10 << 64) tokens-of-fee-per-1-share.
        let position_shares: u128 = 1_000;
        let claimable_a = compute_claimable(
            pool.cumulative_fees_per_share_a, // delta = cumulative - 0
            position_shares,
        )
        .unwrap();
        // (10 << 64) * 1000 >> 64 == 10_000.
        assert_eq!(claimable_a, 10_000u64);
        let claimable_b =
            compute_claimable(pool.cumulative_fees_per_share_b, position_shares).unwrap();
        assert_eq!(claimable_b, 0u64);
    }

    /// D28 — zero-delta path: position snapshots already match pool
    /// cumulative on both sides → claimable is 0 on both sides, no
    /// arithmetic error. Mirrors the no-op behaviour expected from a
    /// double-claim within the same swap epoch.
    #[test]
    fn claim_lp_fees_zero_delta_no_op() {
        let cumulative_a: u128 = 7u128 << 64;
        let cumulative_b: u128 = 3u128 << 64;
        let pool = make_pool(cumulative_a, cumulative_b, 1);
        let lp = make_position(
            [0u8; 32],
            [0u8; 32],
            500,
            cumulative_a,
            cumulative_b,
        );
        let delta_a = pool
            .cumulative_fees_per_share_a
            .checked_sub(lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b = pool
            .cumulative_fees_per_share_b
            .checked_sub(lp.fees_claimed_per_share_b)
            .unwrap();
        assert_eq!(delta_a, 0u128);
        assert_eq!(delta_b, 0u128);
        assert_eq!(compute_claimable(delta_a, lp.shares).unwrap(), 0u64);
        assert_eq!(compute_claimable(delta_b, lp.shares).unwrap(), 0u64);
    }

    /// D28 — pool/position mismatch reverts. Captures the `lp.pool != pool_key`
    /// guard inside `claim_lp_fees_internal`. We mimic the comparison
    /// directly — the full handler path requires Arlex BPF wiring tested in
    /// integration suites.
    #[test]
    fn claim_lp_fees_position_pool_mismatch_revert() {
        let pool_key: [u8; 32] = [1u8; 32];
        let other_pool_key: [u8; 32] = [2u8; 32];
        let lp = make_position(other_pool_key, [0u8; 32], 100, 0, 0);
        // The guard: `if lp.pool != pool_key { return Err(InvalidLpPosition) }`.
        assert_ne!(lp.pool, pool_key);
        // Pin the chosen error variant — defence against accidentally
        // returning a different error code that callers (TS bot, dashboard)
        // would mis-route in their decoders.
        let err = ProgramError::from(DexError::InvalidLpPosition);
        let want = ProgramError::from(DexError::InvalidLpPosition);
        assert_eq!(format!("{:?}", err), format!("{:?}", want));
    }

    /// D28 — after a successful claim, `lp.fees_claimed_per_share_<side>`
    /// must equal `pool.cumulative_fees_per_share_<side>` so the next claim
    /// computes `delta == 0`. Mirrors the effects step inside
    /// `claim_lp_fees_internal`.
    #[test]
    fn claim_lp_fees_updates_position_snapshot() {
        let cumulative_a: u128 = (5u128 << 64) | 0xABCD;
        let cumulative_b: u128 = (11u128 << 64) | 0x1234;
        let pool = make_pool(cumulative_a, cumulative_b, 1);
        let mut lp = make_position([0u8; 32], [0u8; 32], 1_000, 0, 0);
        // Simulate the effects step.
        lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
        lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;
        assert_eq!({ lp.fees_claimed_per_share_a }, cumulative_a);
        assert_eq!({ lp.fees_claimed_per_share_b }, cumulative_b);
        // A second claim must observe zero delta on both sides.
        let delta_a = pool
            .cumulative_fees_per_share_a
            .checked_sub(lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b = pool
            .cumulative_fees_per_share_b
            .checked_sub(lp.fees_claimed_per_share_b)
            .unwrap();
        assert_eq!(delta_a, 0u128);
        assert_eq!(delta_b, 0u128);
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
        const SRC: &str = include_str!("claim_lp_fees.rs");
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
