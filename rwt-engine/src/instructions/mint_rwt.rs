use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::RwtError;
use crate::events::RwtMinted;
use crate::nav::calculate_nav;
use crate::validation::read_token_account_mint;
use crate::state::RwtVault;

#[derive(Accounts)]
pub struct MintRwt<'info> {
    #[account(signer)]
    pub user: &'info AccountView,

    // NOTE: account_type requires has_one (Arlex constraint). Discriminator checked by load_mut.
    #[account(mut, seeds = [b"rwt_vault"], bump)]
    pub rwt_vault: &'info AccountView,

    // RWT mint, authority = vault PDA
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    // User's USDC ATA (source of deposit)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_deposit: &'info AccountView,

    // User's RWT ATA (receives minted RWT)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_rwt: &'info AccountView,

    // Capital Accumulator USDC ATA (vault's USDC)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub capital_acc: &'info AccountView,

    // Areal fee destination ATA
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub dao_fee_account: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(ctx: Context<MintRwt>, amount: u64, min_rwt_out: u64) -> Result<()> {
    let vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    // --- Checks ---
    if vault.mint_paused {
        return Err(ProgramError::from(RwtError::MintPaused));
    }
    if amount == 0 {
        return Err(ProgramError::from(RwtError::ZeroAmount));
    }
    if amount < MIN_MINT_AMOUNT {
        return Err(ProgramError::from(RwtError::BelowMinMint));
    }

    // Validate accounts match vault state
    if ctx.accounts.rwt_mint.address().as_ref() != vault.rwt_mint.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }
    if ctx.accounts.capital_acc.address().as_ref() != vault.capital_accumulator_ata.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }
    if ctx.accounts.dao_fee_account.address().as_ref() != vault.areal_fee_destination.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // SECURITY: Validate token account mints (defense-in-depth, SPL Transfer also checks)
    // user_deposit must hold the same mint as capital_acc (USDC)
    let deposit_mint = read_token_account_mint(ctx.accounts.user_deposit)?;
    let capital_mint = read_token_account_mint(ctx.accounts.capital_acc)?;
    if deposit_mint != capital_mint {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }
    // SECURITY (M-6): dao_fee_account must also hold USDC (same mint as capital)
    let dao_fee_mint = read_token_account_mint(ctx.accounts.dao_fee_account)?;
    if dao_fee_mint != capital_mint {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }
    // user_rwt must hold vault's RWT mint
    let user_rwt_mint = read_token_account_mint(ctx.accounts.user_rwt)?;
    if user_rwt_mint != vault.rwt_mint {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // --- Fee math (use constants, not hardcoded /100) ---
    // Recalculate NAV from state to prevent stale-cache bugs (critical invariant)
    let nav = calculate_nav(vault.total_invested_capital, vault.total_rwt_supply)?;
    let fee_total = arlex_lang::math::mul_div_u64(amount, MINT_FEE_BPS, BPS_DENOMINATOR)
        .ok_or(ProgramError::from(RwtError::MathOverflow))?;
    // N-11: checked_div(2) can't fail for u64 / 2; use arithmetic shift for clarity and CU.
    let dao_fee = fee_total >> 1;
    let vault_fee = fee_total.checked_sub(dao_fee).ok_or(ProgramError::from(RwtError::MathOverflow))?;
    let net_deposit = amount.checked_sub(fee_total).ok_or(ProgramError::from(RwtError::MathOverflow))?;

    // RWT output: net_deposit * NAV_SCALE / nav
    let rwt_out = arlex_lang::math::mul_div_u64(net_deposit, NAV_SCALE, nav)
        .ok_or(ProgramError::from(RwtError::MathOverflow))?;

    // SECURITY: reject mint that would produce 0 RWT (user pays fee, gets nothing)
    if rwt_out == 0 {
        return Err(ProgramError::from(RwtError::ZeroRwtOutput));
    }

    // SECURITY: slippage protection — user MUST specify minimum acceptable output
    if min_rwt_out == 0 {
        return Err(ProgramError::from(RwtError::ZeroSlippage));
    }
    if rwt_out < min_rwt_out {
        return Err(ProgramError::from(RwtError::SlippageExceeded));
    }

    // --- Effects: update vault state BEFORE CPIs ---
    let capital_increase = (net_deposit as u128)
        .checked_add(vault_fee as u128)
        .ok_or(ProgramError::from(RwtError::MathOverflow))?;
    vault.total_invested_capital = vault.total_invested_capital
        .checked_add(capital_increase)
        .ok_or(ProgramError::from(RwtError::MathOverflow))?;
    vault.total_rwt_supply = vault.total_rwt_supply
        .checked_add(rwt_out)
        .ok_or(ProgramError::from(RwtError::MathOverflow))?;
    vault.nav_book_value = calculate_nav(vault.total_invested_capital, vault.total_rwt_supply)?;

    let nav_after = vault.nav_book_value;

    // --- Interactions: CPIs ---

    // 1. User transfers (net_deposit + vault_fee) → capital_acc
    let capital_transfer_amount = net_deposit.checked_add(vault_fee)
        .ok_or(ProgramError::from(RwtError::MathOverflow))?;
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.user_deposit,
        to: ctx.accounts.capital_acc,
        authority: ctx.accounts.user,
        amount: capital_transfer_amount,
    }.invoke()?;

    // 2. User transfers dao_fee → dao_fee_account
    if dao_fee > 0 {
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.user_deposit,
            to: ctx.accounts.dao_fee_account,
            authority: ctx.accounts.user,
            amount: dao_fee,
        }.invoke()?;
    }

    // 3. Vault PDA mints RWT to user
    let bump = [vault.bump];
    let seeds = [
        Seed::from(b"rwt_vault" as &[u8]),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    arlex_lang::token::instructions::MintTo {
        mint: ctx.accounts.rwt_mint,
        account: ctx.accounts.user_rwt,
        mint_authority: ctx.accounts.rwt_vault,
        amount: rwt_out,
    }.invoke_signed(&[signer])?;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(RwtMinted {
        user: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.user.address().as_ref());
            arr
        },
        deposit_amount: amount,
        rwt_amount: rwt_out,
        fee_vault: vault_fee,
        fee_dao: dao_fee,
        nav_after,
        is_admin: false,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
