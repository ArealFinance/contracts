//! Vesting math for Yield Distribution.
//!
//! All multiplications cast to u128 BEFORE multiply to prevent overflow.
//! Per-claimant share uses SATURATING subtraction so a holder whose cumulative
//! dropped (because off-chain algorithm reweighted the tree between epochs)
//! cannot underflow.

use arlex_lang::prelude::*;

use crate::constants::MIN_VESTED_AMOUNT;
use crate::error::YdError;
use crate::state::MerkleDistributor;

/// Called at each fund event. Locks in yield that has already vested
/// before applying the new deposit, so past holders retain their share.
pub fn lock_vesting(dist: &mut MerkleDistributor, now: i64) -> Result<()> {
    let elapsed = (now - dist.last_fund_ts).max(0) as u64;
    let period = (dist.vesting_period_secs as u64).max(1);
    let capped = elapsed.min(period);

    let unvested = dist
        .total_funded
        .checked_sub(dist.locked_vested)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;

    let increment = arlex_lang::math::mul_div_u128_u64(unvested as u128, capped, period)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))? as u64;

    dist.locked_vested = dist
        .locked_vested
        .checked_add(increment)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
    Ok(())
}

/// Total vested RWT at time `now`, capped at max_total_claim,
/// floored at min(MIN_VESTED_AMOUNT, max_total_claim).
pub fn calculate_total_vested(dist: &MerkleDistributor, now: i64) -> Result<u64> {
    let elapsed = (now - dist.last_fund_ts).max(0) as u64;
    let period = (dist.vesting_period_secs as u64).max(1);
    let capped = elapsed.min(period);

    let new_portion = dist
        .total_funded
        .checked_sub(dist.locked_vested)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
    let new_vested = arlex_lang::math::mul_div_u128_u64(new_portion as u128, capped, period)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))? as u64;

    let total = dist
        .locked_vested
        .checked_add(new_vested)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
    let capped_total = total.min(dist.max_total_claim);
    let floor = MIN_VESTED_AMOUNT.min(dist.max_total_claim);
    Ok(capped_total.max(floor))
}

/// Per-claimant share. SATURATING subtraction — if the holder's cumulative shrank
/// (e.g. they sold OT between snapshots), claimable floors at 0 rather than
/// underflowing.
pub fn calculate_claimable(
    total_vested: u64,
    cumulative_amount: u64,
    max_total_claim: u64,
    already_claimed: u64,
) -> Result<u64> {
    if max_total_claim == 0 {
        // Defense-in-depth: publish_root requires max_total_claim > 0, so this
        // should be unreachable during a live claim. We still guard to avoid
        // a later divide-by-zero inside mul_div.
        return Ok(0);
    }
    let my_share =
        arlex_lang::math::mul_div_u128_u64(total_vested as u128, cumulative_amount, max_total_claim)
            .ok_or_else(|| ProgramError::from(YdError::MathOverflow))? as u64;
    Ok(my_share.saturating_sub(already_claimed))
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
        const SRC: &str = include_str!("vesting.rs");
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
