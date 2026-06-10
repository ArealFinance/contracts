use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::error::RwtError;
use crate::events::MintPauseToggled;
use crate::state::RwtVault;

#[derive(Accounts)]
pub struct PauseMint<'info> {
    #[account(signer)]
    pub pause_authority: &'info AccountView,

    // NOTE: account_type requires has_one (Arlex constraint). Discriminator checked by load_mut.
    #[account(mut, seeds = [b"rwt_vault"], bump)]
    pub rwt_vault: &'info AccountView,
}

#[derive(Accounts)]
pub struct UnpauseMint<'info> {
    #[account(signer)]
    pub pause_authority: &'info AccountView,

    // NOTE: account_type requires has_one (Arlex constraint). Discriminator checked by load_mut.
    #[account(mut, seeds = [b"rwt_vault"], bump)]
    pub rwt_vault: &'info AccountView,
}

pub fn pause_handler(ctx: Context<PauseMint>) -> Result<()> {
    // `mut` binding: mint_paused write goes through the guard's DerefMut. No CPI.
    let mut vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    // Manual check: signer must be pause_authority (not authority — different role)
    if ctx.accounts.pause_authority.address().as_ref() != vault.pause_authority.as_ref() {
        return Err(ProgramError::from(RwtError::UnauthorizedPause));
    }

    // Idempotent — double pause succeeds silently
    vault.mint_paused = true;

    let clock = Clock::get()?;
    emit!(MintPauseToggled {
        paused: true,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

pub fn unpause_handler(ctx: Context<UnpauseMint>) -> Result<()> {
    // `mut` binding: mint_paused write goes through the guard's DerefMut. No CPI.
    let mut vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    // Manual check: signer must be pause_authority
    if ctx.accounts.pause_authority.address().as_ref() != vault.pause_authority.as_ref() {
        return Err(ProgramError::from(RwtError::UnauthorizedPause));
    }

    // Idempotent — double unpause succeeds silently
    vault.mint_paused = false;

    let clock = Clock::get()?;
    emit!(MintPauseToggled {
        paused: false,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
