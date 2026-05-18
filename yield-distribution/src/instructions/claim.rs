//! claim — merkle-proof-verified RWT claim for OT holders.
//!
//! Critical security guards:
//!   * proof.len() <= MAX_PROOF_LEN
//!   * canonical SHA256 proof verification
//!   * saturating subtraction (holder may have sold OT between snapshots)
//!   * total_claimed + claimable <= max_total_claim
//!   * CEI: state mutation BEFORE CPI Transfer
//!
//! ClaimStatus is init-if-needed: if data_len == 0 we CreateAccount + seed
//! the struct. Replay is prevented by re-validating (claimant, distributor)
//! fields after load_mut.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, rent::Rent, Sysvar};

use crate::constants::*;
use crate::error::YdError;
use crate::events::RewardsClaimed;
use crate::merkle;
use crate::state::{ClaimStatus, DistributionConfig, MerkleDistributor};
use crate::validation::{read_token_account_mint, read_token_account_owner};
use crate::vesting;

#[derive(Accounts)]
pub struct Claim<'info> {
    #[account(signer)]
    pub claimant: &'info AccountView, // holder wallet or PDA signer

    #[account(mut, signer)]
    pub payer: &'info AccountView, // pays rent for ClaimStatus init

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

    // ClaimStatus PDA — may be uninitialised on first claim.
    // NOT using #[account(init_if_needed)] (not available in arlex-derive);
    // we handle init manually inside the handler.
    #[account(
        mut,
        seeds = [b"claim_status", distributor.address().as_ref(), claimant.address().as_ref()],
        bump
    )]
    pub claim_status: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub reward_vault: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub claimant_token: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<Claim>,
    cumulative_amount: u64,
    proof: alloc::vec::Vec<[u8; 32]>,
) -> Result<()> {
    // --- Config / distributor state ---
    let config = DistributionConfig::load(ctx.accounts.config, ctx.program_id)?;
    if !config.is_active {
        return Err(ProgramError::from(YdError::SystemPaused));
    }

    let dist = MerkleDistributor::load_mut(ctx.accounts.distributor, ctx.program_id)?;
    if !dist.is_active {
        return Err(ProgramError::from(YdError::DistributorNotActive));
    }
    if dist.epoch == 0 {
        return Err(ProgramError::from(YdError::RootNotPublished));
    }
    if dist.ot_mint != ctx.accounts.ot_mint.address().as_ref() {
        return Err(ProgramError::from(YdError::InvalidOtMint));
    }

    if proof.len() > MAX_PROOF_LEN {
        return Err(ProgramError::from(YdError::ProofTooLong));
    }

    // --- Reward vault + claimant ATA checks ---
    if ctx.accounts.reward_vault.address().as_ref() != dist.reward_vault.as_ref() {
        return Err(ProgramError::from(YdError::InvalidRewardVault));
    }
    let claimant_mint = read_token_account_mint(ctx.accounts.claimant_token)?;
    if claimant_mint != RWT_MINT {
        return Err(ProgramError::from(YdError::InvalidTokenAccount));
    }

    // --- Merkle proof verification ---
    let claimant_bytes = {
        let mut a = [0u8; 32];
        a.copy_from_slice(ctx.accounts.claimant.address().as_ref());
        a
    };

    // M-5: claimant_token.owner must equal claimant signer.
    // Prevents a proof holder from redirecting RWT to a foreign ATA (or a hostile
    // ATA of the same mint) — the token account must be owned by the signer.
    let claimant_owner = read_token_account_owner(ctx.accounts.claimant_token)?;
    if claimant_owner != claimant_bytes {
        return Err(ProgramError::from(YdError::InvalidClaimantTokenOwner));
    }
    let distributor_bytes = {
        let mut a = [0u8; 32];
        a.copy_from_slice(ctx.accounts.distributor.address().as_ref());
        a
    };
    let ot_mint_bytes = dist.ot_mint;

    let leaf = merkle::compute_leaf(&claimant_bytes, cumulative_amount);
    if !merkle::verify_merkle_proof(&proof, &dist.merkle_root, &leaf) {
        return Err(ProgramError::from(YdError::InvalidProof));
    }

    // --- ClaimStatus init-if-needed (manual) ---
    let cs_account = ctx.accounts.claim_status;
    let is_new = cs_account.data_len() == 0;

    if is_new {
        let (_, cs_bump) = arlex_lang::find_program_address(
            &[
                b"claim_status",
                ctx.accounts.distributor.address().as_ref(),
                ctx.accounts.claimant.address().as_ref(),
            ],
            ctx.program_id,
        );

        let rent = Rent::get()?;
        let lamports = rent.try_minimum_balance(ClaimStatus::SPACE)?;

        let cs_bump_arr = [cs_bump];
        let dist_seed_bytes = ctx.accounts.distributor.address();
        let claimant_seed_bytes = ctx.accounts.claimant.address();

        arlex_lang::system::instructions::CreateAccount {
            from: ctx.accounts.payer,
            to: cs_account,
            lamports,
            space: ClaimStatus::SPACE as u64,
            owner: ctx.program_id,
        }
        .invoke_signed(&[Signer::from(&[
            Seed::from(b"claim_status" as &[u8]),
            Seed::from(dist_seed_bytes.as_ref()),
            Seed::from(claimant_seed_bytes.as_ref()),
            Seed::from(cs_bump_arr.as_ref()),
        ])])?;

        let cs = ClaimStatus::init(cs_account, ctx.program_id)?;
        cs.claimant = claimant_bytes;
        cs.distributor = distributor_bytes;
        cs.claimed_amount = 0;
        cs.bump = cs_bump;
    }

    let cs = ClaimStatus::load_mut(cs_account, ctx.program_id)?;
    // Replay defense: make sure the PDA we just loaded actually matches the claimant/distributor.
    if cs.claimant != claimant_bytes || cs.distributor != distributor_bytes {
        return Err(ProgramError::from(YdError::InvalidClaimStatus));
    }

    // --- Vesting math ---
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let total_vested = vesting::calculate_total_vested(dist, now)?;
    let claimable = vesting::calculate_claimable(
        total_vested,
        cumulative_amount,
        dist.max_total_claim,
        cs.claimed_amount,
    )?;

    // --- Early exit for zero (success, no transfer) ---
    if claimable == 0 {
        emit!(RewardsClaimed {
            claimant: claimant_bytes,
            ot_mint: ot_mint_bytes,
            amount: 0,
            cumulative_claimed: cs.claimed_amount,
            timestamp: now,
        });
        return Ok(());
    }

    // --- Safety cap ---
    let new_total_claimed = dist
        .total_claimed
        .checked_add(claimable)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
    if new_total_claimed > dist.max_total_claim {
        return Err(ProgramError::from(YdError::ExceedsMaxClaim));
    }

    // --- Effects (before CPI) ---
    cs.claimed_amount = cs
        .claimed_amount
        .checked_add(claimable)
        .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
    dist.total_claimed = new_total_claimed;

    let cumulative_claimed = cs.claimed_amount;
    let dist_bump = dist.bump;

    // --- Interactions: reward_vault -> claimant_token, signed by distributor PDA ---
    let bump_arr = [dist_bump];
    let seeds = [
        Seed::from(b"merkle_dist" as &[u8]),
        Seed::from(ot_mint_bytes.as_ref()),
        Seed::from(bump_arr.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.reward_vault,
        to: ctx.accounts.claimant_token,
        authority: ctx.accounts.distributor,
        amount: claimable,
    }
    .invoke_signed(&[signer])?;

    emit!(RewardsClaimed {
        claimant: claimant_bytes,
        ot_mint: ot_mint_bytes,
        amount: claimable,
        cumulative_claimed,
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
        const SRC: &str = include_str!("claim.rs");
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
