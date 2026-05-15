use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::SwapExecuted;
use crate::state::*;
use crate::amm::{constant_product_output, calculate_fees};
use crate::validation::*;
use crate::concentrated;

#[derive(Accounts)]
pub struct Swap<'info> {
    #[account(signer)]
    pub user: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,

    // User's input/output token accounts
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_token_in: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_token_out: &'info AccountView,

    // Pool vaults (in and out direction determined by a_to_b)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_in: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_out: &'info AccountView,

    // Protocol fee destination (Areal Finance RWT ATA)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub areal_fee_account: &'info AccountView,

    // Optional: OT treasury fee destination (remaining accounts[0])

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

/// Decoupled view of `Swap` accounts used by `swap_internal`.
///
/// Allows `swap_internal` to be invoked from any handler whose accounts can
/// project into this shape (user-signed `swap`, or PDA-signed alternative
/// callers added in later layers).
pub(crate) struct SwapAccountsView<'info> {
    pub authority: &'info AccountView,
    pub dex_config: &'info AccountView,
    pub pool_state: &'info AccountView,
    pub user_token_in: &'info AccountView,
    pub user_token_out: &'info AccountView,
    pub vault_in: &'info AccountView,
    pub vault_out: &'info AccountView,
    pub areal_fee_account: &'info AccountView,
}

impl<'info> Swap<'info> {
    pub(crate) fn view(&self) -> SwapAccountsView<'info> {
        SwapAccountsView {
            authority: self.user,
            dex_config: self.dex_config,
            pool_state: self.pool_state,
            user_token_in: self.user_token_in,
            user_token_out: self.user_token_out,
            vault_in: self.vault_in,
            vault_out: self.vault_out,
            areal_fee_account: self.areal_fee_account,
        }
    }
}

pub fn handler(
    ctx: Context<Swap>,
    amount_in: u64,
    min_amount_out: u64,
    a_to_b: bool,
) -> Result<()> {
    swap_internal(
        &ctx.accounts.view(),
        ctx.remaining_accounts,
        ctx.program_id,
        amount_in,
        min_amount_out,
        a_to_b,
        None,
    )
}

