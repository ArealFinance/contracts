use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::{LiquidityRemoved, LpFeesClaimed};
use crate::state::*;
use crate::amm::calculate_remove_amounts;
use crate::instructions::claim_lp_fees::compute_claimable;
use crate::validation::*;
use crate::concentrated;

#[derive(Accounts)]
pub struct RemoveLiquidity<'info> {
    #[account(signer)]
    pub provider: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,

    #[account(mut)]
    pub lp_position: &'info AccountView,

    // Provider's token accounts (receive withdrawn tokens)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub provider_token_a: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub provider_token_b: &'info AccountView,

    // Pool vaults
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_a: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_b: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

/// Decoupled view of `RemoveLiquidity` accounts used by
/// `remove_liquidity_internal`.
///
/// Allows reuse of the core remove-liquidity logic from any handler whose
/// accounts can project into this shape (user-signed `remove_liquidity` or
/// PDA-signed alternative callers added in later layers).
pub(crate) struct RemoveLiquidityAccountsView<'info> {
    pub authority: &'info AccountView,
    pub pool_state: &'info AccountView,
    pub lp_position: &'info AccountView,
    pub provider_token_a: &'info AccountView,
    pub provider_token_b: &'info AccountView,
    pub vault_a: &'info AccountView,
    pub vault_b: &'info AccountView,
}

impl<'info> RemoveLiquidity<'info> {
    pub(crate) fn view(&self) -> RemoveLiquidityAccountsView<'info> {
        RemoveLiquidityAccountsView {
            authority: self.provider,
            pool_state: self.pool_state,
            lp_position: self.lp_position,
            provider_token_a: self.provider_token_a,
            provider_token_b: self.provider_token_b,
            vault_a: self.vault_a,
            vault_b: self.vault_b,
        }
    }
}

pub fn handler(
    ctx: Context<RemoveLiquidity>,
    shares_to_burn: u128,
) -> Result<()> {
    remove_liquidity_internal(
        &ctx.accounts.view(),
        ctx.remaining_accounts,
        ctx.program_id,
        shares_to_burn,
        None,
    )
}

