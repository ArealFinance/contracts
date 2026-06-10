use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::error::DexError;
use crate::events::{AuthorityTransferProposed, AuthorityTransferAccepted};
use crate::state::{DexConfig, PoolCreators};
use crate::validation::pubkey_bytes;

// --- Propose ---

#[derive(Accounts)]
pub struct ProposeAuthorityTransfer<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "DexConfig",
        seeds = [b"dex_config"], bump
    )]
    pub dex_config: &'info AccountView,
}

pub fn propose_handler(
    ctx: Context<ProposeAuthorityTransfer>,
    new_authority: [u8; 32],
) -> Result<()> {
    // `mut` binding: field writes go through the guard's DerefMut. No CPI here.
    let mut config = DexConfig::load_mut(ctx.accounts.dex_config, ctx.program_id)?;

    if new_authority == [0u8; 32] {
        return Err(ProgramError::from(DexError::ZeroAddress));
    }
    if new_authority == config.authority {
        return Err(ProgramError::from(DexError::SelfTransfer));
    }

    let current_authority = config.authority;

    // Overwrites any existing pending transfer
    config.pending_authority = new_authority;
    config.has_pending = true;

    let clock = Clock::get()?;
    emit!(AuthorityTransferProposed {
        current_authority,
        pending_authority: new_authority,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

// --- Accept ---

#[derive(Accounts)]
pub struct AcceptAuthorityTransfer<'info> {
    #[account(signer)]
    pub new_authority: &'info AccountView,

    #[account(mut, seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    // DEX accept updates BOTH PDAs (unlike OT/Futarchy/RWT)
    #[account(mut, seeds = [b"pool_creators"], bump)]
    pub pool_creators: &'info AccountView,
}

pub fn accept_handler(ctx: Context<AcceptAuthorityTransfer>) -> Result<()> {
    // `mut` binding: field writes go through the guard's DerefMut. No CPI here.
    let mut config = DexConfig::load_mut(ctx.accounts.dex_config, ctx.program_id)?;

    if !config.has_pending {
        return Err(ProgramError::from(DexError::NoPendingAuthority));
    }

    let new_auth_key = pubkey_bytes(ctx.accounts.new_authority);
    if new_auth_key != config.pending_authority {
        return Err(ProgramError::from(DexError::InvalidPendingAuthority));
    }

    let old_authority = config.authority;

    // Update DexConfig authority
    config.authority = new_auth_key;
    config.pending_authority = [0u8; 32];
    config.has_pending = false;

    // Update PoolCreators authority. `mut` binding: write via DerefMut. No CPI.
    let mut creators = PoolCreators::load_mut(ctx.accounts.pool_creators, ctx.program_id)?;
    creators.authority = new_auth_key;

    let clock = Clock::get()?;
    emit!(AuthorityTransferAccepted {
        old_authority,
        new_authority: new_auth_key,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
