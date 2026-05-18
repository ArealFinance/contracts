use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::BPS_DENOMINATOR;
use crate::error::RwtError;
use crate::events::DistributionConfigUpdated;
use crate::state::{RwtVault, RwtDistributionConfig};

#[derive(Accounts)]
pub struct UpdateDistributionConfig<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        has_one = authority, account_type = "RwtVault",
        seeds = [b"rwt_vault"], bump
    )]
    pub rwt_vault: &'info AccountView,

    #[account(mut, seeds = [b"dist_config_rwt"], bump)]
    pub dist_config: &'info AccountView,
}

pub fn handler(
    ctx: Context<UpdateDistributionConfig>,
    book_value_bps: u16,
    liquidity_bps: u16,
    protocol_revenue_bps: u16,
    liquidity_destination: [u8; 32],
    protocol_revenue_destination: [u8; 32],
) -> Result<()> {
    // --- Validate destinations ---
    if liquidity_destination == [0u8; 32] || protocol_revenue_destination == [0u8; 32] {
        return Err(ProgramError::from(RwtError::ZeroDestination));
    }

    // --- Validate BPS sum ---
    let sum = (book_value_bps as u64)
        .checked_add(liquidity_bps as u64)
        .and_then(|s| s.checked_add(protocol_revenue_bps as u64))
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    if sum != BPS_DENOMINATOR {
        return Err(ProgramError::from(RwtError::InvalidDistributionRatios));
    }

    // --- Effects ---
    let config = RwtDistributionConfig::load_mut(ctx.accounts.dist_config, ctx.program_id)?;
    config.book_value_bps = book_value_bps;
    config.liquidity_bps = liquidity_bps;
    config.protocol_revenue_bps = protocol_revenue_bps;
    config.liquidity_destination = liquidity_destination;
    config.protocol_revenue_destination = protocol_revenue_destination;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(DistributionConfigUpdated {
        book_value_bps,
        liquidity_bps,
        protocol_revenue_bps,
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
        const SRC: &str = include_str!("update_distribution_config.rs");
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
