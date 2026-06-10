use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::{LiquidityAdded, LpFeesClaimed};
use crate::state::{DexConfig, PoolState, LpPosition};
use crate::amm::calculate_lp_shares;
use crate::instructions::claim_lp_fees::compute_claimable;
use crate::validation::*;

#[derive(Accounts)]
pub struct AddLiquidity<'info> {
    #[account(signer)]
    pub provider: &'info AccountView,

    #[account(mut, signer)]
    pub payer: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    // NOTE: pool_state PDA verified via PoolState::load_mut (checks discriminator + owner).
    // Full seed validation is done at create_pool time. Here we rely on program-ownership.
    #[account(mut)]
    pub pool_state: &'info AccountView,

    // LpPosition PDA: ["lp", pool_state, provider]
    #[account(mut)]
    pub lp_position: &'info AccountView,

    // Provider's token accounts
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

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

/// Decoupled view of `AddLiquidity` accounts used by `add_liquidity_internal`.
///
/// Allows reuse of the core add-liquidity logic from any handler whose accounts
/// can project into this shape (user-signed `add_liquidity` or PDA-signed
/// alternative callers added in later layers).
pub(crate) struct AddLiquidityAccountsView<'info> {
    pub authority: &'info AccountView,
    pub payer: &'info AccountView,
    pub dex_config: &'info AccountView,
    pub pool_state: &'info AccountView,
    pub lp_position: &'info AccountView,
    pub provider_token_a: &'info AccountView,
    pub provider_token_b: &'info AccountView,
    pub vault_a: &'info AccountView,
    pub vault_b: &'info AccountView,
}

