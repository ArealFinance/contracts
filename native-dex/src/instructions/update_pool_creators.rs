use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::PoolCreatorsUpdated;
use crate::state::{DexConfig, PoolCreators};

/// Action: 0 = Add, 1 = Remove
const ACTION_ADD: u8 = 0;
const ACTION_REMOVE: u8 = 1;

#[derive(Accounts)]
pub struct UpdatePoolCreators<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        has_one = authority, account_type = "DexConfig",
        seeds = [b"dex_config"], bump
    )]
    pub dex_config: &'info AccountView,

    #[account(
        mut, seeds = [b"pool_creators"], bump
    )]
    pub pool_creators: &'info AccountView,
}

pub fn handler(
    ctx: Context<UpdatePoolCreators>,
    wallet: [u8; 32],
    action: u8,
) -> Result<()> {
    let creators = PoolCreators::load_mut(ctx.accounts.pool_creators, ctx.program_id)?;

    if wallet == [0u8; 32] {
        return Err(ProgramError::from(DexError::ZeroAddress));
    }

    match action {
        ACTION_ADD => {
            // Check not already present
            for i in 0..creators.active_count as usize {
                if creators.creators[i] == wallet {
                    return Err(ProgramError::from(DexError::CreatorAlreadyExists));
                }
            }
            // Check not full
            if creators.active_count as usize >= MAX_POOL_CREATORS {
                return Err(ProgramError::from(DexError::WhitelistFull));
            }
            // Add at next slot
            creators.creators[creators.active_count as usize] = wallet;
            creators.active_count += 1;
        }
        ACTION_REMOVE => {
            // Find and remove
            let mut found_idx = None;
            for i in 0..creators.active_count as usize {
                if creators.creators[i] == wallet {
                    found_idx = Some(i);
                    break;
                }
            }
            let idx = found_idx.ok_or(ProgramError::from(DexError::CreatorNotFound))?;

            // Swap with last and decrement count
            let last_idx = (creators.active_count - 1) as usize;
            if idx != last_idx {
                creators.creators[idx] = creators.creators[last_idx];
            }
            creators.creators[last_idx] = [0u8; 32];
            creators.active_count -= 1;
        }
        _ => {
            return Err(ProgramError::InvalidArgument);
        }
    }

    let clock = Clock::get()?;
    emit!(PoolCreatorsUpdated {
        wallet,
        action,
        active_count: creators.active_count,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