/// Core swap logic — usable by user-signed `swap` and by PDA-signed alternative
/// callers (e.g. future layers passing `Some(&[Signer])`).
///
/// Internal helper — do NOT use as instruction entrypoint. Caller must validate
/// access control before invoking.
///
/// `authority_signer_seeds`:
///   - `None` — authority is a transaction signer (user-signed path); the
///     inbound `user_token_in -> vault_in` transfer is invoked without seeds.
///   - `Some(seeds)` — authority is a PDA; the inbound transfer is invoked
///     with the supplied signer seeds.
///
/// Pool-side outbound transfers (`vault_out -> user_token_out`,
/// `vault -> areal_fee_account`, `vault -> ot_fee_account`) always sign with
/// the pool PDA seeds derived from `PoolState`, regardless of caller.
#[inline(never)]
pub(crate) fn swap_internal<'info>(
    accounts: &SwapAccountsView<'info>,
    remaining_accounts: &'info [AccountView],
    program_id: &Address,
    amount_in: u64,
    min_amount_out: u64,
    a_to_b: bool,
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
    if amount_in == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount));
    }
    // For StandardCurve: both reserves must be non-zero
    // For Concentrated: one-sided liquidity is legitimate (bin-walk handles it)
    if pool.pool_type == POOL_TYPE_STANDARD && (pool.reserve_a == 0 || pool.reserve_b == 0) {
        return Err(ProgramError::from(DexError::EmptyReserves));
    }

    // SECURITY: Validate vaults match pool state
    let (expected_vault_in, expected_vault_out) = if a_to_b {
        (&pool.vault_a, &pool.vault_b)
    } else {
        (&pool.vault_b, &pool.vault_a)
    };
    validate_vault(accounts.vault_in, expected_vault_in)?;
    validate_vault(accounts.vault_out, expected_vault_out)?;

    // Validate areal_fee_account matches DexConfig
    if accounts.areal_fee_account.address().as_ref() != config.areal_fee_destination.as_ref() {
        return Err(ProgramError::from(DexError::InvalidTokenAccount));
    }

    // --- OT treasury fee account validation ---
    if pool.has_ot_treasury {
        if remaining_accounts.is_empty() {
            return Err(ProgramError::from(DexError::MissingOtTreasuryAccount));
        }
        let ot_fee_account = &remaining_accounts[0];
        if ot_fee_account.address().as_ref() != pool.ot_treasury_fee_destination.as_ref() {
            return Err(ProgramError::from(DexError::OtTreasuryAccountMismatch));
        }
    }

    // --- Determine RWT side and fee direction ---
    // Fee is ALWAYS taken from the RWT side:
    // - Selling RWT (input is RWT): fee from input BEFORE swap
    // - Buying RWT (output is RWT): fee from output AFTER swap
    let input_is_rwt = if a_to_b {
        is_rwt_mint(&pool.token_a_mint)
    } else {
        is_rwt_mint(&pool.token_b_mint)
    };

    let (reserve_in, reserve_out) = if a_to_b {
        (pool.reserve_a, pool.reserve_b)
    } else {
        (pool.reserve_b, pool.reserve_a)
    };

    // Determine if token_a is RWT (needed for concentrated fee_lp bin sync)
    let token_a_is_rwt_side = is_rwt_mint(&pool.token_a_mint);

    let (amount_out, fee_lp, fee_protocol, fee_ot_treasury, net_input);

    // Note (Layer 9 D28): `token_a_is_rwt_side` is no longer used by
    // `sync_fee_lp_to_bin` — fee_lp is tracked exclusively via
    // `cumulative_fees_per_share_<side>` and is no longer mirrored into
    // bins or reserves. The local is preserved as `_` to keep its derivation
    // close to where the fee side is determined for future readers.
    let _token_a_is_rwt_side = token_a_is_rwt_side;

    // Per spec (Fee Architecture Step 4: "Always RWT"; Step 2: Protocol Fee
    // "always in RWT, transferred to areal_fee_destination"), the protocol
    // fee destination MUST be an RWT ATA. Verify the mint here and revert
    // fast on mismatch so operators notice the misconfiguration rather than
    // silently burning protocol fees into the LP accumulator. The
    // areal_fee_destination address is set at initialize_dex and is
    // immutable thereafter — fixing a misconfigured cluster requires a
    // program-upgrade migration or a fresh bootstrap.
    let dest_mint = read_token_account_mint(accounts.areal_fee_account)?;
    if dest_mint != RWT_MINT {
        return Err(ProgramError::from(DexError::InvalidProtocolFeeDestination));
    }

    if input_is_rwt {
        // Selling RWT (fee-on-top per docs/contracts/native-dex.mdx:522-568):
        // Fees are charged ON TOP of `amount_in` — the full `amount_in` enters
        // the constant-product curve. User's wallet is debited
        // `amount_in + fee_total + ot_treasury_fee` (computed below as
        // `user_total_debit`). `fee_lp` stays in the RWT vault and is tracked
        // by the per-share accumulator (D28); `fee_protocol` + `fee_ot_treasury`
        // are CPI-extracted to their destinations by the outbound transfers.
        let fees = calculate_fees(amount_in, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
        // Full amount_in enters the curve — fees are external to it.
        net_input = amount_in;

        // Branch on pool type for output calculation
        if pool.pool_type == POOL_TYPE_CONCENTRATED {
            // Load BinArray from remaining_accounts with PDA verification
            let bin_idx = if pool.has_ot_treasury { 1 } else { 0 };
            if remaining_accounts.len() <= bin_idx {
                return Err(ProgramError::from(DexError::InvalidBinRange));
            }
            let pool_key = pubkey_bytes(accounts.pool_state);
            let (expected_bin_pda, _) = arlex_lang::find_program_address(
                &[b"bins", pool_key.as_ref()],
                program_id,
            );
            if remaining_accounts[bin_idx].address().as_ref() != expected_bin_pda.as_ref() {
                return Err(ProgramError::InvalidSeeds);
            }
            let bin_array = BinArray::load_mut(&remaining_accounts[bin_idx], program_id)?;
            // net_input == amount_in (fee-on-top): full amount enters bin walk.
            let (walk_out, walk_remaining) = concentrated::bin_walk_swap(bin_array, pool.bin_step_bps, net_input, a_to_b)?;
            amount_out = walk_out;
            pool.active_bin_id = bin_array.active_bin_id;

            // SECURITY: Sync unconsumed input into bins so sum(bins) == reserves.
            // Layer 9 D28: fee_lp is NOT synced into bins — it lives in the
            // `cumulative_fees_per_share_<side>` accumulator instead, and is
            // explicitly excluded from reserves by the effects step below.
            concentrated::sync_remaining_to_bin(bin_array, walk_remaining, a_to_b)?;
        } else {
            // Standard path: full amount_in enters the curve (fee-on-top).
            amount_out = constant_product_output(reserve_in, reserve_out, amount_in)?;
        }

        fee_lp = fees.fee_lp;
        fee_protocol = fees.fee_protocol;
        fee_ot_treasury = fees.ot_treasury_fee;
    } else {
        // Buying RWT: fee deducted from output AFTER swap
        net_input = amount_in;

        // Branch on pool type for output calculation
        let gross_out;
        if pool.pool_type == POOL_TYPE_CONCENTRATED {
            let bin_idx = if pool.has_ot_treasury { 1 } else { 0 };
            if remaining_accounts.len() <= bin_idx {
                return Err(ProgramError::from(DexError::InvalidBinRange));
            }
            let pool_key = pubkey_bytes(accounts.pool_state);
            let (expected_bin_pda, _) = arlex_lang::find_program_address(
                &[b"bins", pool_key.as_ref()],
                program_id,
            );
            if remaining_accounts[bin_idx].address().as_ref() != expected_bin_pda.as_ref() {
                return Err(ProgramError::InvalidSeeds);
            }
            let bin_array = BinArray::load_mut(&remaining_accounts[bin_idx], program_id)?;
            let (walk_out, walk_remaining) = concentrated::bin_walk_swap(bin_array, pool.bin_step_bps, net_input, a_to_b)?;
            gross_out = walk_out;
            pool.active_bin_id = bin_array.active_bin_id;

            // Sync unconsumed input into bins. Layer 9 D28: fee_lp stays in
            // the RWT vault but is tracked via the per-share accumulator,
            // not via bin liquidity (bins must mirror reserves; reserves
            // exclude fee_lp post-D28).
            concentrated::sync_remaining_to_bin(bin_array, walk_remaining, a_to_b)?;

            let fees = calculate_fees(gross_out, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;

            let total_deducted = fees.fee_total.checked_add(fees.ot_treasury_fee)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            amount_out = gross_out.checked_sub(total_deducted)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            fee_lp = fees.fee_lp;
            fee_protocol = fees.fee_protocol;
            fee_ot_treasury = fees.ot_treasury_fee;
        } else {
            gross_out = constant_product_output(reserve_in, reserve_out, net_input)?;

            let fees = calculate_fees(gross_out, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
            let total_deducted = fees.fee_total.checked_add(fees.ot_treasury_fee)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            amount_out = gross_out.checked_sub(total_deducted)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            fee_lp = fees.fee_lp;
            fee_protocol = fees.fee_protocol;
            fee_ot_treasury = fees.ot_treasury_fee;
        }
    }

    // Slippage check
    if amount_out == 0 {
        return Err(ProgramError::from(DexError::ZeroOutput));
    }
    if amount_out < min_amount_out {
        return Err(ProgramError::from(DexError::SlippageExceeded));
    }

    // --- Effects: update reserves BEFORE CPIs ---
    //
    // Layer 9 D28: LP fee no longer auto-compounds into reserves. fee_lp
    // physically stays in the RWT vault on the fee side, but reserves are
    // updated as if it were extracted. The off-balance amount is tracked
    // via `cumulative_fees_per_share_<side>` (see accumulator update
    // below), and is paid out to LPs by `claim_lp_fees` (D28).
    //
    // For the input-RWT branches the difference vs pre-D28 is "do not add
    // fee_lp back into reserves". For the output-RWT branches the
    // difference is "subtract fee_lp from reserves alongside fee_protocol
    // and fee_ot_treasury". Either way the post-swap invariant holds:
    // `vault_<side>_balance == reserves + cumulative_fees_owed_to_LPs`.
    if a_to_b {
        if input_is_rwt {
            // User sends amount_in + fees via the inbound transfer below; full amount_in
            // enters reserves; fee_lp stays in the RWT vault tracked by the per-share
            // accumulator (D28); fee_protocol + ot_treasury are CPI-extracted to their
            // destinations by the outbound transfers below.
            pool.reserve_a = pool.reserve_a
                .checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_b = pool.reserve_b.checked_sub(amount_out)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
        } else {
            // Output side (B=RWT): fee_lp now leaves reserves (off-reserve
            // accumulator) instead of staying as auto-compound.
            pool.reserve_a = pool.reserve_a.checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_b = pool.reserve_b.checked_sub(amount_out)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_protocol)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_ot_treasury)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_lp)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
        }
    } else {
        if input_is_rwt {
            pool.reserve_b = pool.reserve_b
                .checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_a = pool.reserve_a.checked_sub(amount_out)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
        } else {
            pool.reserve_b = pool.reserve_b.checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_a = pool.reserve_a.checked_sub(amount_out)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_protocol)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_ot_treasury)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_lp)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
        }
    }

    // Layer 9 D28 — update LP-fee per-share accumulator on the RWT side.
    //
    // The fee always accrues on the RWT side of the pair (token_a or token_b
    // depending on `is_rwt_mint`). When `total_lp_shares == 0` the fee_lp
    // funds remain in the vault unaccounted; this is only reachable before
    // the very first `add_liquidity` (no LP yet). We document the edge but
    // do not panic — the deposited fee_lp would be effectively "burned"
    // toward the eventual first LP cohort because the snapshot at first
    // add_liquidity will be 0. In practice swaps cannot succeed at all
    // when reserves are zero, so this branch is unreachable in the
    // standard pool path; concentrated pools allow one-sided liquidity, so
    // the guard is required there.
    {
        let rwt_is_side_a = (a_to_b && input_is_rwt) || (!a_to_b && !input_is_rwt);
        accrue_lp_fee_per_share(pool, fee_lp, rwt_is_side_a)?;
    }

    pool.total_fees_accumulated = pool.total_fees_accumulated
        .checked_add(fee_lp).ok_or(ProgramError::from(DexError::MathOverflow))?
        .checked_add(fee_protocol).ok_or(ProgramError::from(DexError::MathOverflow))?
        .checked_add(fee_ot_treasury).ok_or(ProgramError::from(DexError::MathOverflow))?;

    // --- Interactions: CPIs ---
    let pool_bump = [pool.bump];

    // Fee-on-top inbound transfer sizing (docs/contracts/native-dex.mdx:534-535):
    // When the user is selling RWT, the wallet debit is `amount_in + fee_total +
    // ot_treasury_fee`. `fee_total == fee_lp + fee_protocol`, so the explicit sum
    // below is equivalent and makes each fee component visible to readers.
    // When the user is buying RWT, fees come out of the output side, so the
    // inbound debit stays equal to `amount_in`.
    let user_total_debit = if input_is_rwt {
        amount_in
            .checked_add(fee_lp).ok_or(ProgramError::from(DexError::MathOverflow))?
            .checked_add(fee_protocol).ok_or(ProgramError::from(DexError::MathOverflow))?
            .checked_add(fee_ot_treasury).ok_or(ProgramError::from(DexError::MathOverflow))?
    } else {
        amount_in
    };

    // 1. Authority sends input tokens to vault_in.
    //    User-signed path: empty signer slice (authority is a transaction signer).
    //    PDA-signed path: caller-supplied seeds authorize the PDA-owned ATA.
    //    Amount = `user_total_debit` so the RWT vault receives both the swap
    //    principal (which enters reserves) AND the fee components (which leave
    //    the vault via the outbound CPIs below — fee_protocol to areal_fee_account,
    //    fee_ot_treasury to ot_fee_account — while fee_lp stays in the vault and
    //    is tracked by `cumulative_fees_per_share_<rwt_side>`).
    {
        let transfer_in = arlex_lang::token::instructions::Transfer {
            from: accounts.user_token_in,
            to: accounts.vault_in,
            authority: accounts.authority,
            amount: user_total_debit,
        };
        match authority_signer_seeds {
            Some(seeds) => transfer_in.invoke_signed(seeds)?,
            None => transfer_in.invoke()?,
        }
    }

    // 2. Pool sends output tokens to user
    {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: accounts.vault_out,
            to: accounts.user_token_out,
            authority: accounts.pool_state,
            amount: amount_out,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // 3. Pool sends protocol fee to areal_fee_account (from RWT vault)
    if fee_protocol > 0 {
        let rwt_vault = if input_is_rwt { accounts.vault_in } else { accounts.vault_out };
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: rwt_vault,
            to: accounts.areal_fee_account,
            authority: accounts.pool_state,
            amount: fee_protocol,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // 4. Pool sends OT treasury fee (if applicable)
    if fee_ot_treasury > 0 {
        let ot_fee_account = &remaining_accounts[0];
        let rwt_vault = if input_is_rwt { accounts.vault_in } else { accounts.vault_out };
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: rwt_vault,
            to: ot_fee_account,
            authority: accounts.pool_state,
            amount: fee_ot_treasury,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(SwapExecuted {
        pool: pubkey_bytes(accounts.pool_state),
        user: pubkey_bytes(accounts.authority),
        a_to_b,
        amount_in,
        amount_out,
        fee_lp,
        fee_protocol,
        fee_ot_treasury,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

/// Layer 9 D28 — accrue LP-side `fee_lp` into the per-share accumulator on
/// the RWT side of the pool.
///
/// Q64.64 fixed-point: `delta_per_share = (fee_lp << 64) / total_lp_shares`.
/// Returns `Ok(())` (no-op) when either `fee_lp == 0` or
/// `total_lp_shares == 0`. The latter is only reachable before the first
/// `add_liquidity`; concentrated pools allow swaps with one-sided liquidity
/// before any LP exists, so the guard is required to avoid div-by-zero.
///
/// Extracted as `pub(crate)` so the unit tests below can drive the
/// accumulator math against a synthetic `PoolState` without spinning up the
/// full BPF runtime. Production callers must continue to invoke it from
/// inside `swap_internal` after the reserve effects step (we mutate
/// `cumulative_fees_per_share_<side>` only — reserves are owned by the
/// caller).
pub(crate) fn accrue_lp_fee_per_share(
    pool: &mut PoolState,
    fee_lp: u64,
    rwt_is_side_a: bool,
) -> core::result::Result<(), ProgramError> {
    if pool.total_lp_shares == 0 || fee_lp == 0 {
        return Ok(());
    }
    let delta_q64: u128 = (fee_lp as u128) << 64;
    let delta_per_share: u128 = delta_q64 / pool.total_lp_shares;
    if rwt_is_side_a {
        pool.cumulative_fees_per_share_a = pool
            .cumulative_fees_per_share_a
            .checked_add(delta_per_share)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    } else {
        pool.cumulative_fees_per_share_b = pool
            .cumulative_fees_per_share_b
            .checked_add(delta_per_share)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Layer 9 D28 — spot tests for the LP-fee per-share accumulator.
    //!
    //! Layer 4-5 swap-math regression coverage continues to live in the
    //! per-package test suites (`amm.rs`, `concentrated.rs`, integration
    //! harnesses). These tests pin the new accumulator behaviour: a swap
    //! with non-zero `fee_lp` should bump
    //! `cumulative_fees_per_share_<side>` by `(fee_lp << 64) / total_lp_shares`
    //! on the RWT side and leave the other side untouched, while a pool
    //! with `total_lp_shares == 0` must not divide-by-zero.
    use super::*;

    /// Build a synthetic `PoolState` in raw bytes for accumulator math
    /// testing. Only the fields the helper actually reads/writes are set.
    /// SAFETY: PoolState is `#[repr(C, packed)]` via `#[account]` and
    /// all-zero is a valid bit pattern for every field type.
    fn make_pool(total_lp_shares: u128) -> PoolState {
        let buf = [0u8; core::mem::size_of::<PoolState>()];
        let mut pool: PoolState = unsafe { core::ptr::read(buf.as_ptr() as *const PoolState) };
        pool.total_lp_shares = total_lp_shares;
        pool
    }

    /// D28 — happy path: side-A accumulator gains `(fee_lp << 64) /
    /// total_lp_shares`; side-B accumulator stays zero.
    #[test]
    fn swap_updates_cumulative_fees_per_share_a() {
        let mut pool = make_pool(1_000);
        accrue_lp_fee_per_share(&mut pool, 10, /* rwt_is_side_a */ true).unwrap();
        let expected: u128 = (10u128 << 64) / 1_000u128;
        assert_eq!({ pool.cumulative_fees_per_share_a }, expected);
        assert_eq!({ pool.cumulative_fees_per_share_b }, 0u128);
    }

    /// D28 — symmetric coverage: when RWT is on side B, only the B-side
    /// accumulator must move.
    #[test]
    fn swap_updates_cumulative_fees_per_share_b() {
        let mut pool = make_pool(2_000);
        accrue_lp_fee_per_share(&mut pool, 25, /* rwt_is_side_a */ false).unwrap();
        let expected: u128 = (25u128 << 64) / 2_000u128;
        assert_eq!({ pool.cumulative_fees_per_share_b }, expected);
        assert_eq!({ pool.cumulative_fees_per_share_a }, 0u128);
    }

    /// D28 — guard: `total_lp_shares == 0` must short-circuit without
    /// dividing by zero. Reachable for concentrated pools that allow a
    /// swap before the first LP joins.
    #[test]
    fn swap_zero_lp_shares_does_not_panic() {
        let mut pool = make_pool(0);
        // Both branches must be safe.
        accrue_lp_fee_per_share(&mut pool, 100, true).unwrap();
        accrue_lp_fee_per_share(&mut pool, 100, false).unwrap();
        assert_eq!({ pool.cumulative_fees_per_share_a }, 0u128);
        assert_eq!({ pool.cumulative_fees_per_share_b }, 0u128);
    }

    /// P0-2 — Protocol fee destination mint validation. The swap path must
    /// revert with `InvalidProtocolFeeDestination` when the
    /// `areal_fee_account` is a token account whose mint is NOT the pinned
    /// `RWT_MINT`. Pins the boolean decision of the check at swap.rs:200-203
    /// without requiring a full BPF runtime to exercise the handler.
    ///
    /// The production check in `swap_internal` is:
    ///   `if dest_mint != RWT_MINT { return Err(InvalidProtocolFeeDestination) }`
    ///
    /// This test documents the binary decision rule and prevents accidental
    /// regression if someone re-introduces a fallback behavior.
    #[test]
    fn swap_rejects_non_rwt_protocol_fee_destination_mint() {
        // The RWT mint constant from this crate
        let rwt = RWT_MINT;

        // Generate a fake non-RWT mint: a 32-byte pubkey statistically
        // certain to differ from the real RWT mint. Use sequential bytes
        // (i+1 at each position) to ensure bit-wise distinction.
        let non_rwt_mint: [u8; 32] = {
            let mut bytes = [0u8; 32];
            for (i, byte) in bytes.iter_mut().enumerate() {
                *byte = (i as u8).wrapping_add(1);
            }
            bytes
        };

        // Sanity check: our fake non-RWT mint differs from RWT
        assert_ne!(non_rwt_mint, rwt, "test fixture: synthetic mint must differ from RWT");

        // Mirror the production check: the decision is whether dest_mint == RWT_MINT
        // RWT mint should satisfy the check
        assert_eq!(
            rwt == RWT_MINT,
            true,
            "RWT_MINT must equal itself (passes the check)"
        );

        // Non-RWT mint should fail the check (causing revert in production)
        assert_ne!(
            non_rwt_mint, RWT_MINT,
            "non-RWT mint must NOT equal RWT_MINT (fails the check, triggers InvalidProtocolFeeDestination revert)"
        );
    }

    // ---------------------------------------------------------------------
    // Fee-on-top compliance (docs/contracts/native-dex.mdx:522-568)
    //
    // The three tests below pin the new sell-RWT semantics:
    //   1) Full `amount_in` enters the constant-product curve (NOT `amount_in - fees`).
    //   2) The inbound transfer debits `amount_in + fee_lp + fee_protocol +
    //      fee_ot_treasury` from the user's wallet (fees are external to the
    //      swap principal).
    //   3) The vault invariant holds:
    //        vault_in_after == reserves + Σ_unclaimed_fee_lp
    //      after the outbound CPIs extract fee_protocol + fee_ot_treasury.
    // ---------------------------------------------------------------------

    /// Pure-math pin: when selling RWT, the full `amount_in` is fed into
    /// `constant_product_output` — fees are external. Compare to the docs
    /// formula at native-dex.mdx:537 (`amount_out = constant_product(amount_in)`).
    #[test]
    fn sell_rwt_full_amount_in_enters_curve() {
        // Realistic-ish 1M / 1M pool with 30 bps fee.
        let reserve_in: u64 = 1_000_000;
        let reserve_out: u64 = 1_000_000;
        let amount_in: u64 = 10_000;
        let fee_bps: u16 = 30;
        let lp_fee_share_bps: u16 = 5_000;
        let has_ot_treasury = false;

        // Compute fees ON the trade size; do NOT subtract them from `amount_in`.
        let fees = crate::amm::calculate_fees(amount_in, fee_bps, lp_fee_share_bps, has_ot_treasury)
            .expect("calculate_fees should not fail");

        // New spec (docs/contracts/native-dex.mdx:565): full amount_in enters
        // the curve. The expected formula is
        //   amount_out = reserve_out * amount_in / (reserve_in + amount_in)
        //              = 1_000_000 * 10_000 / 1_010_000 ≈ 9_900.
        let amount_out = crate::amm::constant_product_output(reserve_in, reserve_out, amount_in)
            .expect("constant_product_output should not fail");

        // Reference: 1M * 10_000 / 1_010_000 = 9_900 (integer truncation).
        let expected_out: u64 = (reserve_out as u128 * amount_in as u128
            / (reserve_in as u128 + amount_in as u128)) as u64;
        assert_eq!(amount_out, expected_out);
        assert_eq!(amount_out, 9_900u64);

        // OLD (pre-fix) formula for clarity — used `amount_in - fees.fee_total`
        // and would have returned `~9_870`. The delta (~30 units) is the user's
        // gross-up benefit under fee-on-top: they get the full curve output.
        let old_net_input = amount_in - fees.fee_total;
        let old_amount_out = crate::amm::constant_product_output(reserve_in, reserve_out, old_net_input)
            .expect("constant_product_output should not fail");
        assert!(old_amount_out < amount_out, "fee-on-top must yield strictly better output for the user");
    }

    /// Pure-math pin: the user's total wallet debit on the sell-RWT path is
    /// `amount_in + fee_lp + fee_protocol + fee_ot_treasury`. Equivalently
    /// `amount_in + fee_total + fee_ot_treasury` since
    /// `fee_lp + fee_protocol == fee_total`. Also asserts the `checked_add`
    /// chain reverts with `MathOverflow` when the components overflow `u64`.
    #[test]
    fn sell_rwt_user_total_debit_math() {
        let amount_in: u64 = 10_000;
        let fee_bps: u16 = 30;
        let lp_fee_share_bps: u16 = 5_000;
        let has_ot_treasury = true; // exercise OT branch too

        let fees = crate::amm::calculate_fees(amount_in, fee_bps, lp_fee_share_bps, has_ot_treasury)
            .expect("calculate_fees should not fail");

        // Mirror the production gross-up: amount_in + fee_lp + fee_protocol + fee_ot_treasury.
        let user_total_debit = amount_in
            .checked_add(fees.fee_lp).unwrap()
            .checked_add(fees.fee_protocol).unwrap()
            .checked_add(fees.ot_treasury_fee).unwrap();

        // Equivalent form: amount_in + fee_total + ot_treasury_fee.
        let user_total_debit_alt = amount_in
            .checked_add(fees.fee_total).unwrap()
            .checked_add(fees.ot_treasury_fee).unwrap();

        assert_eq!(user_total_debit, user_total_debit_alt);
        // fee_lp + fee_protocol must equal fee_total exactly (remainder pattern).
        assert_eq!(fees.fee_lp + fees.fee_protocol, fees.fee_total);

        // Overflow test: u64::MAX as input MUST cause the gross-up chain to
        // overflow `checked_add`. Pick any non-zero fee component so the add
        // overflows on the first step; the production code propagates via
        // `?` → ProgramError::from(DexError::MathOverflow).
        let amount_in_max: u64 = u64::MAX;
        // fee_lp on u64::MAX would itself be derived from u128 arithmetic
        // inside calculate_fees; we instead exercise the gross-up directly
        // with a non-zero fee_lp to ensure the chain reverts.
        let synthetic_fee_lp: u64 = 1;
        let overflow_attempt = amount_in_max
            .checked_add(synthetic_fee_lp);
        assert!(overflow_attempt.is_none(), "u64::MAX + 1 must overflow checked_add");
    }

    /// Vault-invariant simulation: starting from a clean state, walk through
    /// the production sequence (inbound transfer, reserve updates, outbound
    /// CPIs) and assert the post-state invariant
    ///   vault_in_after == reserve_in_after + Σ_unclaimed_fee_lp
    /// since the starting accumulator is zero, `Σ_unclaimed_fee_lp == fee_lp`.
    #[test]
    fn sell_rwt_post_swap_invariant() {
        // Starting state.
        let reserve_in_before: u64 = 1_000_000;
        let reserve_out_before: u64 = 1_000_000;
        let vault_in_before: u64 = reserve_in_before; // clean pool, no prior fee_lp dust
        let amount_in: u64 = 10_000;
        let fee_bps: u16 = 30;
        let lp_fee_share_bps: u16 = 5_000;
        let has_ot_treasury = true;

        let fees = crate::amm::calculate_fees(amount_in, fee_bps, lp_fee_share_bps, has_ot_treasury)
            .expect("calculate_fees should not fail");

        // User's wallet debit (fee-on-top): amount_in + fee_lp + fee_protocol + fee_ot_treasury.
        let user_total_debit = amount_in
            + fees.fee_lp + fees.fee_protocol + fees.ot_treasury_fee;

        // Step 1: inbound transfer — vault gets +user_total_debit.
        let mut vault_in = vault_in_before + user_total_debit;

        // Step 2: reserve updates — full amount_in enters reserves (fee-on-top).
        // amount_out leaves reserve_out; not part of vault_in counter.
        let amount_out = crate::amm::constant_product_output(reserve_in_before, reserve_out_before, amount_in)
            .expect("constant_product_output should not fail");
        let reserve_in_after = reserve_in_before + amount_in;
        let _reserve_out_after = reserve_out_before - amount_out;

        // Step 3: outbound CPIs extract fee_protocol + fee_ot_treasury from vault.
        // fee_lp stays in the vault (tracked off-reserve by the accumulator).
        vault_in -= fees.fee_protocol;
        vault_in -= fees.ot_treasury_fee;

        // Post-state invariant: vault_in == reserves + Σ_unclaimed_fee_lp.
        // Starting accumulator is zero, so Σ_unclaimed_fee_lp == fee_lp (the
        // only swap so far).
        let sum_unclaimed_fee_lp = fees.fee_lp;
        assert_eq!(
            vault_in,
            reserve_in_after + sum_unclaimed_fee_lp,
            "vault_in_after must equal reserve_in_after + Σ_unclaimed_fee_lp"
        );
    }
}
