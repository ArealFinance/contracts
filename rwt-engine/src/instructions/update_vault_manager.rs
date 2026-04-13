use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::events::VaultManagerUpdated;
use crate::state::RwtVault;

#[derive(Accounts)]
pub struct UpdateVaultManager<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "RwtVault",
        seeds = [b"rwt_vault"], bump
    )]
    pub rwt_vault: &'info AccountView,
}

pub fn handler(ctx: Context<UpdateVaultManager>, new_manager: [u8; 32]) -> Result<()> {
    let vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    // NOTE: Zero address is allowed — it disables manager role (no vault_swap possible).
    let old_manager = vault.manager;
    vault.manager = new_manager;

    let clock = Clock::get()?;
    emit!(VaultManagerUpdated {
        old_manager,
        new_manager,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