impl<'info> AddLiquidity<'info> {
    pub(crate) fn view(&self) -> AddLiquidityAccountsView<'info> {
        AddLiquidityAccountsView {
            authority: self.provider,
            payer: self.payer,
            dex_config: self.dex_config,
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
    ctx: Context<AddLiquidity>,
    amount_a: u64,
    amount_b: u64,
    min_shares: u128,
) -> Result<()> {
    // CP-5 — Master (Monotonic Ladder) pools reject user LP entirely.
    // The active bid wall is sized by the Pool Rebalancer (CP-7
    // grow_liquidity / compress_liquidity); the Liquidity Nexus is the
    // only liquidity source on the bid side.
    // See docs/changelog/2026-04-17-monotonic-ladder.mdx:97 and
    // docs/contracts/native-dex.mdx §83.
    //
    // The check lives at the user-signed entrypoint (NOT inside
    // `add_liquidity_internal`) so the shared internal helper remains
    // reachable from `nexus_add_liquidity`, which is the SOLE callsite
    // permitted to deposit into master pools and runs through
    // `add_liquidity_internal` with the Nexus PDA as `authority`.
    {
        let pool = PoolState::load(ctx.accounts.pool_state, ctx.program_id)?;
        enforce_user_lp_allowed(pool.pool_type)?;
    }

    add_liquidity_internal(
        &ctx.accounts.view(),
        ctx.remaining_accounts,
        ctx.program_id,
        amount_a,
        amount_b,
        min_shares,
        None,
    )
}

/// CP-5 — guard helper enforcing the rule that master (Monotonic
/// Ladder) pools reject user LP. Used by user-signed `add_liquidity`
/// and `zap_liquidity` so the rejection surface is identical and
/// unit-testable without fabricating PDA-backed `AccountView`s.
///
/// Deliberately NOT called from `add_liquidity_internal` because that
/// shared helper also services `nexus_add_liquidity`, which IS allowed
/// to deposit into master pools (and would otherwise be blocked here).
///
/// See docs/changelog/2026-04-17-monotonic-ladder.mdx:97 and
/// docs/contracts/native-dex.mdx §83 for the architectural rationale.
#[inline]
pub(crate) fn enforce_user_lp_allowed(pool_type: u8) -> Result<()> {
    if pool_type == POOL_TYPE_CONCENTRATED {
        return Err(ProgramError::from(DexError::MasterPoolUserLpDisabled));
    }
    Ok(())
}

/// Layer 10 R46 — auto-claim outbound LP-fee transfers extracted into a
/// `#[inline(never)]` child frame so the [Seed; 4] arrays + Signer wrappers
/// live in their own stack slot rather than unioning with the caller's
/// fresh-init Signer locals (parent handlers — `add_liquidity_internal` and
/// `zap_liquidity_internal` — already allocate a CreateAccount Signer plus
/// inbound provider→vault Transfer Signers, the additional pool-PDA seeds
/// for the outbound side were pushing the dispatcher's worst-case union
/// frame ~8B over the SBF 4096-byte budget; observed at runtime as
/// `Access violation in stack frame 1 at address 0x200001ff8 of size 8`
/// on `add_liquidity` during Phase F + Phase L of the bootstrap chain
/// — Layer 10 SD-32 closure, 2026-04-29).
///
/// Each side reuses the same canonical pool seeds
/// (`["pool", token_a_mint, token_b_mint, bump]`); the `pool_state` is the
/// vault authority. Skipped per side when its claimable is zero (fresh
/// position or snapshot already aligned). Caller is responsible for
/// emitting `LpFeesClaimed` so the event keeps `clock`, `provider_key`,
/// and `pool_key` in the parent's lexical scope.
/// R46 escalation child helper — fresh-init LpPosition CreateAccount.
///
/// Moved out of `add_liquidity_internal` so the [Seed; 4] + Signer +
/// lamports + rent + lp_pda + lp_bump locals (~256B union worst-case) live
/// in a child stack frame instead of sharing the parent's match-arm budget
/// with the auto-claim outbound branch. The caller still gets `lp_bump` to
/// populate `lp.bump` on the freshly-initialised LpPosition.
#[inline(never)]
pub(crate) fn create_lp_position_account<'info>(
    payer: &'info AccountView,
    lp_position: &'info AccountView,
    program_id: &Address,
    pool_key: &[u8; 32],
    provider_key: &[u8; 32],
) -> Result<u8> {
    let (lp_pda, lp_bump) = arlex_lang::find_program_address(
        &[b"lp", pool_key.as_ref(), provider_key.as_ref()],
        program_id,
    );
    if lp_position.address().as_ref() != lp_pda.as_ref() {
        return Err(ProgramError::InvalidSeeds);
    }

    let rent = pinocchio::sysvars::rent::Rent::get()?;
    let lamports = rent.try_minimum_balance(LpPosition::SPACE)?;
    let bump_arr = [lp_bump];
    arlex_lang::system::instructions::CreateAccount {
        from: payer,
        to: lp_position,
        lamports,
        space: LpPosition::SPACE as u64,
        owner: program_id,
    }.invoke_signed(&[Signer::from(&[
        Seed::from(b"lp" as &[u8]),
        Seed::from(pool_key.as_ref()),
        Seed::from(provider_key.as_ref()),
        Seed::from(bump_arr.as_ref()),
    ])])?;

    Ok(lp_bump)
}

#[inline(never)]
pub(crate) fn auto_claim_lp_fees<'info>(
    vault_a: &'info AccountView,
    vault_b: &'info AccountView,
    provider_token_a: &'info AccountView,
    provider_token_b: &'info AccountView,
    pool_state: &'info AccountView,
    token_a_mint: &[u8; 32],
    token_b_mint: &[u8; 32],
    pool_bump: u8,
    auto_claim_a: u64,
    auto_claim_b: u64,
) -> Result<()> {
    if auto_claim_a > 0 {
        let bump_arr = [pool_bump];
        let seeds_a = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(token_a_mint.as_ref()),
            Seed::from(token_b_mint.as_ref()),
            Seed::from(bump_arr.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: vault_a,
            to: provider_token_a,
            authority: pool_state,
            amount: auto_claim_a,
        }.invoke_signed(&[Signer::from(&seeds_a)])?;
    }
    if auto_claim_b > 0 {
        let bump_arr = [pool_bump];
        let seeds_b = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(token_a_mint.as_ref()),
            Seed::from(token_b_mint.as_ref()),
            Seed::from(bump_arr.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: vault_b,
            to: provider_token_b,
            authority: pool_state,
            amount: auto_claim_b,
        }.invoke_signed(&[Signer::from(&seeds_b)])?;
    }
    Ok(())
}

