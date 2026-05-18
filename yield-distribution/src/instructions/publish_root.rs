//! publish_root — publish authority (server wallet) publishes a new merkle root.
//!
//! Enforces `max_total_claim > 0`, `== total_funded`, `>= total_claimed` to
//! prevent inflation, division-by-zero, or rewinding below already-claimed.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::YdError;
use crate::events::RootPublished;
use crate::state::{DistributionConfig, MerkleDistributor};

#[derive(Accounts)]
pub struct PublishRoot<'info> {
    #[account(signer)]
    pub publish_authority: &'info AccountView,

    #[account(seeds = [b"dist_config"], bump)]
    pub config: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    // NOTE: account_type requires has_one (Arlex constraint). Discriminator checked by load_mut.
    #[account(
        mut,
        seeds = [b"merkle_dist", ot_mint.address().as_ref()], bump
    )]
    pub distributor: &'info AccountView,
}

pub fn handler(
    ctx: Context<PublishRoot>,
    merkle_root: [u8; 32],
    max_total_claim: u64,
) -> Result<()> {
    let config = DistributionConfig::load(ctx.accounts.config, ctx.program_id)?;
    if !config.is_active {
        return Err(ProgramError::from(YdError::SystemPaused));
    }

    // Publish authority check
    if ctx.accounts.publish_authority.address().as_ref() != config.publish_authority.as_ref() {
        return Err(ProgramError::from(YdError::UnauthorizedPublisher));
    }

    let dist = MerkleDistributor::load_mut(ctx.accounts.distributor, ctx.program_id)?;
    if !dist.is_active {
        return Err(ProgramError::from(YdError::DistributorNotActive));
    }
    if dist.ot_mint != ctx.accounts.ot_mint.address().as_ref() {
        return Err(ProgramError::from(YdError::InvalidOtMint));
    }

    // --- Validation order matters ---
    if max_total_claim == 0 {
        return Err(ProgramError::from(YdError::ZeroMaxClaim));
    }
    if max_total_claim != dist.total_funded {
        return Err(ProgramError::from(YdError::InvalidMaxClaim));
    }
    if max_total_claim < dist.total_claimed {
        return Err(ProgramError::from(YdError::MaxClaimBelowClaimed));
    }

    // --- Apply ---
    dist.merkle_root = merkle_root;
    dist.max_total_claim = max_total_claim;
    dist.epoch = dist
        .epoch
        .checked_add(1)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;

    let epoch = dist.epoch;
    let ot_mint_bytes = dist.ot_mint;

    let clock = Clock::get()?;
    emit!(RootPublished {
        ot_mint: ot_mint_bytes,
        epoch,
        merkle_root,
        max_total_claim,
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
        const SRC: &str = include_str!("publish_root.rs");
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
