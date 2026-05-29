//! `complete_unstake(nonce)` — after cooldown, claim the reserved RWT and
//! close the ticket (rent → owner).
//!
//! staking.mdx §"complete_unstake":
//!   - Require `now ≥ ticket.unlock_ts` (CooldownNotElapsed).
//!   - Require `ticket.owner == signer` (TicketOwnerMismatch).
//!   - Effects: `total_rwt_reserved -= ticket.amount_rwt`; close ticket.
//!   - CPI: Transfer pool_vault → user (signed by config PDA).

use arlex_lang::prelude::*;
use pinocchio::cpi::{Seed, Signer};
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::StakingError;
use crate::events::UnstakeCompleted;
use crate::state::{StakingConfig, UnstakeTicket};
use crate::token::read_token_account_mint;

#[derive(Accounts)]
pub struct CompleteUnstake<'info> {
    #[account(mut, signer)]
    pub user: &'info AccountView,

    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,

    /// UnstakeTicket PDA — closed here; rent returns to `user`.
    #[account(mut)]
    pub ticket: &'info AccountView,

    /// RWT pool vault (config-PDA-owned). Source of the payout.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub pool_vault: &'info AccountView,

    /// User's RWT destination ATA.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_rwt_ata: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(ctx: Context<CompleteUnstake>, nonce: u64) -> Result<()> {
    let config = StakingConfig::load_mut(ctx.accounts.staking_config, ctx.program_id)?;

    // Validate vault + user ATA against config.
    if ctx.accounts.pool_vault.address().as_ref() != config.pool_vault.as_ref() {
        return Err(ProgramError::from(StakingError::InvalidTokenAccount));
    }
    if read_token_account_mint(ctx.accounts.user_rwt_ata)? != config.rwt_mint {
        return Err(ProgramError::from(StakingError::InvalidTokenAccount));
    }

    // --- Load + validate the ticket ---
    let (amount_rwt, reserved_after) = {
        let ticket = UnstakeTicket::load(ctx.accounts.ticket, ctx.program_id)?;

        // Owner must equal the signer.
        if ticket.owner.as_ref() != ctx.accounts.user.address().as_ref() {
            return Err(ProgramError::from(StakingError::TicketOwnerMismatch));
        }
        // Nonce must match the requested ticket (PDA-derivation defense).
        if ticket.nonce != nonce {
            return Err(ProgramError::from(StakingError::TicketOwnerMismatch));
        }

        // Cooldown must have elapsed.
        let clock = Clock::get()?;
        if clock.unix_timestamp < ticket.unlock_ts {
            return Err(ProgramError::from(StakingError::CooldownNotElapsed));
        }

        let amount_rwt = ticket.amount_rwt;
        let reserved_after = config
            .total_rwt_reserved
            .checked_sub(amount_rwt)
            .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;
        (amount_rwt, reserved_after)
    };

    // --- Effects: decrement reserved BEFORE CPI; capture bump for signing ---
    config.total_rwt_reserved = reserved_after;
    let config_bump = config.bump;

    // --- Interaction: pool_vault → user, signed by config PDA ---
    let bump_arr = [config_bump];
    let seeds = [
        Seed::from(STAKING_CONFIG_SEED),
        Seed::from(bump_arr.as_ref()),
    ];
    let signer = Signer::from(&seeds);
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.pool_vault,
        to: ctx.accounts.user_rwt_ata,
        authority: ctx.accounts.staking_config,
        amount: amount_rwt,
    }
    .invoke_signed(&[signer])?;

    // --- Close the ticket: rent lamports back to user ---
    let ticket_lamports = ctx.accounts.ticket.lamports();
    let user_lamports = ctx.accounts.user.lamports();
    ctx.accounts.user.set_lamports(
        user_lamports
            .checked_add(ticket_lamports)
            .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?,
    );
    ctx.accounts.ticket.set_lamports(0);
    ctx.accounts.ticket.close()?;

    // --- Emit ---
    let clock = Clock::get()?;
    let mut user_arr = [0u8; 32];
    user_arr.copy_from_slice(ctx.accounts.user.address().as_ref());
    emit!(UnstakeCompleted {
        user: user_arr,
        rwt_out: amount_rwt,
        nonce,
        reserved_after,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
