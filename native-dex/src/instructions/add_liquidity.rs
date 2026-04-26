use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::{LiquidityAdded, LpFeesClaimed};
use crate::state::{DexConfig, PoolState, LpPosition, BinArray};
use crate::amm::calculate_lp_shares;
use crate::instructions::claim_lp_fees::compute_claimable;
use crate::validation::*;
use crate::concentrated;

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
pub(crate) fn add_liquidity_internal<'info>(
    accounts: &AddLiquidityAccountsView<'info>,
    remaining_accounts: &'info [AccountView],
    program_id: &Address,
    amount_a: u64,
    amount_b: u64,
    min_shares: u128,
    authority_signer_seeds: Option<&[Signer]>,
) -> Result<()> {
    let config = DexConfig::load(accounts.dex_config, program_id)?;
    let pool = PoolState::load_mut(accounts.pool_state, program_id)?;

    // --- Checks ---
    if !config.is_active {
        return Err(ProgramError::from(DexError::DexPaused));
    }
    if !pool.is_active {
        return Err(ProgramError::from(DexError::PoolNotActive));
    }
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
    let pool_bump = pool.bump;
    let pool_token_a_mint = pool.token_a_mint;
    let pool_token_b_mint = pool.token_b_mint;

    // --- Calculate LP shares ---
    let is_first = pool.total_lp_shares == 0;
    let (deposit_a, deposit_b, shares) = if is_first {
        // First LP: use both amounts directly
        let shares = calculate_lp_shares(amount_a, amount_b, 0, 0, 0, true)?;
        (amount_a, amount_b, shares)
    } else {
        // Subsequent LP: proportional deposit
        // Calculate max balanced deposit based on pool ratio
        // deposit_b_needed = amount_a * reserve_b / reserve_a
        let b_needed = arlex_lang::math::mul_div_u64(amount_a, pool.reserve_b, pool.reserve_a)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;

        let (dep_a, dep_b) = if b_needed <= amount_b {
            // amount_a is the limiting factor
            (amount_a, b_needed)
        } else {
            // amount_b is the limiting factor
            let a_needed = arlex_lang::math::mul_div_u64(amount_b, pool.reserve_a, pool.reserve_b)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
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

    // --- Distribute to bins for concentrated pools ---
    if pool.pool_type == POOL_TYPE_CONCENTRATED {
        // BinArray passed as last remaining_account
        let bin_account_idx = remaining_accounts.len().checked_sub(1)
            .ok_or(ProgramError::from(DexError::InvalidBinRange))?;
        // Verify BinArray PDA derivation
        let pool_key_for_bin = pubkey_bytes(accounts.pool_state);
        let (expected_bin_pda, _) = arlex_lang::find_program_address(
            &[b"bins", pool_key_for_bin.as_ref()],
            program_id,
        );
        if remaining_accounts[bin_account_idx].address().as_ref() != expected_bin_pda.as_ref() {
            return Err(ProgramError::InvalidSeeds);
        }
        let bin_array = BinArray::load_mut(&remaining_accounts[bin_account_idx], program_id)?;
        concentrated::distribute_to_bins(
            bin_array,
            deposit_a,
            deposit_b,
            is_first,
        )?;
    }

    // --- Effects: update pool state BEFORE CPIs ---
    pool.reserve_a = pool.reserve_a.checked_add(deposit_a)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    pool.reserve_b = pool.reserve_b.checked_add(deposit_b)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    if is_first {
        // Total includes burned MIN_LIQUIDITY shares
        pool.total_lp_shares = shares.checked_add(MIN_LIQUIDITY as u128)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    } else {
        pool.total_lp_shares = pool.total_lp_shares.checked_add(shares)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }

    // --- Initialize or update LpPosition ---
    let provider_key = pubkey_bytes(accounts.authority);
    let pool_key = pubkey_bytes(accounts.pool_state);
    let clock = Clock::get()?;

    // Layer 9 D29 — auto-claim payout amounts. Populated only on the existing-
    // position branch when there are pending fees; declared here so the
    // post-effects pool-PDA-signed outbound transfers (below) have a single
    // source of truth.
    let mut auto_claim_a: u64 = 0;
    let mut auto_claim_b: u64 = 0;

    // Check if LpPosition already exists (data_len > 0 means initialized)
    let lp_data_len = accounts.lp_position.data_len();
    if lp_data_len == 0 {
        // Create new LpPosition PDA
        let (lp_pda, lp_bump) = arlex_lang::find_program_address(
            &[b"lp", pool_key.as_ref(), provider_key.as_ref()],
            program_id,
        );
        if accounts.lp_position.address().as_ref() != lp_pda.as_ref() {
            return Err(ProgramError::InvalidSeeds);
        }

        let rent = pinocchio::sysvars::rent::Rent::get()?;
        let lamports = rent.minimum_balance(LpPosition::SPACE);
        arlex_lang::system::instructions::CreateAccount {
            from: accounts.payer,
            to: accounts.lp_position,
            lamports,
            space: LpPosition::SPACE as u64,
            owner: program_id,
        }.invoke_signed(&[Signer::from(&[
            Seed::from(b"lp" as &[u8]),
            Seed::from(pool_key.as_ref()),
            Seed::from(provider_key.as_ref()),
            Seed::from(&[lp_bump]),
        ])])?;

        let lp = LpPosition::init(accounts.lp_position, program_id)?;
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
        let lp = LpPosition::load_mut(accounts.lp_position, program_id)?;
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
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            let delta_b_q64 = cumulative_b
                .checked_sub(lp.fees_claimed_per_share_b)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            auto_claim_a = compute_claimable(delta_a_q64, lp.shares)?;
            auto_claim_b = compute_claimable(delta_b_q64, lp.shares)?;
            // Advance the snapshot so a subsequent claim_lp_fees observes
            // zero delta. Effects-before-CPI; the vault-side transfer below
            // moves the realised value to the provider ATAs.
            lp.fees_claimed_per_share_a = cumulative_a;
            lp.fees_claimed_per_share_b = cumulative_b;
        }

        lp.shares = lp.shares.checked_add(shares)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        lp.last_update_ts = clock.unix_timestamp;
    }

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

    // Layer 9 D29 — auto-claim outbound transfers. Vault → provider ATA on
    // each side that accrued pending fees. Authority is the pool PDA (vault
    // owner), so the seeds are the canonical create_pool seeds: ["pool",
    // token_a_mint, token_b_mint, bump]. Skipped when both sides are zero
    // (fresh position, or existing position with snapshot already aligned).
    if auto_claim_a > 0 || auto_claim_b > 0 {
        let pool_bump_arr = [pool_bump];
        if auto_claim_a > 0 {
            let seeds = [
                Seed::from(b"pool" as &[u8]),
                Seed::from(pool_token_a_mint.as_ref()),
                Seed::from(pool_token_b_mint.as_ref()),
                Seed::from(pool_bump_arr.as_ref()),
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
                Seed::from(pool_token_a_mint.as_ref()),
                Seed::from(pool_token_b_mint.as_ref()),
                Seed::from(pool_bump_arr.as_ref()),
            ];
            arlex_lang::token::instructions::Transfer {
                from: accounts.vault_b,
                to: accounts.provider_token_b,
                authority: accounts.pool_state,
                amount: auto_claim_b,
            }.invoke_signed(&[Signer::from(&seeds)])?;
        }
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
}