/// Core remove-liquidity logic — usable by user-signed `remove_liquidity` and
/// by PDA-signed alternative callers (e.g. future layers passing
/// `Some(&[Signer])`).
///
/// Internal helper — do NOT use as instruction entrypoint. Caller must
/// validate access control before invoking.
///
/// `_authority_signer_seeds`:
///   - Reserved for interface uniformity with other DEX `_internal` helpers.
///     `remove_liquidity_internal` currently performs only pool-PDA-signed
///     vault → recipient transfers and a direct lamport adjustment when
///     closing the LP position; no inbound authority-signed CPI is required.
///   - Both `None` (user-signed handler) and `Some(seeds)` (PDA-signed caller)
///     produce identical CPI behaviour today. Threaded for future-proofing.
pub(crate) fn remove_liquidity_internal<'info>(
    accounts: &RemoveLiquidityAccountsView<'info>,
    remaining_accounts: &'info [AccountView],
    program_id: &Address,
    shares_to_burn: u128,
    _authority_signer_seeds: Option<&[Signer]>,
) -> Result<()> {
    let pool = PoolState::load_mut(accounts.pool_state, program_id)?;

    // NOTE: remove_liquidity does NOT check pool.is_active — LPs can always exit (by design)

    if shares_to_burn == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount));
    }

    // SECURITY: Validate vaults match pool state
    validate_vault(accounts.vault_a, &pool.vault_a)?;
    validate_vault(accounts.vault_b, &pool.vault_b)?;

    // Layer 9 D30 — snapshot pool's cumulative-fee-per-share BEFORE any state
    // mutation. The auto-claim below realises pending fees against the
    // position's existing share count; without this fix, a full-close would
    // permanently forfeit unclaimed accumulated fees because the LpPosition
    // is closed before `claim_lp_fees` could be called against it. Partial
    // close is also covered: skipping auto-claim there would let the
    // remaining shares inherit pending fees pro-rata while the removed
    // shares forfeit them, creating an arbitrage.
    let cumulative_a = pool.cumulative_fees_per_share_a;
    let cumulative_b = pool.cumulative_fees_per_share_b;

    // --- Validate LP position ---
    let lp = LpPosition::load_mut(accounts.lp_position, program_id)?;
    let provider_key = pubkey_bytes(accounts.authority);

    if lp.owner != provider_key {
        return Err(ProgramError::from(DexError::Unauthorized));
    }
    // SECURITY: Verify LP position belongs to THIS pool (prevents cross-pool drain)
    let pool_key_check = pubkey_bytes(accounts.pool_state);
    if lp.pool != pool_key_check {
        return Err(ProgramError::from(DexError::InvalidVault));
    }
    if lp.shares < shares_to_burn {
        return Err(ProgramError::from(DexError::InsufficientShares));
    }

    // Layer 9 D30 — compute auto-claim payout against the share count
    // BEFORE reduction. `lp.shares >= shares_to_burn > 0` is guaranteed by
    // the InsufficientShares guard above, so the multiplication input is
    // strictly positive (or zero only if both delta and shares are zero —
    // a no-op).
    let delta_a_q64 = cumulative_a
        .checked_sub(lp.fees_claimed_per_share_a)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    let delta_b_q64 = cumulative_b
        .checked_sub(lp.fees_claimed_per_share_b)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    let auto_claim_a = compute_claimable(delta_a_q64, lp.shares)?;
    let auto_claim_b = compute_claimable(delta_b_q64, lp.shares)?;
    // Advance the snapshot so subsequent claim_lp_fees / partial-close calls
    // observe zero delta on the remaining shares. Effects-before-CPI; the
    // vault-side transfer below moves the realised value to the provider.
    lp.fees_claimed_per_share_a = cumulative_a;
    lp.fees_claimed_per_share_b = cumulative_b;

    // --- Calculate proportional withdrawal ---
    let (amount_a, amount_b) = calculate_remove_amounts(
        shares_to_burn, pool.reserve_a, pool.reserve_b, pool.total_lp_shares,
    )?;

    // --- For concentrated pools: proportionally reduce bins (MANDATORY) ---
    if pool.pool_type == POOL_TYPE_CONCENTRATED {
        if remaining_accounts.is_empty() {
            return Err(ProgramError::from(DexError::InvalidBinRange));
        }
        let pool_key_for_bin = pubkey_bytes(accounts.pool_state);
        let (expected_bin_pda, _) = arlex_lang::find_program_address(
            &[b"bins", pool_key_for_bin.as_ref()],
            program_id,
        );
        if remaining_accounts[0].address().as_ref() != expected_bin_pda.as_ref() {
            return Err(ProgramError::InvalidSeeds);
        }
        let bin_array = BinArray::load_mut(&remaining_accounts[0], program_id)?;
        concentrated::proportional_bin_remove(
            bin_array,
            shares_to_burn,
            pool.total_lp_shares,
        )?;
    }

    // --- Effects: update state BEFORE CPIs ---
    pool.reserve_a = pool.reserve_a.checked_sub(amount_a)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    pool.reserve_b = pool.reserve_b.checked_sub(amount_b)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    pool.total_lp_shares = pool.total_lp_shares.checked_sub(shares_to_burn)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    lp.shares = lp.shares.checked_sub(shares_to_burn)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    let clock = Clock::get()?;
    lp.last_update_ts = clock.unix_timestamp;

    // If shares == 0, close the LpPosition and return rent to provider
    let close_position = lp.shares == 0;

    // --- Interactions: transfer tokens from vaults to provider ---
    let pool_key = pubkey_bytes(accounts.pool_state);
    let bump = [pool.bump];

    if amount_a > 0 {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: accounts.vault_a,
            to: accounts.provider_token_a,
            authority: accounts.pool_state,
            amount: amount_a,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    if amount_b > 0 {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: accounts.vault_b,
            to: accounts.provider_token_b,
            authority: accounts.pool_state,
            amount: amount_b,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // Layer 9 D30 — auto-claim outbound transfers. Pool-PDA-signed Transfer
    // vault → provider ATA on each side that accrued pending fees, sized
    // against the pre-reduction share count (see snapshot above). MUST run
    // before the close_position lamport sweep because both legs target the
    // same `provider_token_<side>` ATAs and the close path can drain
    // `lp_position` of state we'd otherwise still be inspecting.
    if auto_claim_a > 0 {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: accounts.vault_a,
            to: accounts.provider_token_a,
            authority: accounts.pool_state,
            amount: auto_claim_a,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }
    if auto_claim_b > 0 {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: accounts.vault_b,
            to: accounts.provider_token_b,
            authority: accounts.pool_state,
            amount: auto_claim_b,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }
    if auto_claim_a > 0 || auto_claim_b > 0 {
        emit!(LpFeesClaimed {
            recipient: provider_key,
            pool: pool_key,
            claimable_a: auto_claim_a,
            claimable_b: auto_claim_b,
            timestamp: clock.unix_timestamp,
        });
    }

    // Close LpPosition if fully withdrawn (rent back to provider)
    if close_position {
        // Transfer lamports from LpPosition to provider
        let lp_lamports = accounts.lp_position.lamports();
        let provider_lamports = accounts.authority.lamports();
        accounts.authority.set_lamports(
            provider_lamports.checked_add(lp_lamports)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
        );
        accounts.lp_position.set_lamports(0);
        // Zero owner + data_len + lamports to close the account
        accounts.lp_position.close()?;
    }

    // --- Emit event ---
    emit!(LiquidityRemoved {
        pool: pool_key,
        provider: provider_key,
        amount_a,
        amount_b,
        shares_burned: shares_to_burn,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Layer 9 D30 — pure-Rust math + invariant tests for the
    //! `remove_liquidity` auto-claim integration.
    //!
    //! Pre-D28, `remove_liquidity` returning proportional reserves
    //! automatically returned the LP's compounded fees because the LP-fee
    //! model was auto-compound (fee_lp grew the reserves). D28 split the
    //! semantic: fee_lp stays in the vault but is tracked via the per-
    //! share accumulator, NOT compounded into reserves. Without an
    //! auto-claim on close, an LP doing a full `remove_liquidity` would
    //! permanently forfeit unclaimed accrued fees because the LpPosition
    //! is closed before `claim_lp_fees` could be called against it.
    //!
    //! This test pins the math: the auto-claim is computed against the
    //! pre-reduction share count, the snapshot is advanced, and a
    //! subsequent claim observes zero delta.
    use super::*;
    use crate::state::{LpPosition, PoolState};

    fn make_pool(cumulative_a: u128, cumulative_b: u128, total_lp_shares: u128) -> PoolState {
        let buf = [0u8; core::mem::size_of::<PoolState>()];
        let mut pool: PoolState = unsafe { core::ptr::read(buf.as_ptr() as *const PoolState) };
        pool.cumulative_fees_per_share_a = cumulative_a;
        pool.cumulative_fees_per_share_b = cumulative_b;
        pool.total_lp_shares = total_lp_shares;
        pool
    }

    fn make_position(
        shares: u128,
        fees_claimed_a: u128,
        fees_claimed_b: u128,
    ) -> LpPosition {
        let buf = [0u8; core::mem::size_of::<LpPosition>()];
        let mut lp: LpPosition =
            unsafe { core::ptr::read(buf.as_ptr() as *const LpPosition) };
        lp.shares = shares;
        lp.fees_claimed_per_share_a = fees_claimed_a;
        lp.fees_claimed_per_share_b = fees_claimed_b;
        lp
    }

    /// D30 — full close auto-claims pending fees against the pre-reduction
    /// share count, then advances the snapshot so a residual partial-close
    /// caller (or post-close re-init) observes zero delta. Reproduces the
    /// effects-step ordering inside `remove_liquidity_internal`: snapshot
    /// pool cumulative → compute claimable on `lp.shares_pre` → advance
    /// position snapshot → reduce shares.
    #[test]
    fn remove_liquidity_full_close_auto_claims_pending_fees() {
        let cumulative_a: u128 = 6u128 << 64;
        let cumulative_b: u128 = 2u128 << 64;
        let pool = make_pool(cumulative_a, cumulative_b, 5_000);

        // Position: 5_000 shares (full pool), zero snapshot ⇒ entire
        // accumulator delta is owed.
        let mut lp = make_position(5_000, 0, 0);
        let shares_pre = lp.shares;

        // Effects step (matches handler).
        let delta_a = pool
            .cumulative_fees_per_share_a
            .checked_sub(lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b = pool
            .cumulative_fees_per_share_b
            .checked_sub(lp.fees_claimed_per_share_b)
            .unwrap();
        let auto_claim_a = compute_claimable(delta_a, shares_pre).unwrap();
        let auto_claim_b = compute_claimable(delta_b, shares_pre).unwrap();
        // (6 << 64) * 5000 >> 64 == 30_000; (2 << 64) * 5000 >> 64 == 10_000.
        assert_eq!(auto_claim_a, 30_000u64);
        assert_eq!(auto_claim_b, 10_000u64);

        // Snapshot advances BEFORE share reduction (matches handler order).
        lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
        lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;

        // Full close: shares go to zero. The LpPosition account would be
        // closed in the on-chain handler — here we just assert the math
        // invariants so the auto-claim path is provably correct against the
        // pre-reduction share count.
        lp.shares = lp.shares.checked_sub(shares_pre).unwrap();
        assert_eq!({ lp.shares }, 0u128);

        // Re-running the delta against the advanced snapshot must observe
        // zero on both sides — auto-claim cannot double-pay even if the
        // handler is re-entered (e.g. partial-close scenarios).
        let delta_a2 = pool
            .cumulative_fees_per_share_a
            .checked_sub(lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b2 = pool
            .cumulative_fees_per_share_b
            .checked_sub(lp.fees_claimed_per_share_b)
            .unwrap();
        assert_eq!(delta_a2, 0u128);
        assert_eq!(delta_b2, 0u128);
    }
}
