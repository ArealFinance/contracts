//! fund_distributor — permissionless RWT deposit into a distributor's reward vault.
//!
//! Applies vesting lock BEFORE increasing total_funded, so yield already vested
//! from prior deposits is preserved when new funds arrive.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::YdError;
use crate::events::DistributorFunded;
use crate::state::{DistributionConfig, MerkleDistributor};
use crate::validation::read_token_account_mint;
use crate::vesting;

#[derive(Accounts)]
pub struct FundDistributor<'info> {
    #[account(mut, signer)]
    pub depositor: &'info AccountView,

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

    // Source RWT ATA (owner = depositor).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub depositor_token: &'info AccountView,

    // Reward vault RWT ATA (must equal distributor.reward_vault).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub reward_vault: &'info AccountView,

    // Areal fee destination RWT ATA (must equal config.areal_fee_destination).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub fee_account: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(ctx: Context<FundDistributor>, amount: u64) -> Result<()> {
    let config = DistributionConfig::load(ctx.accounts.config, ctx.program_id)?;
    if !config.is_active {
        return Err(ProgramError::from(YdError::SystemPaused));
    }

    // `mut` binding: vesting/funding writes go through the guard's DerefMut. The
    // distributor account is NOT passed into the Transfer CPIs below (the
    // depositor signs), so the guard may stay live across them.
    let mut dist = MerkleDistributor::load_mut(ctx.accounts.distributor, ctx.program_id)?;
    if !dist.is_active {
        return Err(ProgramError::from(YdError::DistributorNotActive));
    }

    // --- Input validation ---
    if amount == 0 {
        return Err(ProgramError::from(YdError::ZeroAmount));
    }
    if amount < config.min_distribution_amount {
        return Err(ProgramError::from(YdError::BelowMinDistribution));
    }
    if dist.ot_mint != ctx.accounts.ot_mint.address().as_ref() {
        return Err(ProgramError::from(YdError::InvalidOtMint));
    }
    if ctx.accounts.reward_vault.address().as_ref() != dist.reward_vault.as_ref() {
        return Err(ProgramError::from(YdError::InvalidRewardVault));
    }
    if ctx.accounts.fee_account.address().as_ref() != config.areal_fee_destination.as_ref() {
        return Err(ProgramError::from(YdError::InvalidFeeAccount));
    }

    // Mint sanity: all three token accounts must hold RWT.
    let depositor_mint = read_token_account_mint(ctx.accounts.depositor_token)?;
    let vault_mint = read_token_account_mint(ctx.accounts.reward_vault)?;
    let fee_mint = read_token_account_mint(ctx.accounts.fee_account)?;
    if depositor_mint != RWT_MINT || vault_mint != RWT_MINT || fee_mint != RWT_MINT {
        return Err(ProgramError::from(YdError::InvalidTokenAccount));
    }

    // --- Fee math (BPS of gross amount) ---
    let fee =
        arlex_lang::math::mul_div_u64(amount, config.protocol_fee_bps as u64, BPS_DENOMINATOR)
            .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
    let net = amount
        .checked_sub(fee)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;

    // --- Effects (CEI: update state before CPIs) ---
    let now = Clock::get()?.unix_timestamp;
    // `&mut *dist`: DerefMut through the guard yields the `&mut MerkleDistributor`
    // the by-value-`&mut Self` helper expects.
    vesting::lock_vesting(&mut *dist, now)?;
    dist.total_funded = dist
        .total_funded
        .checked_add(net)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
    dist.last_fund_ts = now;

    let total_funded = dist.total_funded;
    let locked_vested = dist.locked_vested;
    let ot_mint_bytes = dist.ot_mint;

    // --- Interactions: transfer fee then net ---
    if fee > 0 {
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.depositor_token,
            to: ctx.accounts.fee_account,
            authority: ctx.accounts.depositor,
            amount: fee,
        }
        .invoke()?;
    }

    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.depositor_token,
        to: ctx.accounts.reward_vault,
        authority: ctx.accounts.depositor,
        amount: net,
    }
    .invoke()?;

    // --- Emit ---
    emit!(DistributorFunded {
        ot_mint: ot_mint_bytes,
        amount,
        protocol_fee: fee,
        total_funded,
        locked_vested,
        timestamp: now,
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
        const SRC: &str = include_str!("fund_distributor.rs");
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
