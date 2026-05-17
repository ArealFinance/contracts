use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::{ZapLiquidityExecuted, LpFeesClaimed};
use crate::state::*;
use crate::amm::{constant_product_output, calculate_fees, calculate_lp_shares};
use crate::instructions::add_liquidity::auto_claim_lp_fees;
use crate::instructions::claim_lp_fees::compute_claimable;
use crate::validation::*;

#[derive(Accounts)]
pub struct ZapLiquidity<'info> {
    #[account(signer)]
    pub provider: &'info AccountView,

    #[account(mut, signer)]
    pub payer: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,

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

    // Protocol fee destination (for internal swap fee)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub areal_fee_account: &'info AccountView,

    // Optional: OT treasury fee destination (remaining accounts[0])

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

/// Decoupled view of `ZapLiquidity` accounts used by `zap_liquidity_internal`.
///
/// Allows reuse of the core zap logic from any handler whose accounts can
/// project into this shape (user-signed `zap_liquidity` or PDA-signed
/// alternative callers added in later layers).
pub(crate) struct ZapLiquidityAccountsView<'info> {
    pub authority: &'info AccountView,
    pub payer: &'info AccountView,
    pub dex_config: &'info AccountView,
    pub pool_state: &'info AccountView,
    pub lp_position: &'info AccountView,
    pub provider_token_a: &'info AccountView,
    pub provider_token_b: &'info AccountView,
    pub vault_a: &'info AccountView,
    pub vault_b: &'info AccountView,
    pub areal_fee_account: &'info AccountView,
}

impl<'info> ZapLiquidity<'info> {
    pub(crate) fn view(&self) -> ZapLiquidityAccountsView<'info> {
        ZapLiquidityAccountsView {
            authority: self.provider,
            payer: self.payer,
            dex_config: self.dex_config,
            pool_state: self.pool_state,
            lp_position: self.lp_position,
            provider_token_a: self.provider_token_a,
            provider_token_b: self.provider_token_b,
            vault_a: self.vault_a,
            vault_b: self.vault_b,
            areal_fee_account: self.areal_fee_account,
        }
    }
}

pub fn handler(
    ctx: Context<ZapLiquidity>,
    amount_a: u64,
    amount_b: u64,
    min_shares: u128,
) -> Result<()> {
    // CP-5 — Master (Monotonic Ladder) pools reject user LP entirely
    // (see `add_liquidity::handler` for the architectural rationale).
    // The check lives at the user-signed entrypoint, NOT inside
    // `zap_liquidity_internal`, mirroring the structure used by
    // `add_liquidity::handler`. This keeps the shared internal helper
    // free to service a future PDA-signed caller (no such caller
    // exists today, but the parity makes the rejection surface
    // entrypoint-scoped and easy to reason about).
    {
        let pool = PoolState::load(ctx.accounts.pool_state, ctx.program_id)?;
        crate::instructions::add_liquidity::enforce_user_lp_allowed(pool.pool_type)?;
    }

    zap_liquidity_internal(
        &ctx.accounts.view(),
        ctx.remaining_accounts,
        ctx.program_id,
        amount_a,
        amount_b,
        min_shares,
        None,
    )
}

