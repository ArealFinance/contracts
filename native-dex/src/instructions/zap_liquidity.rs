use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::ZapLiquidityExecuted;
use crate::state::*;
use crate::amm::{constant_product_output, calculate_fees, calculate_lp_shares};
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
    // SECURITY: Reject concentrated pools — zap uses constant_product internally,
    // which is incorrect for bin-based pools. Use add_liquidity for concentrated.
    if pool.pool_type == crate::constants::POOL_TYPE_CONCENTRATED {
        return Err(ProgramError::from(DexError::InvalidPoolType));
    }
    if amount_a == 0 && amount_b == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount));
    }

    // SECURITY: Validate vaults match pool state
    validate_vault(accounts.vault_a, &pool.vault_a)?;
    validate_vault(accounts.vault_b, &pool.vault_b)?;

    // Validate areal_fee_account
    if accounts.areal_fee_account.address().as_ref() != config.areal_fee_destination.as_ref() {
        return Err(ProgramError::from(DexError::InvalidTokenAccount));
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

    let (final_a, final_b, swapped_amount, fee_protocol_total, fee_ot_total);

    if pool.reserve_a == 0 && pool.reserve_b == 0 {
        // Empty pool: skip swap, treat as regular add_liquidity
        if amount_a == 0 || amount_b == 0 {
            return Err(ProgramError::from(DexError::ZeroAmount));
        }
        final_a = amount_a;
        final_b = amount_b;
        swapped_amount = 0;
        fee_protocol_total = 0;
        fee_ot_total = 0;
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
                fee_protocol_total = 0;
                fee_ot_total = 0;
            } else {
                // Internal swap: B → A (b_to_a)
                let b_is_rwt = is_rwt_mint(&pool.token_b_mint);
                let (swap_out, fp, fot) = internal_swap(
                    swap_b, pool.reserve_b, pool.reserve_a,
                    b_is_rwt, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury,
                )?;

                // Update reserves after internal swap
                if b_is_rwt {
                    let fees = calculate_fees(swap_b, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
                    let total_deducted = fees.fee_total.checked_add(fees.ot_treasury_fee)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    let net = swap_b.checked_sub(total_deducted)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    pool.reserve_b = pool.reserve_b.checked_add(net).ok_or(ProgramError::from(DexError::MathOverflow))?
                        .checked_add(fees.fee_lp).ok_or(ProgramError::from(DexError::MathOverflow))?;
                    pool.reserve_a = pool.reserve_a.checked_sub(swap_out)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                } else {
                    let gross_out_for_fees = constant_product_output(pool.reserve_b, pool.reserve_a, swap_b)?;
                    let fees = calculate_fees(gross_out_for_fees, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
                    pool.reserve_b = pool.reserve_b.checked_add(swap_b)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    pool.reserve_a = pool.reserve_a.checked_sub(swap_out)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?
                        .checked_sub(fp).ok_or(ProgramError::from(DexError::MathOverflow))?
                        .checked_sub(fot).ok_or(ProgramError::from(DexError::MathOverflow))?;
                }

                final_a = amount_a.checked_add(swap_out)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?;
                final_b = amount_b.checked_sub(swap_b)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?;
                swapped_amount = swap_b;
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
                fee_protocol_total = 0;
                fee_ot_total = 0;
            } else {
                let a_is_rwt = is_rwt_mint(&pool.token_a_mint);
                let (swap_out, fp, fot) = internal_swap(
                    swap_a, pool.reserve_a, pool.reserve_b,
                    a_is_rwt, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury,
                )?;

                // Update reserves after internal swap
                if a_is_rwt {
                    let fees = calculate_fees(swap_a, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
                    let total_deducted = fees.fee_total.checked_add(fees.ot_treasury_fee)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    let net = swap_a.checked_sub(total_deducted)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    pool.reserve_a = pool.reserve_a.checked_add(net).ok_or(ProgramError::from(DexError::MathOverflow))?
                        .checked_add(fees.fee_lp).ok_or(ProgramError::from(DexError::MathOverflow))?;
                    pool.reserve_b = pool.reserve_b.checked_sub(swap_out)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                } else {
                    let gross_out_for_fees = constant_product_output(pool.reserve_a, pool.reserve_b, swap_a)?;
                    let fees = calculate_fees(gross_out_for_fees, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
                    pool.reserve_a = pool.reserve_a.checked_add(swap_a)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?;
                    pool.reserve_b = pool.reserve_b.checked_sub(swap_out)
                        .ok_or(ProgramError::from(DexError::MathOverflow))?
                        .checked_sub(fp).ok_or(ProgramError::from(DexError::MathOverflow))?
                        .checked_sub(fot).ok_or(ProgramError::from(DexError::MathOverflow))?;
                }

                final_a = amount_a.checked_sub(swap_a)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?;
                final_b = amount_b.checked_add(swap_out)
                    .ok_or(ProgramError::from(DexError::MathOverflow))?;
                swapped_amount = swap_a;
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

    // Accumulate swap fees
    pool.total_fees_accumulated = pool.total_fees_accumulated
        .checked_add(fee_protocol_total).ok_or(ProgramError::from(DexError::MathOverflow))?
        .checked_add(fee_ot_total).ok_or(ProgramError::from(DexError::MathOverflow))?;

    // --- Initialize or update LpPosition ---
    let provider_key = pubkey_bytes(accounts.authority);
    let pool_key = pubkey_bytes(accounts.pool_state);
    let clock = Clock::get()?;

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
    } else {
        let lp = LpPosition::load_mut(accounts.lp_position, program_id)?;
        // SECURITY: Verify LpPosition belongs to this provider
        if lp.owner != provider_key {
            return Err(ProgramError::from(DexError::Unauthorized));
        }
        if lp.pool != pool_key {
            return Err(ProgramError::from(DexError::InvalidVault));
        }
        lp.shares = lp.shares.checked_add(shares)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        lp.last_update_ts = clock.unix_timestamp;
    }

    // --- Interactions: transfer tokens from provider to vaults ---
    // Transfer the total user input (amount_a + amount_b), the internal swap is virtual.
    // User-signed path: empty signer slice (authority is a transaction signer).
    // PDA-signed path: caller-supplied seeds authorize the PDA-owned ATA.
    if amount_a > 0 {
        let transfer_a = arlex_lang::token::instructions::Transfer {
            from: accounts.provider_token_a,
            to: accounts.vault_a,
            authority: accounts.authority,
            amount: amount_a,
        };
        match authority_signer_seeds {
            Some(seeds) => transfer_a.invoke_signed(seeds)?,
            None => transfer_a.invoke()?,
        }
    }

    if amount_b > 0 {
        let transfer_b = arlex_lang::token::instructions::Transfer {
            from: accounts.provider_token_b,
            to: accounts.vault_b,
            authority: accounts.authority,
            amount: amount_b,
        };
        match authority_signer_seeds {
            Some(seeds) => transfer_b.invoke_signed(seeds)?,
            None => transfer_b.invoke()?,
        }
    }

    // Transfer protocol fees from vault (RWT side)
    let pool_bump_arr = [pool.bump];

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
fn internal_swap(
    amount_in: u64,
    reserve_in: u64,
    reserve_out: u64,
    input_is_rwt: bool,
    fee_bps: u16,
    lp_fee_share_bps: u16,
    has_ot_treasury: bool,
) -> core::result::Result<(u64, u64, u64), ProgramError> {
    if input_is_rwt {
        let fees = calculate_fees(amount_in, fee_bps, lp_fee_share_bps, has_ot_treasury)?;
        let total_deducted = fees.fee_total.checked_add(fees.ot_treasury_fee)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        let net_input = amount_in.checked_sub(total_deducted)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        let amount_out = constant_product_output(reserve_in, reserve_out, net_input)?;
        Ok((amount_out, fees.fee_protocol, fees.ot_treasury_fee))
    } else {
        let gross_out = constant_product_output(reserve_in, reserve_out, amount_in)?;
        let fees = calculate_fees(gross_out, fee_bps, lp_fee_share_bps, has_ot_treasury)?;
        let total_deducted = fees.fee_total.checked_add(fees.ot_treasury_fee)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        let amount_out = gross_out.checked_sub(total_deducted)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        Ok((amount_out, fees.fee_protocol, fees.ot_treasury_fee))
    }
}
