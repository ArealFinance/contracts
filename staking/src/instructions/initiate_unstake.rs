//! `initiate_unstake(strwt_amount, nonce)` — burn stRWT, fix the RWT amount
//! at the current rate, move it active → reserved, start the 21-day cooldown.
//!
//! staking.mdx §"initiate_unstake":
//!   rwt_out = strwt_amount × (active + V_ASSETS) / (strwt_supply + V_SHARES)
//!   (u128 intermediate, floor)
//!   Effects: burn stRWT; active -= rwt_out; reserved += rwt_out; create ticket.
//!   Ticket seed ["unstake", user, nonce.to_le_bytes()] with
//!     { amount_rwt: rwt_out, unlock_ts: now + cooldown_seconds, nonce }.
//!   `nonce` is client-supplied; collision on a live nonce fails `init`.

use arlex_lang::prelude::*;
use pinocchio::cpi::{Seed, Signer};
use pinocchio::sysvars::{clock::Clock, rent::Rent, Sysvar};

use crate::constants::*;
use crate::error::StakingError;
use crate::events::UnstakeInitiated;
use crate::rate::rwt_out_for_unstake;
use crate::state::{StakingConfig, UnstakeTicket};
use crate::token::{read_mint_supply, read_token_account_mint};

#[derive(Accounts)]
pub struct InitiateUnstake<'info> {
    #[account(mut, signer)]
    pub user: &'info AccountView,

    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,

    /// stRWT mint (mutated by Burn).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub strwt_mint: &'info AccountView,

    /// User's stRWT ATA (source of the burn).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_strwt_ata: &'info AccountView,

    /// UnstakeTicket PDA — created here. Seed ["unstake", user, nonce_le].
    #[account(mut)]
    pub ticket: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(ctx: Context<InitiateUnstake>, strwt_amount: u64, nonce: u64) -> Result<()> {
    // `mut` binding: counter writes go through the guard's DerefMut. The
    // staking_config PDA is NOT passed into either CPI below (the Burn is signed
    // by `user`; the CreateAccount is signed by the ticket PDA), so the guard may
    // stay live across both CPIs — config.cooldown_seconds is read between them.
    StakingConfig::assert_account_size(ctx.accounts.staking_config)?;
    let mut config = StakingConfig::load_mut(ctx.accounts.staking_config, ctx.program_id)?;

    // --- Checks ---
    if strwt_amount == 0 {
        return Err(ProgramError::from(StakingError::ZeroRwtOutput));
    }
    if ctx.accounts.strwt_mint.address().as_ref() != config.strwt_mint.as_ref() {
        return Err(ProgramError::from(StakingError::InvalidTokenAccount));
    }
    if read_token_account_mint(ctx.accounts.user_strwt_ata)? != config.strwt_mint {
        return Err(ProgramError::from(StakingError::InvalidTokenAccount));
    }

    // --- Rate math (read live supply BEFORE the burn) ---
    let strwt_supply = read_mint_supply(ctx.accounts.strwt_mint)?;
    let active = config.total_rwt_active;
    let rwt_out = rwt_out_for_unstake(strwt_amount, strwt_supply, active)
        .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;
    if rwt_out == 0 {
        return Err(ProgramError::from(StakingError::ZeroRwtOutput));
    }

    // --- Effects: move active → reserved BEFORE CPI / ticket creation ---
    let active_after = active
        .checked_sub(rwt_out)
        .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;
    let reserved_after = config
        .total_rwt_reserved
        .checked_add(rwt_out)
        .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;
    config.total_rwt_active = active_after;
    config.total_rwt_reserved = reserved_after;

    // --- Interaction 1: burn stRWT (user is owner/authority of the ATA) ---
    arlex_lang::token::instructions::Burn {
        account: ctx.accounts.user_strwt_ata,
        mint: ctx.accounts.strwt_mint,
        authority: ctx.accounts.user,
        amount: strwt_amount,
    }
    .invoke()?;

    // --- Create the UnstakeTicket PDA (init semantics: live-nonce reuse fails) ---
    let clock = Clock::get()?;
    let unlock_ts = clock
        .unix_timestamp
        .checked_add(config.cooldown_seconds)
        .ok_or_else(|| ProgramError::from(StakingError::MathOverflow))?;

    let nonce_le = nonce.to_le_bytes();
    let user_seed = ctx.accounts.user.address();
    let (_, ticket_bump) = arlex_lang::find_program_address(
        &[UNSTAKE_SEED, user_seed.as_ref(), &nonce_le],
        ctx.program_id,
    );

    let rent = Rent::get()?;
    let lamports = rent.try_minimum_balance(UnstakeTicket::SPACE)?;
    let ticket_bump_arr = [ticket_bump];

    arlex_lang::system::instructions::CreateAccount {
        from: ctx.accounts.user,
        to: ctx.accounts.ticket,
        lamports,
        space: UnstakeTicket::SPACE as u64,
        owner: ctx.program_id,
    }
    .invoke_signed(&[Signer::from(&[
        Seed::from(UNSTAKE_SEED),
        Seed::from(user_seed.as_ref()),
        Seed::from(nonce_le.as_ref()),
        Seed::from(ticket_bump_arr.as_ref()),
    ])])?;

    let mut owner_arr = [0u8; 32];
    owner_arr.copy_from_slice(user_seed.as_ref());

    // `mut` binding: field writes go through the guard's DerefMut. No CPI
    // follows this init (the ticket account was created by the CPI above).
    let mut ticket = UnstakeTicket::init(ctx.accounts.ticket, ctx.program_id)?;
    ticket.owner = owner_arr;
    ticket.amount_rwt = rwt_out;
    ticket.unlock_ts = unlock_ts;
    ticket.nonce = nonce;
    ticket.bump = ticket_bump;

    // --- Emit ---
    emit!(UnstakeInitiated {
        user: owner_arr,
        strwt_burned: strwt_amount,
        rwt_out,
        unlock_ts,
        nonce,
        active_after,
        reserved_after,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
