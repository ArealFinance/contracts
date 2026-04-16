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

pub fn handler(
    ctx: Context<Swap>,
    amount_in: u64,
    min_amount_out: u64,
    a_to_b: bool,
) -> Result<()> {
    let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;
    let pool = PoolState::load_mut(ctx.accounts.pool_state, ctx.program_id)?;

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
    validate_vault(ctx.accounts.vault_in, expected_vault_in)?;
    validate_vault(ctx.accounts.vault_out, expected_vault_out)?;

    // Validate areal_fee_account matches DexConfig
    if ctx.accounts.areal_fee_account.address().as_ref() != config.areal_fee_destination.as_ref() {
        return Err(ProgramError::from(DexError::InvalidTokenAccount));
    }

    // --- OT treasury fee account validation ---
    if pool.has_ot_treasury {
        if ctx.remaining_accounts.is_empty() {
            return Err(ProgramError::from(DexError::MissingOtTreasuryAccount));
        }
        let ot_fee_account = &ctx.remaining_accounts[0];
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

    if input_is_rwt {
        // Selling RWT: fee deducted from input BEFORE swap
        let fees = calculate_fees(amount_in, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
        let total_deducted = fees.fee_total.checked_add(fees.ot_treasury_fee)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
        net_input = amount_in.checked_sub(total_deducted)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;

        // Branch on pool type for output calculation
        if pool.pool_type == POOL_TYPE_CONCENTRATED {
            // Load BinArray from remaining_accounts with PDA verification
            let bin_idx = if pool.has_ot_treasury { 1 } else { 0 };
            if ctx.remaining_accounts.len() <= bin_idx {
                return Err(ProgramError::from(DexError::InvalidBinRange));
            }
            let pool_key = pubkey_bytes(ctx.accounts.pool_state);
            let (expected_bin_pda, _) = arlex_lang::find_program_address(
                &[b"bins", pool_key.as_ref()],
                ctx.program_id,
            );
            if ctx.remaining_accounts[bin_idx].address().as_ref() != expected_bin_pda.as_ref() {
                return Err(ProgramError::InvalidSeeds);
            }
            let bin_array = BinArray::load_mut(&ctx.remaining_accounts[bin_idx], ctx.program_id)?;
            let (walk_out, walk_remaining) = concentrated::bin_walk_swap(bin_array, pool.bin_step_bps, net_input, a_to_b)?;
            amount_out = walk_out;
            pool.active_bin_id = bin_array.active_bin_id;

            // SECURITY: Sync unconsumed input + fee_lp into bins so sum(bins) == reserves.
            concentrated::sync_remaining_to_bin(bin_array, walk_remaining, a_to_b)?;
            concentrated::sync_fee_lp_to_bin(bin_array, fees.fee_lp, token_a_is_rwt_side)?;
        } else {
            amount_out = constant_product_output(reserve_in, reserve_out, net_input)?;
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
            if ctx.remaining_accounts.len() <= bin_idx {
                return Err(ProgramError::from(DexError::InvalidBinRange));
            }
            let pool_key = pubkey_bytes(ctx.accounts.pool_state);
            let (expected_bin_pda, _) = arlex_lang::find_program_address(
                &[b"bins", pool_key.as_ref()],
                ctx.program_id,
            );
            if ctx.remaining_accounts[bin_idx].address().as_ref() != expected_bin_pda.as_ref() {
                return Err(ProgramError::InvalidSeeds);
            }
            let bin_array = BinArray::load_mut(&ctx.remaining_accounts[bin_idx], ctx.program_id)?;
            let (walk_out, walk_remaining) = concentrated::bin_walk_swap(bin_array, pool.bin_step_bps, net_input, a_to_b)?;
            gross_out = walk_out;
            pool.active_bin_id = bin_array.active_bin_id;

            // Sync unconsumed input into bins
            concentrated::sync_remaining_to_bin(bin_array, walk_remaining, a_to_b)?;

            // Fees calculated once, used for both bin sync and reserve update
            let fees = calculate_fees(gross_out, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
            // fee_lp stays in RWT vault — sync to active bin so bins match reserves
            concentrated::sync_fee_lp_to_bin(bin_array, fees.fee_lp, token_a_is_rwt_side)?;

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
    // LP fee auto-compounds into reserves (stays in vault on the RWT side)
    if a_to_b {
        if input_is_rwt {
            // Input side (A=RWT): user sends amount_in, but only net_input goes to reserves
            // fee_lp stays in vault = auto-compound. fee_protocol + ot_treasury extracted.
            pool.reserve_a = pool.reserve_a
                .checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_add(fee_lp)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_b = pool.reserve_b.checked_sub(amount_out)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
        } else {
            // Output side (B=RWT): fee_lp stays on output side
            pool.reserve_a = pool.reserve_a.checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_b = pool.reserve_b.checked_sub(amount_out)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_protocol)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_ot_treasury)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            // fee_lp stays in reserve_b (auto-compound)
        }
    } else {
        if input_is_rwt {
            pool.reserve_b = pool.reserve_b
                .checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_add(fee_lp)
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
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
        }
    }

    pool.total_fees_accumulated = pool.total_fees_accumulated
        .checked_add(fee_lp).ok_or(ProgramError::from(DexError::MathOverflow))?
        .checked_add(fee_protocol).ok_or(ProgramError::from(DexError::MathOverflow))?
        .checked_add(fee_ot_treasury).ok_or(ProgramError::from(DexError::MathOverflow))?;

    // --- Interactions: CPIs ---
    let pool_bump = [pool.bump];

    // 1. User sends input tokens to vault_in
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.user_token_in,
        to: ctx.accounts.vault_in,
        authority: ctx.accounts.user,
        amount: amount_in,
    }.invoke()?;

    // 2. Pool sends output tokens to user
    {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.vault_out,
            to: ctx.accounts.user_token_out,
            authority: ctx.accounts.pool_state,
            amount: amount_out,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // 3. Pool sends protocol fee to areal_fee_account (from RWT vault)
    if fee_protocol > 0 {
        let rwt_vault = if input_is_rwt { ctx.accounts.vault_in } else { ctx.accounts.vault_out };
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: rwt_vault,
            to: ctx.accounts.areal_fee_account,
            authority: ctx.accounts.pool_state,
            amount: fee_protocol,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // 4. Pool sends OT treasury fee (if applicable)
    if fee_ot_treasury > 0 {
        let ot_fee_account = &ctx.remaining_accounts[0];
        let rwt_vault = if input_is_rwt { ctx.accounts.vault_in } else { ctx.accounts.vault_out };
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: rwt_vault,
            to: ot_fee_account,
            authority: ctx.accounts.pool_state,
            amount: fee_ot_treasury,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(SwapExecuted {
        pool: pubkey_bytes(ctx.accounts.pool_state),
        user: pubkey_bytes(ctx.accounts.user),
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
