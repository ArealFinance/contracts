use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::LiquidityRemoved;
use crate::state::*;
use crate::amm::calculate_remove_amounts;
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