/// Core add-liquidity logic — usable by user-signed `add_liquidity` and by
/// PDA-signed alternative callers (e.g. future layers passing
/// `Some(&[Signer])`).
///
/// Internal helper — do NOT use as instruction entrypoint. Caller must
/// validate access control before invoking.
///
/// `authority_signer_seeds`:
///   - `None` — authority is a transaction signer (user-signed path); the
///     inbound `provider_token_* -> vault_*` transfers are invoked without
///     seeds.
///   - `Some(seeds)` — authority is a PDA; the inbound transfers are invoked
///     with the supplied signer seeds.
#[inline(never)]
pub(crate) fn add_liquidity_internal<'info>(
    accounts: &AddLiquidityAccountsView<'info>,
    remaining_accounts: &'info [AccountView],
    program_id: &Address,
    amount_a: u64,
    amount_b: u64,
    min_shares: u128,
    authority_signer_seeds: Option<&[Signer]>,
) -> Result<()> {
    // arlex v0.1.2: PoolState::load_mut returns an RAII guard whose Drop releases
    // the borrow flag. The pool_state PDA is passed into the pool-PDA-signed
    // auto_claim_lp_fees CPI below as the signing authority, which the checked
    // invoke rejects while the account is still mutably borrowed. Scope the pool
    // (and LpPosition) mutations to a block that copies out every value the CPIs
    // need (deposit amounts, shares, keys, pool bump/mints, auto-claim payouts);
    // both guards drop at the end of the block, releasing the flag before the
    // interactions. (Pattern C/D from the migration.)
    let deposit_a: u64;
    let deposit_b: u64;
    let shares: u128;
    let provider_key: [u8; 32];
    let pool_key: [u8; 32];
    let pool_bump: u8;
    let pool_token_a_mint: [u8; 32];
    let pool_token_b_mint: [u8; 32];
    let mut auto_claim_a: u64 = 0;
    let mut auto_claim_b: u64 = 0;
    let clock = Clock::get()?;
    {
    let config = DexConfig::load(accounts.dex_config, program_id)?;
    let mut pool = PoolState::load_mut(accounts.pool_state, program_id)?;

    // --- Checks ---
    if !config.is_active {
        return Err(ProgramError::from(DexError::DexPaused));
    }
    if !pool.is_active {
        return Err(ProgramError::from(DexError::PoolNotActive));
    }
    // CP-5 — Master-pool user-LP rejection lives at the user-signed
    // entrypoints (`add_liquidity::handler`, `zap_liquidity::handler`)
    // via `enforce_user_lp_allowed`, NOT here. This shared helper also
    // services `nexus_add_liquidity`, which IS allowed to deposit into
    // master pools — gating the entire helper would block the Nexus
    // path and break the Monotonic Ladder bid surface.
    if amount_a == 0 || amount_b == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount));
    }

    // SECURITY: Validate vaults match pool state
    validate_vault(accounts.vault_a, &pool.vault_a)?;
    validate_vault(accounts.vault_b, &pool.vault_b)?;

    // Layer 9 D29 — snapshot pool fields needed for fee-snapshot init / auto-claim.
    // Read here while we hold the pool mut handle; values are immutable for the
    // remainder of the handler (cumulative_fees_per_share_<side> only mutates inside
    // swap_internal, never inside add_liquidity_internal). Pool PDA fields (bump,
    // mints) are needed downstream for the pool-PDA-signed outbound auto-claim
    // transfer when an existing position has pending fees.
    let cumulative_a = pool.cumulative_fees_per_share_a;
    let cumulative_b = pool.cumulative_fees_per_share_b;
    pool_bump = pool.bump;
    pool_token_a_mint = pool.token_a_mint;
    pool_token_b_mint = pool.token_b_mint;

    // --- Calculate LP shares ---
    let is_first = pool.total_lp_shares == 0;
    (deposit_a, deposit_b, shares) = if is_first {
        // First LP: use both amounts directly
        let shares = calculate_lp_shares(amount_a, amount_b, 0, 0, 0, true)?;
        (amount_a, amount_b, shares)
    } else {
        // Subsequent LP: proportional deposit
        // Calculate max balanced deposit based on pool ratio
        // deposit_b_needed = amount_a * reserve_b / reserve_a
        let b_needed = arlex_lang::math::mul_div_u64(amount_a, pool.reserve_b, pool.reserve_a)
            .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;

        let (dep_a, dep_b) = if b_needed <= amount_b {
            // amount_a is the limiting factor
            (amount_a, b_needed)
        } else {
            // amount_b is the limiting factor
            let a_needed = arlex_lang::math::mul_div_u64(amount_b, pool.reserve_a, pool.reserve_b)
                .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
            (a_needed, amount_b)
        };

        let shares = calculate_lp_shares(dep_a, dep_b, pool.reserve_a, pool.reserve_b, pool.total_lp_shares, false)?;
        (dep_a, dep_b, shares)
    };

    if shares == 0 {
        return Err(ProgramError::from(DexError::ZeroOutput));
    }

    // Slippage protection via min_shares
    if shares < min_shares {
        return Err(ProgramError::from(DexError::SlippageExceeded));
    }

    // CP-5 — concentrated bin distribution removed. `pool.pool_type ==
    // POOL_TYPE_CONCENTRATED` is rejected up-front by the
    // `MasterPoolUserLpDisabled` guard above; only StandardCurve pools
    // ever reach this point, so there is no bin-side bookkeeping to do.

    // --- Effects: update pool state BEFORE CPIs ---
    pool.reserve_a = pool.reserve_a.checked_add(deposit_a)
        .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
    pool.reserve_b = pool.reserve_b.checked_add(deposit_b)
        .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;

    if is_first {
        // Total includes burned MIN_LIQUIDITY shares
        pool.total_lp_shares = shares.checked_add(MIN_LIQUIDITY as u128)
            .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
    } else {
        pool.total_lp_shares = pool.total_lp_shares.checked_add(shares)
            .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
    }

    // --- Initialize or update LpPosition ---
    provider_key = pubkey_bytes(accounts.authority);
    pool_key = pubkey_bytes(accounts.pool_state);

    // Layer 9 D29 — auto-claim payout amounts (auto_claim_a/b declared in the
    // outer scope) are populated only on the existing-position branch when there
    // are pending fees; the post-effects pool-PDA-signed outbound transfers
    // (after the guard drop) have a single source of truth.

    // Check if LpPosition already exists (data_len > 0 means initialized)
    let lp_data_len = accounts.lp_position.data_len();
    if lp_data_len == 0 {
        // R46 escalation: extract fresh-init CreateAccount + Signer locals into
        // a child frame so the [Seed; 4] + Signer + lamports + rent locals
        // (~256B union worst-case) don't share parent stack budget with the
        // auto-claim helper outbound branch's seeds. Returns lp_bump for
        // downstream LpPosition::init to populate `lp.bump`.
        let lp_bump = create_lp_position_account(
            accounts.payer,
            accounts.lp_position,
            program_id,
            &pool_key,
            &provider_key,
        )?;

        let mut lp = LpPosition::init(accounts.lp_position, program_id)?;
        lp.pool = pool_key;
        lp.owner = provider_key;
        lp.shares = shares;
        lp.last_update_ts = clock.unix_timestamp;
        lp.bump = lp_bump;
        // Layer 9 D29 — snapshot pool's current cumulative-fee-per-share so
        // the new LP starts with zero claimable. Without this init, a fresh
        // LP would be able to claim historical fees that accrued before they
        // joined the pool, breaking `vault == reserves + Σ(claimable)`.
        lp.fees_claimed_per_share_a = cumulative_a;
        lp.fees_claimed_per_share_b = cumulative_b;
    } else {
        // Update existing position
        let mut lp = LpPosition::load_mut(accounts.lp_position, program_id)?;
        // SECURITY: Verify LpPosition belongs to this provider
        if lp.owner != provider_key {
            return Err(ProgramError::from(DexError::Unauthorized));
        }
        if lp.pool != pool_key {
            return Err(ProgramError::from(DexError::InvalidVault));
        }

        // Layer 9 D29 — auto-claim pending fees BEFORE incrementing shares.
        // Skipping this would dilute pre-existing claimable across the
        // post-increment share count, over-paying the fresh deposit at the
        // expense of the position's own historical accrual. The CEI-style
        // outbound vault-PDA transfer is deferred until after all account-
        // mutation effects are applied (further below in this handler).
        if lp.shares > 0 {
            let delta_a_q64 = cumulative_a
                .checked_sub(lp.fees_claimed_per_share_a)
                .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
            let delta_b_q64 = cumulative_b
                .checked_sub(lp.fees_claimed_per_share_b)
                .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
            auto_claim_a = compute_claimable(delta_a_q64, lp.shares)?;
            auto_claim_b = compute_claimable(delta_b_q64, lp.shares)?;
            // Advance the snapshot so a subsequent claim_lp_fees observes
            // zero delta. Effects-before-CPI; the vault-side transfer below
            // moves the realised value to the provider ATAs.
            lp.fees_claimed_per_share_a = cumulative_a;
            lp.fees_claimed_per_share_b = cumulative_b;
        } else {
            // Defensive: an LpPosition account exists but holds zero shares. In
            // normal flow `remove_liquidity` closes the account when shares hit
            // zero, so this branch is unreachable. However, if a future refactor
            // breaks the close path, the stale `lp.fees_claimed_per_share_*`
            // from when the LP last had shares would let the newly-minted shares
            // retroactively claim all bumps that occurred during the zero-shares
            // period — draining vault. Re-pin to the current cumulative so the
            // new shares start with zero claimable, matching fresh-init semantics.
            lp.fees_claimed_per_share_a = cumulative_a;
            lp.fees_claimed_per_share_b = cumulative_b;
        }

        lp.shares = lp.shares.checked_add(shares)
            .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
        lp.last_update_ts = clock.unix_timestamp;
    }
    } // PoolState + LpPosition guards dropped here — borrow flags released before
      // the interactions, in particular the pool-PDA-signed auto_claim_lp_fees.

    // --- Interactions: transfer tokens from provider to vaults ---
    // User-signed path: empty signer slice (authority is a transaction signer).
    // PDA-signed path: caller-supplied seeds authorize the PDA-owned ATA.
    if deposit_a > 0 {
        let transfer_a = arlex_lang::token::instructions::Transfer {
            from: accounts.provider_token_a,
            to: accounts.vault_a,
            authority: accounts.authority,
            amount: deposit_a,
        };
        match authority_signer_seeds {
            Some(seeds) => transfer_a.invoke_signed(seeds)?,
            None => transfer_a.invoke()?,
        }
    }

    if deposit_b > 0 {
        let transfer_b = arlex_lang::token::instructions::Transfer {
            from: accounts.provider_token_b,
            to: accounts.vault_b,
            authority: accounts.authority,
            amount: deposit_b,
        };
        match authority_signer_seeds {
            Some(seeds) => transfer_b.invoke_signed(seeds)?,
            None => transfer_b.invoke()?,
        }
    }

    // Layer 10 R46 — auto-claim outbound transfers extracted into a child
    // frame (`auto_claim_lp_fees`) so the [Seed; 4] arrays + Signer wrappers
    // live in their own stack slot rather than unioning with this handler's
    // fresh-init Signer locals (CreateAccount above + provider→vault
    // transfers). This shaves the parent frame back under the SBF 4096-byte
    // budget; see crate-level "BPF stack-frame budget" doc-comment.
    auto_claim_lp_fees(
        accounts.vault_a,
        accounts.vault_b,
        accounts.provider_token_a,
        accounts.provider_token_b,
        accounts.pool_state,
        &pool_token_a_mint,
        &pool_token_b_mint,
        pool_bump,
        auto_claim_a,
        auto_claim_b,
    )?;

    if auto_claim_a > 0 || auto_claim_b > 0 {
        emit!(LpFeesClaimed {
            recipient: provider_key,
            pool: pool_key,
            claimable_a: auto_claim_a,
            claimable_b: auto_claim_b,
            timestamp: clock.unix_timestamp,
        });
    }

    // --- Emit event ---
    emit!(LiquidityAdded {
        pool: pool_key,
        provider: provider_key,
        amount_a: deposit_a,
        amount_b: deposit_b,
        shares_minted: shares,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Layer 9 D29 — pure-Rust math + invariant tests for the
    //! `add_liquidity` LP-fee snapshot init / auto-claim integration.
    //!
    //! Handler-level wiring is exercised by future BPF integration tests
    //! where Arlex can be invoked end-to-end. The tests here pin the on-
    //! chain math (snapshot alignment, claim-then-increment ordering)
    //! against a synthetic in-memory state, mirroring the style used by
    //! `claim_lp_fees.rs::tests`.
    use super::*;
    use crate::state::{LpPosition, PoolState};

    /// Synthetic `PoolState` with the cumulative-fee fields populated.
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

    /// Synthetic `LpPosition` with a known shares + snapshot. SAFETY: same
    /// `repr(C, packed)` zero-default rationale as `make_pool`.
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

    /// D29 — fresh LpPosition init path. Pool has cumulative_a > 0 from
    /// prior swaps; the new LP MUST snapshot the pool's current cumulative
    /// so claimable evaluates to 0 — otherwise it would steal historical
    /// fees that accrued before it joined.
    #[test]
    fn add_liquidity_initializes_fee_snapshot_from_pool_cumulative() {
        let cumulative_a: u128 = 7u128 << 64;
        let cumulative_b: u128 = 11u128 << 64;
        let pool = make_pool(cumulative_a, cumulative_b, /* total_shares */ 5_000);

        // Mirror the fresh-init effect inside add_liquidity_internal.
        let mut lp = make_position(/* shares */ 0, 0, 0);
        lp.shares = 1_000;
        lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
        lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;

        // Immediate claim against the freshly-snapped position must be zero.
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

    /// D29 — existing-position increment path. The handler MUST realise
    /// pending claimable BEFORE incrementing `shares`; otherwise the new
    /// shares dilute the historical accrual. This pins the math: the
    /// claimable is computed on `lp.shares_pre`, snapshot is advanced, then
    /// shares are added.
    #[test]
    fn add_liquidity_existing_position_auto_claims_before_increment() {
        // Pool has cumulative_a from prior swaps; cumulative_b stays zero.
        let cumulative_a: u128 = 4u128 << 64;
        let cumulative_b: u128 = 0;
        let pool = make_pool(cumulative_a, cumulative_b, 1_000);

        // Existing position with 1_000 shares, zero snapshot ⇒ pending fees.
        let mut lp = make_position(1_000, 0, 0);
        let shares_pre = lp.shares;

        // Effects step (matches handler):
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
        // (4 << 64) * 1000 >> 64 == 4_000.
        assert_eq!(auto_claim_a, 4_000u64);
        assert_eq!(auto_claim_b, 0u64);

        // Snapshot advances BEFORE the share increment.
        lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
        lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;

        // Now increment shares (e.g. by 500 from the new deposit).
        let new_shares: u128 = 500;
        lp.shares = lp.shares.checked_add(new_shares).unwrap();
        assert_eq!({ lp.shares }, 1_500);

        // Second claim immediately after must observe zero delta on both
        // sides — the post-increment 1_500 shares cannot retroactively
        // multiply the original 4 << 64 cumulative.
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
        assert_eq!(compute_claimable(delta_a2, lp.shares).unwrap(), 0u64);
        assert_eq!(compute_claimable(delta_b2, lp.shares).unwrap(), 0u64);
    }

    // ----- CP-5 MasterPoolUserLpDisabled guard ---------------------------

    /// Decode `ProgramError::Custom` into the inner u32 so tests can match
    /// against `DexError` discriminants without depending on the exact
    /// numeric base.
    fn custom_code(err: ProgramError) -> u32 {
        match err {
            ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    fn code_of(err: DexError) -> u32 {
        custom_code(ProgramError::from(err))
    }

    /// CP-5 — `add_liquidity` MUST reject master (Monotonic Ladder)
    /// pools with `MasterPoolUserLpDisabled`. The user-LP surface is
    /// disabled by design; the only liquidity sources are Nexus
    /// (`nexus_add_liquidity`) and the Pool Rebalancer
    /// (`grow_liquidity` / `compress_liquidity`).
    /// Pinned via the shared guard helper so refactors cannot drift
    /// the rejection surface between `add_liquidity_internal` and
    /// `zap_liquidity_internal`.
    #[test]
    fn rejects_master_pool_add_liquidity() {
        let err = enforce_user_lp_allowed(POOL_TYPE_CONCENTRATED).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::MasterPoolUserLpDisabled));
    }

    /// CP-5 — StandardCurve pools still accept user LP. Sanity that
    /// the guard doesn't accidentally widen its rejection to the
    /// non-master path.
    #[test]
    fn accepts_standard_curve_add_liquidity() {
        assert!(enforce_user_lp_allowed(POOL_TYPE_STANDARD).is_ok());
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
        const SRC: &str = include_str!("add_liquidity.rs");
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
