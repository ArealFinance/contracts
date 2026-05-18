use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::FutarchyError;
use crate::events::ProposalCreated;
use crate::state::*;

#[derive(Accounts)]
pub struct CreateProposal<'info> {
    #[account(mut, signer)]
    pub authority: &'info AccountView,

    #[account(
        mut,
        has_one = authority, account_type = "FutarchyConfig",
    )]
    pub config: &'info AccountView,

    #[account(mut)]
    pub proposal: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<CreateProposal>,
    proposal_type: u8,
    amount: u64,
    destination: [u8; 32],
    token_mint: [u8; 32],
    params_hash: [u8; 32],
) -> Result<()> {
    let config = FutarchyConfig::load_mut(ctx.accounts.config, ctx.program_id)?;

    if !config.is_active {
        return Err(ProgramError::from(FutarchyError::GovernancePaused));
    }

    // Validate proposal type and fields
    let zero = [0u8; 32];
    match proposal_type {
        PROPOSAL_TYPE_MINT_OT | PROPOSAL_TYPE_SPEND_TREASURY => {
            if amount == 0 {
                return Err(ProgramError::from(FutarchyError::ZeroAmount));
            }
            if destination == zero {
                return Err(ProgramError::from(FutarchyError::ZeroDestination));
            }
        }
        PROPOSAL_TYPE_UPDATE_DESTINATIONS => {
            if params_hash == [0u8; 32] {
                return Err(ProgramError::from(FutarchyError::EmptyParamsHash));
            }
        }
        _ => return Err(ProgramError::from(FutarchyError::InvalidProposalType)),
    }

    let proposal_id = config.next_proposal_id;
    config.next_proposal_id = config.next_proposal_id
        .checked_add(1)
        .ok_or_else(|| ProgramError::from(FutarchyError::MathOverflow))?;

    // Verify proposal PDA and create account
    let id_bytes = proposal_id.to_le_bytes();
    let (expected_pda, bump) = arlex_lang::find_program_address(
        &[b"proposal", ctx.accounts.config.address().as_ref(), id_bytes.as_ref()],
        ctx.program_id,
    );
    if ctx.accounts.proposal.address() != &expected_pda {
        return Err(ProgramError::InvalidSeeds);
    }

    // Create proposal account
    let space = 203u64;
    let rent = pinocchio::sysvars::rent::Rent::get()?;
    let lamports = rent.try_minimum_balance(space as usize)?;

    let bump_bytes = [bump];
    let seeds = [
        Seed::from(b"proposal" as &[u8]),
        Seed::from(ctx.accounts.config.address().as_ref()),
        Seed::from(id_bytes.as_ref()),
        Seed::from(bump_bytes.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    arlex_lang::system::instructions::CreateAccount {
        from: ctx.accounts.authority,
        to: ctx.accounts.proposal,
        lamports,
        space,
        owner: ctx.program_id,
    }.invoke_signed(&[signer])?;

    // Initialize proposal data
    let proposal = Proposal::init(ctx.accounts.proposal, ctx.program_id)?;

    let mut authority_bytes = [0u8; 32];
    authority_bytes.copy_from_slice(ctx.accounts.authority.address().as_ref());

    proposal.proposal_id = proposal_id;
    proposal.ot_mint = config.ot_mint;
    proposal.proposer = authority_bytes;
    proposal.proposal_type = proposal_type;
    proposal.amount = amount;
    proposal.destination = destination;
    proposal.token_mint = token_mint;
    proposal.params_hash = params_hash;
    proposal.status = STATUS_ACTIVE;

    let clock = Clock::get()?;
    proposal.created_ts = clock.unix_timestamp;
    proposal.executed_ts = 0;
    proposal.bump = bump;

    emit!(ProposalCreated {
        proposal_id,
        ot_mint: config.ot_mint,
        proposer: authority_bytes,
        proposal_type,
        amount,
        destination,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Proposal created");
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
        const SRC: &str = include_str!("create_proposal.rs");
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
