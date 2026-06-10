use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::MIN_CAPITAL_FLOOR;
use crate::error::RwtError;
use crate::events::CapitalAdjusted;
use crate::nav::calculate_nav;
use crate::state::RwtVault;

#[derive(Accounts)]
pub struct AdjustCapital<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "RwtVault",
        seeds = [b"rwt_vault"], bump
    )]
    pub rwt_vault: &'info AccountView,
}

pub fn handler(ctx: Context<AdjustCapital>, writedown_amount: u64) -> Result<()> {
    // `mut` binding: capital/NAV writes go through the guard's DerefMut. No CPI.
    let mut vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    // --- Checks ---
    if writedown_amount == 0 {
        return Err(ProgramError::from(RwtError::ZeroAmount));
    }

    let old_capital = vault.total_invested_capital;
    let old_nav = vault.nav_book_value;

    let new_capital = vault.total_invested_capital
        .checked_sub(writedown_amount as u128)
        .ok_or_else(|| ProgramError::from(RwtError::InsufficientCapital))?;

    if new_capital < MIN_CAPITAL_FLOOR as u128 {
        return Err(ProgramError::from(RwtError::InsufficientCapital));
    }

    // --- Effects ---
    vault.total_invested_capital = new_capital;
    vault.nav_book_value = calculate_nav(new_capital, vault.total_rwt_supply)?;

    let new_nav = vault.nav_book_value;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(CapitalAdjusted {
        old_capital,
        new_capital,
        writedown_amount,
        old_nav,
        new_nav,
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
        const SRC: &str = include_str!("adjust_capital.rs");
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