/// Core zap-liquidity logic — usable by user-signed `zap_liquidity` and by
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
pub(crate) fn zap_liquidity_internal<'info>(
    accounts: &ZapLiquidityAccountsView<'info>,
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
    // CP-5 — Master-pool user-LP rejection lives at the user-signed
    // entrypoints (`add_liquidity::handler`, `zap_liquidity::handler`)
    // via `enforce_user_lp_allowed`, NOT here. This shared helper is
    // kept entry-agnostic so a future PDA-signed caller can reuse it
    // without tripping the master-pool guard. Prior to CP-5 this branch
    // raised `InvalidPoolType` (zap had no bin-aware path); the typed
    // rejection now lives in `zap_liquidity::handler` per
    // docs/changelog/2026-04-17-monotonic-ladder.mdx:97 and
    // docs/contracts/native-dex.mdx §83.
    if amount_a == 0 && amount_b == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount));
    }

    // SECURITY: Validate vaults match pool state
    validate_vault(accounts.vault_a, &pool.vault_a)?;
    validate_vault(accounts.vault_b, &pool.vault_b)?;

    // Layer 9 D29+ — LpPosition pinning uses the LIVE (post-bump) cumulative
    // `pool.cumulative_fees_per_share_<side>`, not a pre-bump snapshot. The
    // virtual internal swap below bumps the per-share accumulator (consistent
    // with `swap_internal`); subsequent LpPosition handling reads the bumped
    // value so:
    //   - Fresh-init: new LP starts with `fees_claimed_per_share = post-bump`,
    //     i.e. zero claimable from the bump they themselves just triggered.
    //   - Existing-position: auto-claim computes the FULL delta (historical +
    //     this LP's fair share of the zap-internal bump, based on `shares_pre`).
    //     Snapshot then advances to post-bump so the new shares the depositor
    //     receives don't retroactively claim against the same bump.
    // Net effect across all LPs: total outstanding claim from a single
    // zap-internal bump equals `fee_lp` exactly (sum of `bump_per_share ×
    // shares_pre` over all LPs present at bump time), matching the physical
    // `fee_lp` that stays in the RWT-side vault.

    // Validate areal_fee_account address matches the immutable
    // `dex_config.areal_fee_destination` pinned at `initialize_dex` time.
    if accounts.areal_fee_account.address().as_ref() != config.areal_fee_destination.as_ref() {
        return Err(ProgramError::from(DexError::InvalidTokenAccount));
    }
    // Per spec (Fee Architecture Step 4: "Always RWT"; Step 2: Protocol Fee
    // "always in RWT, transferred to areal_fee_destination"), the protocol
    // fee destination MUST be an RWT ATA. Mirror the same fail-fast revert
    // used by `swap` so misconfigured clusters surface a typed error rather
    // than the SPL Token raw `MintMismatch` (0x3) at CPI time. Symmetric with
    // `swap.rs:200-203`.
    let areal_dest_mint = read_token_account_mint(accounts.areal_fee_account)?;
    if areal_dest_mint != RWT_MINT {
        return Err(ProgramError::from(DexError::InvalidProtocolFeeDestination));
    }

    // OT treasury validation
    if pool.has_ot_treasury {
        if remaining_accounts.is_empty() {
            return Err(ProgramError::from(DexError::MissingOtTreasuryAccount));
        }
        let ot_fee_account = &remaining_accounts[0];
        if ot_fee_account.address().as_ref() != pool.ot_treasury_fee_destination.as_ref() {
            return Err(ProgramError::from(DexError::OtTreasuryAccountMismatch));
        }
    }

    let (final_a, final_b, swapped_amount, fee_lp_total, fee_protocol_total, fee_ot_total);

    // Fee-on-top gross-up on the RWT side (docs/contracts/native-dex.mdx:534-535).
    // When the zap-internal swap consumes the RWT side as INPUT, the user
    // contributes `swap_<rwt> + fee_total + fee_ot_treasury` (equivalently
    // `swap_<rwt> + fee_lp + fee_protocol + fee_ot_treasury`) on that side.
    // The extra amount is captured in `rwt_extra_a` / `rwt_extra_b` here and
    // added to the inbound transfer on the RWT side below. Exactly one of
    // these is non-zero on the RWT-input branches; both stay 0 otherwise.
    let rwt_extra_a: u64;
    let rwt_extra_b: u64;

    if pool.reserve_a == 0 && pool.reserve_b == 0 {
        // Empty pool: skip swap, treat as regular add_liquidity
        if amount_a == 0 || amount_b == 0 {
            return Err(ProgramError::from(DexError::ZeroAmount));
        }
        final_a = amount_a;
        final_b = amount_b;
        swapped_amount = 0;
        fee_lp_total = 0;
        fee_protocol_total = 0;
        fee_ot_total = 0;
        rwt_extra_a = 0;
        rwt_extra_b = 0;
    } else {
        // Calculate the balanced ratio and determine excess
        // Target ratio: amount_a / amount_b = reserve_a / reserve_b
        let value_a_in_b = arlex_lang::math::mul_div_u64(amount_a, pool.reserve_b, pool.reserve_a)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;

        if value_a_in_b <= amount_b && amount_a > 0 {
            // Excess B: swap some B → A
            let excess_b = amount_b.checked_sub(value_a_in_b)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            let swap_b = excess_b / 2; // swap half of excess

            if swap_b == 0 {
                // No swap needed, use amounts as-is (balanced enough)
                final_a = amount_a;
                final_b = amount_b;
                swapped_amount = 0;
                fee_lp_total = 0;
                fee_protocol_total = 0;
                fee_ot_total = 0;
                rwt_extra_a = 0;
                rwt_extra_b = 0;
            } else {
                // Internal swap: B → A (b_to_a)
                let b_is_rwt = is_rwt_mint(&pool.token_b_mint);
                let (swap_out, flp, fp, fot) = internal_swap(
                    swap_b, pool.reserve_b, pool.reserve_a,
                    b_is_rwt, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury,
                )?;

                // Update reserves after internal swap. Fee-on-top per docs
                // (native-dex.mdx:522-568): full swap_<rwt> enters reserves
                // on the RWT side; fee_lp stays in the RWT vault and is
                // tracked via `pool.cumulative_fees_per_share_<rwt_side>`;
                // fee_protocol + fee_ot_treasury are CPI-extracted by the
                // outbound transfers below.
                if b_is_rwt {
                    // B is RWT input. User contributes amount_b + fees to the
                    // RWT (B) vault (fees on top per docs); full swap_b enters
                    // reserves; fee_lp stays in vault tracked by the per-share
                    // accumulator; fee_protocol + fee_ot_treasury are CPI-extracted.
                    let fees = calculate_fees(swap_b, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
                    // Full swap_b into reserves (fee-on-top).
                    pool.reserve_b = pool.reserve_b.checked_add(swap_b)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    pool.reserve_a = pool.reserve_a.checked_sub(swap_out)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    crate::instructions::swap::accrue_lp_fee_per_share(pool, fees.fee_lp, false)?;

                    // Inbound debit on the RWT (B) side must include fee_lp +
                    // fee_protocol + fee_ot_treasury. Equivalent to
                    // fee_total + ot_treasury_fee.
                    rwt_extra_a = 0;
                    rwt_extra_b = fees.fee_lp
                        .checked_add(fees.fee_protocol).ok_or(ProgramError::from(DexError::MathOverflow))?
                        .checked_add(fees.ot_treasury_fee).ok_or(ProgramError::from(DexError::MathOverflow))?;
                } else {
                    // A is RWT output. RWT side = A → rwt_is_side_a = true.
                    // Fees come from the output (gross_out) side per docs; the
                    // RWT-input gross-up does not apply here.
                    let gross_out_for_fees = constant_product_output(pool.reserve_b, pool.reserve_a, swap_b)?;
                    let fees = calculate_fees(gross_out_for_fees, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
                    pool.reserve_b = pool.reserve_b.checked_add(swap_b)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    // Subtract FULL gross_out (fee_lp stays in vault, tracked via accumulator).
                    pool.reserve_a = pool.reserve_a.checked_sub(gross_out_for_fees)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    crate::instructions::swap::accrue_lp_fee_per_share(pool, fees.fee_lp, true)?;

                    rwt_extra_a = 0;
                    rwt_extra_b = 0;
                }

                final_a = amount_a.checked_add(swap_out)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?;
                final_b = amount_b.checked_sub(swap_b)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?;
                swapped_amount = swap_b;
                fee_lp_total = flp;
                fee_protocol_total = fp;
                fee_ot_total = fot;
            }
        } else {
            // Excess A: swap some A → B
            let value_b_in_a = arlex_lang::math::mul_div_u64(amount_b, pool.reserve_a, pool.reserve_b)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            let excess_a = amount_a.checked_sub(value_b_in_a).unwrap_or(0);
            let swap_a = excess_a / 2;

            if swap_a == 0 {
                final_a = amount_a;
                final_b = amount_b;
                swapped_amount = 0;
                fee_lp_total = 0;
                fee_protocol_total = 0;
                fee_ot_total = 0;
                rwt_extra_a = 0;
                rwt_extra_b = 0;
            } else {
                let a_is_rwt = is_rwt_mint(&pool.token_a_mint);
                let (swap_out, flp, fp, fot) = internal_swap(
                    swap_a, pool.reserve_a, pool.reserve_b,
                    a_is_rwt, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury,
                )?;

                // Update reserves after internal swap. See "Excess B" branch
                // above for the rationale — full swap_<rwt> enters reserves on
                // the RWT side (fees on top per docs); fee_lp stays in the
                // RWT-side vault and is tracked via `cumulative_fees_per_share_<side>`.
                if a_is_rwt {
                    // A is RWT input. User contributes amount_a + fees to the
                    // RWT (A) vault (fees on top per docs); full swap_a enters
                    // reserves; fee_lp stays in vault tracked by the per-share
                    // accumulator; fee_protocol + fee_ot_treasury are CPI-extracted.
                    let fees = calculate_fees(swap_a, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
                    // Full swap_a into reserves (fee-on-top).
                    pool.reserve_a = pool.reserve_a.checked_add(swap_a)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    pool.reserve_b = pool.reserve_b.checked_sub(swap_out)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    crate::instructions::swap::accrue_lp_fee_per_share(pool, fees.fee_lp, true)?;

                    // Inbound debit on the RWT (A) side must include fee_lp +
                    // fee_protocol + fee_ot_treasury.
                    rwt_extra_a = fees.fee_lp
                        .checked_add(fees.fee_protocol).ok_or(ProgramError::from(DexError::MathOverflow))?
                        .checked_add(fees.ot_treasury_fee).ok_or(ProgramError::from(DexError::MathOverflow))?;
                    rwt_extra_b = 0;
                } else {
                    // B is RWT output. RWT side = B → rwt_is_side_a = false.
                    // Fees come from the output (gross_out) side per docs.
                    let gross_out_for_fees = constant_product_output(pool.reserve_a, pool.reserve_b, swap_a)?;
                    let fees = calculate_fees(gross_out_for_fees, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
                    pool.reserve_a = pool.reserve_a.checked_add(swap_a)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    // Subtract FULL gross_out (fee_lp stays in vault, tracked via accumulator).
                    pool.reserve_b = pool.reserve_b.checked_sub(gross_out_for_fees)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    crate::instructions::swap::accrue_lp_fee_per_share(pool, fees.fee_lp, false)?;

                    rwt_extra_a = 0;
                    rwt_extra_b = 0;
                }

                final_a = amount_a.checked_sub(swap_a)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?;
                final_b = amount_b.checked_add(swap_out)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?;
                swapped_amount = swap_a;
                fee_lp_total = flp;
                fee_protocol_total = fp;
                fee_ot_total = fot;
            }
        }
    }

    // --- Calculate LP shares from balanced amounts ---
    let is_first = pool.total_lp_shares == 0;

    // For LP share calculation, use the balanced amounts against current reserves
    let (dep_a, dep_b) = if is_first {
        (final_a, final_b)
    } else {
        // Proportional: find the balanced pair
        let b_needed = arlex_lang::math::mul_div_u64(final_a, pool.reserve_b, pool.reserve_a)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        if b_needed <= final_b {
            (final_a, b_needed)
        } else {
            let a_needed = arlex_lang::math::mul_div_u64(final_b, pool.reserve_a, pool.reserve_b)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            (a_needed, final_b)
        }
    };

    if dep_a == 0 || dep_b == 0 {
        return Err(ProgramError::from(DexError::ZeroOutput));
    }

    let shares = calculate_lp_shares(
        dep_a, dep_b, pool.reserve_a, pool.reserve_b, pool.total_lp_shares, is_first,
    )?;

    if shares == 0 {
        return Err(ProgramError::from(DexError::ZeroOutput));
    }
    if shares < min_shares {
        return Err(ProgramError::from(DexError::SlippageExceeded));
    }

    // --- Effects: update pool reserves ---
    pool.reserve_a = pool.reserve_a.checked_add(dep_a)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    pool.reserve_b = pool.reserve_b.checked_add(dep_b)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    if is_first {
        pool.total_lp_shares = shares.checked_add(MIN_LIQUIDITY as u128)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    } else {
        pool.total_lp_shares = pool.total_lp_shares.checked_add(shares)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }

    // Accumulate swap fees.
    // W-1 sweep: include fee_lp_total for parity with `swap_internal`
    // (swap.rs:388-391). Stats counter only — funds flow is unchanged
    // (fee_lp physically stays in the vault, tracked off-reserve via
    // the per-share accumulator); this just makes the cumulative
    // protocol-revenue stat truthful on the zap path.
    pool.total_fees_accumulated = pool.total_fees_accumulated
        .checked_add(fee_lp_total).ok_or(ProgramError::from(DexError::MathOverflow))?
        .checked_add(fee_protocol_total).ok_or(ProgramError::from(DexError::MathOverflow))?
        .checked_add(fee_ot_total).ok_or(ProgramError::from(DexError::MathOverflow))?;

    // --- Initialize or update LpPosition ---
    let provider_key = pubkey_bytes(accounts.authority);
    let pool_key = pubkey_bytes(accounts.pool_state);
    let clock = Clock::get()?;

    // Layer 9 D29 — auto-claim payout amounts. Populated on the existing-
    // position branch when there are pending fees; consumed by the post-
    // effects pool-PDA-signed outbound transfers below.
    let mut auto_claim_a: u64 = 0;
    let mut auto_claim_b: u64 = 0;

    let lp_data_len = accounts.lp_position.data_len();
    if lp_data_len == 0 {
        let (lp_pda, lp_bump) = arlex_lang::find_program_address(
            &[b"lp", pool_key.as_ref(), provider_key.as_ref()],
            program_id,
        );
        if accounts.lp_position.address().as_ref() != lp_pda.as_ref() {
            return Err(ProgramError::InvalidSeeds);
        }

        let rent = pinocchio::sysvars::rent::Rent::get()?;
        let lamports = rent.try_minimum_balance(LpPosition::SPACE)?;
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
        // Pin to POST-bump cumulative so the new LP's shares get zero claimable
        // from the zap-internal swap that just bumped the accumulator (they weren't
        // in the `total_lp_shares` denominator at bump time). Without this, the
        // new LP would claim `(bump_per_share × new_shares)` of fee_lp that was
        // distributed only across pre-deposit LPs → unfunded claim.
        lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
        lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;
    } else {
        let lp = LpPosition::load_mut(accounts.lp_position, program_id)?;
        // SECURITY: Verify LpPosition belongs to this provider
        if lp.owner != provider_key {
            return Err(ProgramError::from(DexError::Unauthorized));
        }
        if lp.pool != pool_key {
            return Err(ProgramError::from(DexError::InvalidVault));
        }

        // Layer 9 D29 — auto-claim pending fees BEFORE incrementing shares.
        // Mirrors `add_liquidity_internal`: skipping this would dilute the
        // pre-existing claimable across the post-increment share count,
        // over-paying the fresh deposit at the expense of the position's own
        // historical accrual.
        if lp.shares > 0 {
            // Use LIVE post-bump cumulative so the auto-claim includes this LP's
            // fair share of the zap-internal bump (`bump_per_share × shares_pre`,
            // since `lp.shares` here is pre-increment). Historical fees + new
            // bump share are paid out atomically. Then snapshot advances to
            // post-bump so the new shares added below don't retroactively claim
            // against the same bump.
            let delta_a_q64 = pool
                .cumulative_fees_per_share_a
                .checked_sub(lp.fees_claimed_per_share_a)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            let delta_b_q64 = pool
                .cumulative_fees_per_share_b
                .checked_sub(lp.fees_claimed_per_share_b)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            auto_claim_a = compute_claimable(delta_a_q64, lp.shares)?;
            auto_claim_b = compute_claimable(delta_b_q64, lp.shares)?;
            // Effects-before-CPI; the vault-side transfer below moves the
            // realised value to the provider ATAs.
            lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
            lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;
        } else {
            // Defensive: an LpPosition account exists but holds zero shares. In
            // normal flow `remove_liquidity` closes the account when shares hit
            // zero (rent returned to provider), so this branch is unreachable.
            // However, if a future refactor inadvertently breaks the close path,
            // the stale `lp.fees_claimed_per_share_*` from when the LP last had
            // shares would let the newly-minted shares retroactively claim all
            // bumps that occurred during the zero-shares period — draining vault.
            // Re-pin to live cumulative so the new shares start with zero
            // claimable, matching the fresh-init semantics.
            lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
            lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;
        }

        lp.shares = lp.shares.checked_add(shares)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        lp.last_update_ts = clock.unix_timestamp;
    }

    // --- Interactions: transfer tokens from provider to vaults ---
    // Transfer the total user input (amount_<side> + rwt_extra_<side>), the
    // internal swap is virtual. Fee-on-top (docs/contracts/native-dex.mdx:534-535):
    // on the RWT side the user is debited `amount_<rwt> + fee_lp + fee_protocol
    // + fee_ot_treasury` so the RWT vault holds enough to honour reserves
    // PLUS the upcoming pool-PDA-signed extractions of fee_protocol/fee_ot_treasury
    // (fee_lp stays in vault, tracked via the per-share accumulator).
    // Non-RWT side has `rwt_extra_<side> == 0` and is untouched.
    // User-signed path: empty signer slice (authority is a transaction signer).
    // PDA-signed path: caller-supplied seeds authorize the PDA-owned ATA.
    let inbound_a = amount_a
        .checked_add(rwt_extra_a)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;
    let inbound_b = amount_b
        .checked_add(rwt_extra_b)
        .ok_or(ProgramError::from(DexError::MathOverflow))?;

    if inbound_a > 0 {
        let transfer_a = arlex_lang::token::instructions::Transfer {
            from: accounts.provider_token_a,
            to: accounts.vault_a,
            authority: accounts.authority,
            amount: inbound_a,
        };
        match authority_signer_seeds {
            Some(seeds) => transfer_a.invoke_signed(seeds)?,
            None => transfer_a.invoke()?,
        }
    }

    if inbound_b > 0 {
        let transfer_b = arlex_lang::token::instructions::Transfer {
            from: accounts.provider_token_b,
            to: accounts.vault_b,
            authority: accounts.authority,
            amount: inbound_b,
        };
        match authority_signer_seeds {
            Some(seeds) => transfer_b.invoke_signed(seeds)?,
            None => transfer_b.invoke()?,
        }
    }

    // Transfer protocol fees from vault (RWT side)
    let pool_bump_arr = [pool.bump];

    // Layer 10 R46 — auto-claim outbound transfers extracted into a child
    // frame (`auto_claim_lp_fees`, defined in add_liquidity.rs and shared
    // between both LP-add paths) so the [Seed; 4] arrays + Signer wrappers
    // live in their own stack slot rather than unioning with this handler's
    // protocol-fee + ot-fee Signer locals below. See crate-level "BPF
    // stack-frame budget" doc-comment.
    auto_claim_lp_fees(
        accounts.vault_a,
        accounts.vault_b,
        accounts.provider_token_a,
        accounts.provider_token_b,
        accounts.pool_state,
        &pool.token_a_mint,
        &pool.token_b_mint,
        pool.bump,
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

    if fee_protocol_total > 0 {
        let rwt_vault = if is_rwt_mint(&pool.token_a_mint) { accounts.vault_a } else { accounts.vault_b };
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump_arr.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: rwt_vault,
            to: accounts.areal_fee_account,
            authority: accounts.pool_state,
            amount: fee_protocol_total,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    if fee_ot_total > 0 {
        let ot_fee_account = &remaining_accounts[0];
        let rwt_vault = if is_rwt_mint(&pool.token_a_mint) { accounts.vault_a } else { accounts.vault_b };
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump_arr.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: rwt_vault,
            to: ot_fee_account,
            authority: accounts.pool_state,
            amount: fee_ot_total,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // --- Emit event ---
    emit!(ZapLiquidityExecuted {
        pool: pool_key,
        provider: provider_key,
        input_a: amount_a,
        input_b: amount_b,
        swapped_amount,
        shares_minted: shares,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

/// Internal swap calculation (no CPI, virtual swap for zap).
/// Returns (amount_out, fee_protocol, fee_ot_treasury).
///
/// Fee-on-top (docs/contracts/native-dex.mdx:522-568):
/// - `input_is_rwt`: full `amount_in` enters the curve; fees are external.
///   `amount_out = constant_product(amount_in)`.
/// - `!input_is_rwt`: full `amount_in` enters the curve; fees come out of
///   the gross output. `amount_out = gross_out - fee_total - ot_treasury_fee`.
fn internal_swap(
    amount_in: u64,
    reserve_in: u64,
    reserve_out: u64,
    input_is_rwt: bool,
    fee_bps: u16,
    lp_fee_share_bps: u16,
    has_ot_treasury: bool,
) -> core::result::Result<(u64, u64, u64, u64), ProgramError> {
    if input_is_rwt {
        // Fee-on-top: feed the FULL amount_in into the curve. Fees are
        // surfaced separately by the caller (via the gross-up of the
        // RWT-side inbound transfer) and extracted from the vault by the
        // outbound CPIs.
        let fees = calculate_fees(amount_in, fee_bps, lp_fee_share_bps, has_ot_treasury)?;
        let amount_out = constant_product_output(reserve_in, reserve_out, amount_in)?;
        Ok((amount_out, fees.fee_lp, fees.fee_protocol, fees.ot_treasury_fee))
    } else {
        let gross_out = constant_product_output(reserve_in, reserve_out, amount_in)?;
        let fees = calculate_fees(gross_out, fee_bps, lp_fee_share_bps, has_ot_treasury)?;
        let total_deducted = fees.fee_total.checked_add(fees.ot_treasury_fee)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        let amount_out = gross_out.checked_sub(total_deducted)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        Ok((amount_out, fees.fee_lp, fees.fee_protocol, fees.ot_treasury_fee))
    }
}

#[cfg(test)]
mod tests {
    //! Layer 9 D29+ — pure-Rust math + invariant tests for the
    //! `zap_liquidity` LP-fee accumulator integration.
    //! The virtual internal swap bumps the per-share accumulator
    //! (consistent with normal `swap_internal`). LpPosition `fees_claimed_per_share`
    //! is pinned to the POST-bump cumulative so:
    //!   - Fresh-init: new LP has zero claimable from the bump they triggered.
    //!   - Existing-position: auto-claim absorbs the LP's fair share of the
    //!     zap-internal bump (`bump_per_share × shares_pre`), then snapshot
    //!     advances so new shares don't retroactively claim against the
    //!     same bump.
    //! Net invariant: Σ_outstanding_claim_from_one_zap_bump == fee_lp.
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

    /// D29+ — `zap_liquidity` existing-position branch must auto-claim
    /// pending fees against the LIVE post-bump cumulative BEFORE
    /// incrementing shares. This ensures the LP receives their fair share
    /// of any zap-internal bump (based on shares_pre) AND of historical
    /// fees, while preventing the new shares from retroactively claiming
    /// against the same bump.
    #[test]
    fn zap_liquidity_existing_position_auto_claims_before_increment() {
        // Pool has cumulative_b from prior swaps (RWT on side B example);
        // cumulative_a stays zero.
        let cumulative_a: u128 = 0;
        let cumulative_b: u128 = 8u128 << 64;
        let pool = make_pool(cumulative_a, cumulative_b, 2_000);

        // Existing position with 2_000 shares, zero snapshot ⇒ pending B-fees.
        let mut lp = make_position(2_000, 0, 0);
        let shares_pre = lp.shares;

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
        assert_eq!(auto_claim_a, 0u64);
        // (8 << 64) * 2000 >> 64 == 16_000.
        assert_eq!(auto_claim_b, 16_000u64);

        // Snapshot advances before share increment.
        lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
        lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;
        let new_shares: u128 = 1_000;
        lp.shares = lp.shares.checked_add(new_shares).unwrap();
        assert_eq!({ lp.shares }, 3_000);

        // Subsequent claim observes zero delta — post-increment 3_000
        // shares cannot retroactively multiply the original 8 << 64.
        let delta_a2 = pool
            .cumulative_fees_per_share_a
            .checked_sub(lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b2 = pool
            .cumulative_fees_per_share_b
            .checked_sub(lp.fees_claimed_per_share_b)
            .unwrap();
        assert_eq!(compute_claimable(delta_a2, lp.shares).unwrap(), 0u64);
        assert_eq!(compute_claimable(delta_b2, lp.shares).unwrap(), 0u64);
    }

    /// D29 — ZAP-internal swap on the RWT-input side must bump
    /// `cumulative_fees_per_share_<rwt_side>` by the canonical Q64.64 delta:
    /// `(fee_lp << 64) / total_lp_shares`. Non-RWT side stays zero.
    /// This pins the new behaviour where ZAP-internal swap bumps the
    /// per-share accumulator (consistent with normal `swap_internal`).
    #[test]
    fn zap_internal_swap_bumps_accumulator_on_rwt_input_side() {
        // Scenario: RWT is side B (input). Pool has 1M total shares.
        let mut pool = make_pool(0, 0, 1_000_000);
        let fee_lp = 50_000u64;

        // Simulate ZAP-internal swap where RWT (side B) is the input.
        // RWT is side B ⇒ rwt_is_side_a = false.
        crate::instructions::swap::accrue_lp_fee_per_share(&mut pool, fee_lp, false)
            .expect("accrue_lp_fee_per_share should not fail");

        // Copy fields to locals to work around packed struct alignment issues.
        let cumulative_a = pool.cumulative_fees_per_share_a;
        let cumulative_b = pool.cumulative_fees_per_share_b;

        // Side A (non-RWT) unchanged.
        assert_eq!(cumulative_a, 0u128);

        // Side B (RWT) gains delta_per_share = (50_000 << 64) / 1_000_000.
        let expected_b = ((fee_lp as u128) << 64) / 1_000_000u128;
        assert_eq!(cumulative_b, expected_b);

        // Verify the math: a hypothetical LP with 100_000 shares and
        // zero snapshot would claim (expected_b * 100_000) >> 64 tokens.
        // Due to Q64.64 truncation, the claimed amount loses the fractional dust.
        let shares: u128 = 100_000;
        let claimable_b = compute_claimable(cumulative_b, shares)
            .expect("compute_claimable should not fail");
        // (50_000 << 64) / 1_000_000 * 100_000 >> 64 ≈ 4_999 (dust lost).
        assert_eq!(claimable_b, 4_999u64);
    }

    /// D29 — ZAP-internal swap on the RWT-output side (symmetric coverage).
    /// When RWT is side A (output), the A-side accumulator bumps and B stays zero.
    #[test]
    fn zap_internal_swap_bumps_accumulator_on_rwt_output_side() {
        // Scenario: RWT is side A (output). Pool has 2M total shares.
        let mut pool = make_pool(0, 0, 2_000_000);
        let fee_lp = 75_000u64;

        // Simulate ZAP-internal swap where RWT (side A) is the output.
        // RWT is side A ⇒ rwt_is_side_a = true.
        crate::instructions::swap::accrue_lp_fee_per_share(&mut pool, fee_lp, true)
            .expect("accrue_lp_fee_per_share should not fail");

        // Copy fields to locals to work around packed struct alignment issues.
        let cumulative_a = pool.cumulative_fees_per_share_a;
        let cumulative_b = pool.cumulative_fees_per_share_b;

        // Side B (non-RWT) unchanged.
        assert_eq!(cumulative_b, 0u128);

        // Side A (RWT) gains delta_per_share = (75_000 << 64) / 2_000_000.
        let expected_a = ((fee_lp as u128) << 64) / 2_000_000u128;
        assert_eq!(cumulative_a, expected_a);

        // Verify the math: a hypothetical LP with 200_000 shares and
        // zero snapshot would claim (expected_a * 200_000) >> 64 tokens.
        // Due to Q64.64 truncation, the claimed amount loses the fractional dust.
        let shares: u128 = 200_000;
        let claimable_a = compute_claimable(cumulative_a, shares)
            .expect("compute_claimable should not fail");
        // (75_000 << 64) / 2_000_000 * 200_000 >> 64 ≈ 7_499 (dust lost).
        assert_eq!(claimable_a, 7_499u64);
    }

    /// D29 — ZAP-internal swap with empty pool (total_lp_shares == 0) must
    /// not panic or divide by zero. The guard inside `accrue_lp_fee_per_share`
    /// ensures a no-op: both accumulators stay zero. While this scenario
    /// should not occur in practice (ZAP requires a non-empty pool), the
    /// defensive guard must be pinned to prevent future regressions.
    #[test]
    fn zap_internal_swap_with_zero_total_shares_is_noop() {
        // Pool with no LPs yet.
        let mut pool = make_pool(0, 0, 0);
        let fee_lp = 100_000u64;

        // Call accrue on both sides. Should be safe (no panic, no error).
        crate::instructions::swap::accrue_lp_fee_per_share(&mut pool, fee_lp, true)
            .expect("accrue_lp_fee_per_share should handle zero shares gracefully");
        crate::instructions::swap::accrue_lp_fee_per_share(&mut pool, fee_lp, false)
            .expect("accrue_lp_fee_per_share should handle zero shares gracefully");

        // Copy fields to locals to work around packed struct alignment issues.
        let cumulative_a = pool.cumulative_fees_per_share_a;
        let cumulative_b = pool.cumulative_fees_per_share_b;

        // Both accumulators remain zero (no-op when total_lp_shares == 0).
        assert_eq!(cumulative_a, 0u128);
        assert_eq!(cumulative_b, 0u128);
    }

    /// D29+ — `zap_liquidity` fresh-init branch must pin to POST-bump cumulative.
    /// A new LP entering via ZAP should have `fees_claimed_per_share_<side>` equal
    /// to the pool's cumulative AFTER the ZAP-internal swap has bumped the
    /// accumulator. This ensures the new LP has zero claimable from the
    /// zap-internal bump that just happened (they weren't counted in the
    /// `total_lp_shares` denominator when the bump was computed).
    ///
    /// Invariant pinned by this test: Σ_outstanding_claim == fee_lp for any
    /// single bump. Without post-bump pinning, a fresh LP would retroactively
    /// claim `(bump_per_share × new_shares)` from a bump that was only
    /// distributed across pre-deposit LPs → unfunded claim.
    #[test]
    fn zap_fresh_init_starts_at_post_bump_cumulative() {
        // Pre-bump state: 1 existing LP cohort with 100 shares, no fees yet.
        let mut pool = make_pool(0, 0, 100);

        // Simulate ZAP-internal swap: RWT on side B (input), fee_lp = 100.
        // This bumps cumulative_b by (100 << 64) / 100 = 1 << 64.
        let fee_lp = 100u64;
        crate::instructions::swap::accrue_lp_fee_per_share(&mut pool, fee_lp, false)
            .expect("accrue_lp_fee_per_share should not fail");

        // Copy fields to locals to work around packed struct alignment issues.
        let cumulative_a_post_bump = pool.cumulative_fees_per_share_a;
        let cumulative_b_post_bump = pool.cumulative_fees_per_share_b;

        // Verify the bump occurred on side B only.
        assert_eq!(cumulative_a_post_bump, 0u128, "non-RWT side should stay zero");
        let expected_b = 1u128 << 64; // (100 << 64) / 100 = 1 << 64
        assert_eq!(cumulative_b_post_bump, expected_b, "RWT side should have bumped");

        // Simulate fresh-init pinning: new LP's snapshot gets POST-bump cumulative.
        let new_lp = make_position(50, cumulative_a_post_bump, cumulative_b_post_bump);

        // Assert invariant: new LP has zero claimable from the bump.
        // delta_b = cumulative_b_post_bump - fees_claimed_b = 0
        let delta_a = cumulative_a_post_bump
            .checked_sub(new_lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b = cumulative_b_post_bump
            .checked_sub(new_lp.fees_claimed_per_share_b)
            .unwrap();
        let claimable_a = compute_claimable(delta_a, new_lp.shares).expect("compute_claimable");
        let claimable_b = compute_claimable(delta_b, new_lp.shares).expect("compute_claimable");

        assert_eq!(claimable_a, 0u64, "fresh LP should have zero claimable on side A");
        assert_eq!(claimable_b, 0u64, "fresh LP should have zero claimable on side B (post-bump pinning)");

        // Invariant check: sum of claimable across all LPs for this bump
        // should equal fee_lp. Pre-deposit LP with 100 shares at zero snapshot
        // would claim: (expected_b * 100) >> 64 = (1 << 64) * 100 >> 64 = 100.
        // New LP claims 0. Total = 100 = fee_lp. ✓
        let predeposit_lp = make_position(100, 0, 0);
        let predeposit_delta_b = cumulative_b_post_bump
            .checked_sub(predeposit_lp.fees_claimed_per_share_b)
            .unwrap();
        let predeposit_claimable = compute_claimable(predeposit_delta_b, predeposit_lp.shares)
            .expect("compute_claimable");
        assert_eq!(
            predeposit_claimable + claimable_b, fee_lp as u64,
            "total outstanding claim from bump should equal fee_lp"
        );
    }

    /// D29+ — `zap_liquidity` existing-position branch auto-claims full share
    /// of the zap-internal bump based on shares_pre, then advances snapshot to
    /// post-bump so new shares don't retroactively claim the same bump.
    ///
    /// This test exercises the invariant: a single zap-internal bump creates
    /// `fee_lp` total claim across all LPs present at bump time, split by
    /// `(bump_per_share × shares_pre)` per LP. After snapshot advance, new
    /// shares from the same depositor have zero delta against the post-bump
    /// cumulative, preventing double-claim.
    #[test]
    fn zap_existing_position_auto_claims_full_share_then_advances_to_post_bump() {
        // Pre-state: existing LP with 100 shares, fee snapshot at zero.
        // Pool total shares = 100, cumulative_b = 0 (no fees yet).
        let mut pool = make_pool(0, 0, 100);
        let mut lp = make_position(100, 0, 0);
        let shares_pre = lp.shares;

        // Simulate ZAP-internal swap: RWT on side B, fee_lp = 100.
        // Bump: cumulative_b = (100 << 64) / 100 = 1 << 64.
        let fee_lp = 100u64;
        crate::instructions::swap::accrue_lp_fee_per_share(&mut pool, fee_lp, false)
            .expect("accrue_lp_fee_per_share should not fail");

        // Copy fields to locals to work around packed struct alignment issues.
        let cumulative_a_post_bump = pool.cumulative_fees_per_share_a;
        let cumulative_b_post_bump = pool.cumulative_fees_per_share_b;

        // Simulate existing-position auto-claim: compute pending fees against
        // LIVE post-bump cumulative before incrementing shares.
        let delta_a = cumulative_a_post_bump
            .checked_sub(lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b = cumulative_b_post_bump
            .checked_sub(lp.fees_claimed_per_share_b)
            .unwrap();
        let auto_claim_a = compute_claimable(delta_a, shares_pre).expect("compute_claimable");
        let auto_claim_b = compute_claimable(delta_b, shares_pre).expect("compute_claimable");

        // Verify auto-claim includes the LP's full fair share of the bump.
        // delta_b = 1 << 64, shares_pre = 100
        // claimable = (1 << 64) * 100 >> 64 = 100
        assert_eq!(auto_claim_a, 0u64, "side A should have zero fees (no bump)");
        assert_eq!(
            auto_claim_b, 100u64,
            "side B auto-claim should be full fair share of bump"
        );

        // Simulate snapshot advance BEFORE share increment.
        lp.fees_claimed_per_share_a = cumulative_a_post_bump;
        lp.fees_claimed_per_share_b = cumulative_b_post_bump;

        // Simulate share increment: add 50 new shares.
        let new_shares = 50u128;
        lp.shares = lp.shares.checked_add(new_shares).unwrap();
        let shares_after_increment = lp.shares;
        assert_eq!(shares_after_increment, 150u128, "post-increment share count");

        // Verify invariant: after snapshot advance, new shares have zero delta
        // against the post-bump cumulative. This prevents them from
        // retroactively claiming against the same bump.
        let delta_a_after = cumulative_a_post_bump
            .checked_sub(lp.fees_claimed_per_share_a)
            .unwrap();
        let delta_b_after = cumulative_b_post_bump
            .checked_sub(lp.fees_claimed_per_share_b)
            .unwrap();
        let claimable_a_after = compute_claimable(delta_a_after, lp.shares).expect("compute_claimable");
        let claimable_b_after = compute_claimable(delta_b_after, lp.shares).expect("compute_claimable");

        assert_eq!(
            claimable_a_after, 0u64,
            "post-advance delta on side A should be zero"
        );
        assert_eq!(
            claimable_b_after, 0u64,
            "post-advance delta on side B should be zero (no retroactive claim)"
        );

        // Final invariant check: total claim from this single bump equals fee_lp.
        // The existing LP's auto_claim_b (100) + any future claim (0) = 100 = fee_lp.
        // New LP (if any) starting from post-bump cumulative would also have zero claimable.
        assert_eq!(
            auto_claim_b, fee_lp as u64,
            "total outstanding claim from bump must equal fee_lp"
        );
    }

    /// D29+ defensive — existing LpPosition with `shares == 0` must re-pin
    /// `fees_claimed_per_share_<side>` to the LIVE pool cumulative on next
    /// add/zap, NOT preserve the stale snapshot from when the LP last held
    /// shares. Otherwise, when the LP redeposits, the newly minted shares
    /// would retroactively claim all bumps that occurred during the
    /// zero-shares interval (drain).
    ///
    /// In normal flow `remove_liquidity` closes the LpPosition account when
    /// shares hit zero, so this branch is theoretically unreachable. This
    /// test pins the defensive `else` branch behavior to prevent regression
    /// if a future refactor breaks the close path.
    #[test]
    fn zap_existing_position_with_zero_shares_repins_to_current_cumulative() {
        // Pool has accumulated 100·2^64 in cumulative_b over time (from many
        // prior swaps). Side A had no fee activity for simplicity.
        let pool = make_pool(0, 100u128 << 64, 1_000_000);

        // Skeleton LpPosition: LP previously had shares, claimed at the time
        // when cumulative_b = 50·2^64, then fully removed. Account persists
        // (theoretical edge case; normal flow closes it).
        let mut lp = make_position(0, 0, 50u128 << 64);

        // Simulate the defensive `else` branch from zap_liquidity_internal:
        // when lp.shares == 0 on the existing-position path, re-pin
        // fees_claimed_per_share_<side> to the LIVE cumulative.
        if lp.shares == 0 {
            lp.fees_claimed_per_share_a = pool.cumulative_fees_per_share_a;
            lp.fees_claimed_per_share_b = pool.cumulative_fees_per_share_b;
        }

        // Now simulate the share increment.
        let new_shares: u128 = 100_000;
        lp.shares = lp.shares.checked_add(new_shares).unwrap();

        // Copy fields to locals to avoid packed-struct alignment issues.
        let lp_fees_claimed_b = lp.fees_claimed_per_share_b;
        let pool_cumulative_b = pool.cumulative_fees_per_share_b;

        // Re-pin worked: fees_claimed advanced to live cumulative (100·2^64),
        // not left at the stale 50·2^64.
        assert_eq!(
            lp_fees_claimed_b, 100u128 << 64,
            "fees_claimed_per_share_b should be re-pinned to live cumulative"
        );

        // Future claim attempt observes zero delta → zero claimable.
        let delta_b = pool_cumulative_b
            .checked_sub(lp_fees_claimed_b)
            .unwrap();
        assert_eq!(delta_b, 0u128, "delta should be zero after re-pin");
        let claimable_b = compute_claimable(delta_b, lp.shares).expect("compute_claimable");
        assert_eq!(
            claimable_b, 0u64,
            "no claimable fees after defensive re-pin"
        );

        // For comparison — the broken behavior we prevent: if defensive re-pin
        // were absent, `lp.fees_claimed_per_share_b` would remain `50 << 64`.
        // Then `delta = 50 << 64`, `claimable = (50 << 64 × 100_000) >> 64
        // = 5_000_000` tokens — funds the LP did NOT earn (those bumps were
        // distributed over OTHER LPs during the zero-shares period).
        let broken_delta_b: u128 = 50u128 << 64;
        let broken_claimable_b = compute_claimable(broken_delta_b, lp.shares)
            .expect("compute_claimable");
        assert_eq!(
            broken_claimable_b, 5_000_000u64,
            "broken path (no re-pin) would have drained 5M tokens"
        );
    }

    // ---------------------------------------------------------------------
    // Fee-on-top compliance (docs/contracts/native-dex.mdx:522-568)
    //
    // The three tests below pin the new zap-internal-swap semantics:
    //   1) Full swap_<rwt> enters reserves on the RWT side (NOT swap_<rwt> - fees).
    //   2) Inbound transfer on the RWT-input side is grossed up by
    //      `fee_lp + fee_protocol + fee_ot_treasury` (equivalently
    //      `fee_total + fee_ot_treasury`).
    //   3) Vault invariant after the full sequence holds:
    //        vault_<rwt>_after == reserve_<rwt>_after + Σ_unclaimed_fee_lp.
    // ---------------------------------------------------------------------

    /// New spec (docs/contracts/native-dex.mdx:565): in the zap-internal swap,
    /// the full `swap_<rwt>` amount enters reserves on the RWT side. Pure-math
    /// pin against `internal_swap` with `input_is_rwt = true`.
    #[test]
    fn zap_internal_swap_full_swap_amount_enters_reserves() {
        // Scenario: B is RWT (input). 1M / 1M pool, 30 bps fee, 50/50 split.
        let reserve_a_before: u64 = 1_000_000;
        let reserve_b_before: u64 = 1_000_000;
        let swap_b: u64 = 10_000;
        let fee_bps: u16 = 30;
        let lp_fee_share_bps: u16 = 5_000;
        let has_ot_treasury = false;

        // internal_swap with input_is_rwt = true now returns amount_out based on
        // the FULL swap_b entering the curve.
        let (swap_out, _flp, _fp, _fot) = internal_swap(
            swap_b,
            reserve_b_before,
            reserve_a_before,
            /* input_is_rwt */ true,
            fee_bps,
            lp_fee_share_bps,
            has_ot_treasury,
        )
        .expect("internal_swap should not fail");

        // Cross-check: amount_out matches the pure constant_product formula
        // with full swap_b as net_input.
        let expected_out = crate::amm::constant_product_output(reserve_b_before, reserve_a_before, swap_b)
            .expect("constant_product_output should not fail");
        assert_eq!(swap_out, expected_out);

        // Simulate the production reserve effects for the b_is_rwt branch:
        // full swap_b into reserve_b, swap_out out of reserve_a.
        let reserve_b_after = reserve_b_before + swap_b;
        let reserve_a_after = reserve_a_before - swap_out;

        // Pin: reserve_b_after == reserve_b_before + swap_b (full, no fee deduction).
        assert_eq!(
            reserve_b_after,
            reserve_b_before + swap_b,
            "fee-on-top: full swap_b must enter reserves"
        );
        // Sanity on the output side.
        assert!(reserve_a_after < reserve_a_before);
    }

    /// Pure-math pin for the inbound debit on the RWT-input side. With
    /// `b_is_rwt = true`, the transferred amount on side B equals
    /// `amount_b + fee_lp + fee_protocol + fee_ot_treasury`. Equivalently
    /// `amount_b + fee_total + fee_ot_treasury`.
    #[test]
    fn zap_inbound_debit_grossed_up_on_rwt_side() {
        // Realistic input amounts. We don't drive the full zap math here — we
        // pin the gross-up formula directly using `calculate_fees` against the
        // expected `swap_b` (the architect's spec asks for given `amount_a`,
        // `amount_b`, `swap_b`, `b_is_rwt = true`).
        let amount_a: u64 = 100_000;
        let amount_b: u64 = 110_000;
        let swap_b: u64 = 5_000;
        let fee_bps: u16 = 30;
        let lp_fee_share_bps: u16 = 5_000;
        let has_ot_treasury = true;

        // Sanity on the precondition.
        assert!(amount_b > amount_a);

        // Fees are computed on swap_b (the virtual zap-internal swap consumes
        // swap_b from the RWT-input side).
        let fees = calculate_fees(swap_b, fee_bps, lp_fee_share_bps, has_ot_treasury)
            .expect("calculate_fees should not fail");

        // Production formula for rwt_extra_b on the b_is_rwt branch.
        let rwt_extra_b = fees.fee_lp + fees.fee_protocol + fees.ot_treasury_fee;

        // Equivalent form using fee_total.
        let rwt_extra_b_alt = fees.fee_total + fees.ot_treasury_fee;
        assert_eq!(rwt_extra_b, rwt_extra_b_alt);

        // The inbound transfer on side B must include the gross-up.
        let inbound_b = amount_b + rwt_extra_b;
        assert_eq!(
            inbound_b,
            amount_b + fees.fee_total + fees.ot_treasury_fee,
            "RWT-input side must be grossed up by fee_total + fee_ot_treasury"
        );

        // Non-RWT side (A) stays untouched.
        let rwt_extra_a: u64 = 0;
        let inbound_a = amount_a + rwt_extra_a;
        assert_eq!(inbound_a, amount_a, "non-RWT side must not be grossed up");
    }

    /// Vault-invariant simulation for the zap path. Symmetric to
    /// `sell_rwt_post_swap_invariant` in swap.rs. Walk through inbound
    /// transfers, reserve effects, and outbound fee-extraction CPIs; assert
    /// that the RWT vault counter ends at
    ///   reserve_<rwt>_after + Σ_unclaimed_fee_lp.
    #[test]
    fn zap_post_swap_vault_invariant() {
        // Pool with RWT on side B, clean vault state.
        let reserve_a_before: u64 = 1_000_000;
        let reserve_b_before: u64 = 1_000_000;
        // vault_a is tracked symbolically below (no fee accumulator activity on
        // the non-RWT side); only vault_b is exercised for the invariant.
        let mut vault_b: u64 = reserve_b_before;

        // User zaps amount_a and amount_b. amount_b > amount_a (Excess B path).
        let amount_a: u64 = 50_000;
        let amount_b: u64 = 70_000;
        let fee_bps: u16 = 30;
        let lp_fee_share_bps: u16 = 5_000;
        let has_ot_treasury = true;

        // Production zap math derives swap_b from excess_b (the half-excess
        // heuristic). For the invariant test we only need a concrete swap_b
        // and the production fee gross-up; the slice of math we don't drive
        // here (final_a/final_b/dep_a/dep_b) operates entirely on `final_<a/b>`
        // which net out of `amount_<a/b>` — the vault counter accounts for
        // `amount_<a/b>` directly, so we are safe pinning just the swap leg.
        let value_a_in_b: u64 = (amount_a as u128 * reserve_b_before as u128
            / reserve_a_before as u128) as u64;
        let excess_b = amount_b - value_a_in_b;
        let swap_b = excess_b / 2;
        assert!(swap_b > 0, "fixture must hit the actual swap branch");

        // Fees on swap_b.
        let fees = calculate_fees(swap_b, fee_bps, lp_fee_share_bps, has_ot_treasury)
            .expect("calculate_fees should not fail");
        let rwt_extra_b = fees.fee_lp + fees.fee_protocol + fees.ot_treasury_fee;

        // Inbound transfers (fee-on-top on RWT side B).
        let _inbound_a = amount_a; // non-RWT side, no fee gross-up
        let inbound_b = amount_b + rwt_extra_b;
        vault_b += inbound_b;

        // Reserve effects (Excess-B, b_is_rwt branch):
        // - reserve_b += swap_b (full, fee-on-top)
        // - reserve_a -= swap_out (gross curve output)
        let swap_out = crate::amm::constant_product_output(reserve_b_before, reserve_a_before, swap_b)
            .expect("constant_product_output should not fail");
        let mut reserve_a_after = reserve_a_before - swap_out;
        let reserve_b_after = reserve_b_before + swap_b;

        // The zap also deposits final_<a/b> into reserves. For invariant
        // tracking we treat these as `amount_<a/b> - swap_<rwt>` movements
        // (the final_<a/b> additions cancel against the implicit amounts
        // already added via the inbound). To keep the test self-contained,
        // restrict the invariant check to the swap-leg deltas only by
        // subtracting `amount_a` and `(amount_b - swap_b)` from both sides.
        // Equivalent: the swap leg's vault delta on side B is
        //   inbound_b - swap_b  (rest goes into reserve_b directly via final_b).
        // Reserve-b delta from the swap leg is swap_b.
        // After the swap leg: reserve_b grew by swap_b, vault_b grew by inbound_b.

        // Outbound CPIs extract fee_protocol + fee_ot_treasury from the RWT
        // vault (B). fee_lp stays in the vault (tracked off-reserve by the
        // per-share accumulator).
        vault_b -= fees.fee_protocol;
        vault_b -= fees.ot_treasury_fee;

        // For the swap leg only: deposit step adds amount_a + (amount_b - swap_b)
        // to reserves; the swap leg already moved swap_b into reserves above.
        // We expose only the swap-leg vault balance vs reserve balance:
        let swap_leg_vault_b = vault_b - (amount_b - swap_b); // remove the final_b leg
        let swap_leg_reserve_b = reserve_b_after;

        // Σ_unclaimed_fee_lp == fee_lp (only one swap, starting accumulator zero).
        let sum_unclaimed_fee_lp = fees.fee_lp;
        assert_eq!(
            swap_leg_vault_b,
            swap_leg_reserve_b + sum_unclaimed_fee_lp,
            "vault_b on the swap leg must equal reserve_b + Σ_unclaimed_fee_lp (RWT side)"
        );

        // Non-RWT side (A) has no fee accumulator activity.
        reserve_a_after += amount_a; // deposit leg adds amount_a to reserve_a
        let _ = reserve_a_after; // pin reserve_a path is symmetric and uneventful
    }

    // ----- CP-5 MasterPoolUserLpDisabled guard ---------------------------

    /// Decode `ProgramError::Custom` into the inner u32 so tests can match
    /// against `DexError` discriminants without depending on the exact
    /// numeric base.
    fn custom_code(err: arlex_lang::prelude::ProgramError) -> u32 {
        match err {
            arlex_lang::prelude::ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    fn code_of(err: DexError) -> u32 {
        custom_code(arlex_lang::prelude::ProgramError::from(err))
    }

    /// CP-5 — `zap_liquidity` MUST reject master (Monotonic Ladder)
    /// pools with `MasterPoolUserLpDisabled`. Same surface as
    /// `add_liquidity`: master pools have no user-LP path, only
    /// Nexus and the Pool Rebalancer.
    /// Pinned via the shared `enforce_user_lp_allowed` helper so a
    /// refactor of either handler cannot drift their rejection codes
    /// out of sync.
    #[test]
    fn rejects_master_pool_zap_liquidity() {
        let err =
            crate::instructions::add_liquidity::enforce_user_lp_allowed(POOL_TYPE_CONCENTRATED)
                .unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::MasterPoolUserLpDisabled));
    }

    /// CP-5 — StandardCurve pools still accept user zap-liquidity. Sanity
    /// that the guard doesn't accidentally widen its rejection to the
    /// non-master path.
    #[test]
    fn accepts_standard_curve_zap_liquidity() {
        assert!(
            crate::instructions::add_liquidity::enforce_user_lp_allowed(POOL_TYPE_STANDARD)
                .is_ok()
        );
    }
}
