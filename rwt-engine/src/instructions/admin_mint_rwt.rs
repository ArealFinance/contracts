use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::SPL_TOKEN_PROGRAM;
use crate::error::RwtError;
use crate::events::RwtMinted;
use crate::nav::calculate_nav;
use crate::state::RwtVault;

#[derive(Accounts)]
pub struct AdminMintRwt<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "RwtVault",
        seeds = [b"rwt_vault"], bump
    )]
    pub rwt_vault: &'info AccountView,

    // RWT mint, authority = vault PDA
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    // Recipient RWT ATA
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub recipient_rwt: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<AdminMintRwt>,
    rwt_amount: u64,
    backing_capital_usd: u64,
) -> Result<()> {
    let vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    // --- Checks ---
    // NOTE: admin_mint_rwt is NOT blocked by mint_paused (design decision per spec)
    if rwt_amount == 0 {
        return Err(ProgramError::from(RwtError::ZeroAmount));
    }
    if backing_capital_usd == 0 {
        return Err(ProgramError::from(RwtError::ZeroBackingCapital));
    }

    // Validate mint matches vault
    if ctx.accounts.rwt_mint.address().as_ref() != vault.rwt_mint.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // SECURITY: Validate recipient_rwt holds vault's RWT mint (defense-in-depth)
    let recipient_mint = crate::validation::read_token_account_mint(ctx.accounts.recipient_rwt)?;
    if recipient_mint != vault.rwt_mint {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // --- Effects: update vault state ---
    vault.total_invested_capital = vault.total_invested_capital
        .checked_add(backing_capital_usd as u128)
        .ok_or(ProgramError::from(RwtError::MathOverflow))?;
    vault.total_rwt_supply = vault.total_rwt_supply
        .checked_add(rwt_amount)
        .ok_or(ProgramError::from(RwtError::MathOverflow))?;
    vault.nav_book_value = calculate_nav(vault.total_invested_capital, vault.total_rwt_supply)?;

    let nav_after = vault.nav_book_value;

    // --- Interactions: Vault PDA mints RWT to recipient ---
    let bump = [vault.bump];
    let seeds = [
        Seed::from(b"rwt_vault" as &[u8]),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    arlex_lang::token::instructions::MintTo {
        mint: ctx.accounts.rwt_mint,
        account: ctx.accounts.recipient_rwt,
        mint_authority: ctx.accounts.rwt_vault,
        amount: rwt_amount,
    }.invoke_signed(&[signer])?;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(RwtMinted {
        user: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.authority.address().as_ref());
            arr
        },
        deposit_amount: backing_capital_usd,
        rwt_amount,
        fee_vault: 0,
        fee_dao: 0,
        nav_after,
        is_admin: true,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
