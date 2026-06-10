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
    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut creators = PoolCreators::load_mut(ctx.accounts.pool_creators, ctx.program_id)?;

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
            // Add at next slot. Read the index into a temp first: through the
            // guard's DerefMut, indexing with `creators.active_count` inline
            // would borrow `*creators` immutably (the index) and mutably (the
            // slot write) at once (E0502).
            let next_slot = creators.active_count as usize;
            creators.creators[next_slot] = wallet;
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
            let idx = found_idx.ok_or_else(|| ProgramError::from(DexError::CreatorNotFound))?;

            // Swap with last and decrement count. Read the last slot into a
            // temp first: through the guard's Deref/DerefMut, a direct
            // self-index copy (`creators[idx] = creators[last]`) would borrow
            // `*creators` mutably and immutably at once (E0502).
            let last_idx = (creators.active_count - 1) as usize;
            if idx != last_idx {
                let last_val = creators.creators[last_idx];
                creators.creators[idx] = last_val;
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

#[cfg(test)]
mod cu_hotfix_regression {
    extern crate alloc;

    /// CU-hotfix regression (2026-05-18). Eagerly-evaluated
    /// `Option::ok_or(ProgramError::from(E))` calls invoke the
    /// arlex-derive `From<E>` impl on the success path, which calls
    /// `arlex_lang::log(msg)` — burning ~100 CUs per call site and
    /// emitting a spurious "Arithmetic overflow" log line on every
    /// instruction. See `rwt-engine/src/instructions/mint_rwt.rs`
    /// (`mint_rwt_has_no_eager_ok_or_program_error`) for the full
    /// background and the smoke-3 trace that first exposed this.
    ///
    /// The detection key is reassembled from two halves so this
    /// test's own definition of it does not match.
    #[test]
    fn no_eager_ok_or_program_error() {
        const SRC: &str = include_str!("update_pool_creators.rs");
        const HALF_1: &str = ".ok_or(ProgramError";
        const HALF_2: &str = "::from(";
        let bad_needle = alloc::format!("{HALF_1}{HALF_2}");
        let mut hits = 0usize;
        for raw_line in SRC.lines() {
            let line = match raw_line.find("//") {
                Some(idx) => &raw_line[..idx],
                None => raw_line,
            };
            if let Some(needle_pos) = line.find(&bad_needle) {
                if line[..needle_pos].contains('"') {
                    continue;
                }
                hits += 1;
            }
        }
        assert_eq!(
            hits, 0,
            "found {hits} eager .ok_or(ProgramError-from(...)) calls — \
             use .ok_or_else(|| ...) closure form to keep the error \
             construction (and its arlex_lang::log syscall) off the \
             success path (CU-hotfix 2026-05-18)",
        );
    }
}
