use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::OtError;
use crate::events::OtMinted;
use crate::state::*;

#[derive(Accounts)]
pub struct MintOt<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    // OtGovernance PDA — validated via seeds + has_one
    #[account(
        has_one = authority, account_type = "OtGovernance",
        seeds = [b"ot_governance", ot_mint.address().as_ref()], bump
    )]
    pub ot_governance: &'info AccountView,

    // OtConfig PDA — validated via seeds
    #[account(
        mut,
        seeds = [b"ot_config", ot_mint.address().as_ref()], bump
    )]
    pub ot_config: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    #[account(mut)]
    pub recipient_token_account: &'info AccountView,

    pub recipient: &'info AccountView,

    #[account(mut, signer)]
    pub payer: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,

    // ATA program — required for CreateIdempotent CPI
    #[account(constraint = ata_program.address() == &Address::new_from_array(ASSOCIATED_TOKEN_PROGRAM))]
    pub ata_program: &'info AccountView,
}

pub fn handler(ctx: Context<MintOt>, amount: u64) -> Result<()> {
    if amount == 0 {
        return Err(ProgramError::from(OtError::ZeroAmount));
    }

    // Check is_active
    let governance = OtGovernance::load(ctx.accounts.ot_governance, ctx.program_id)?;
    if !governance.is_active {
        return Err(ProgramError::from(OtError::GovernanceInactive));
    }

    // Load OtConfig (PDA already validated via seeds constraint)
    let ot_config = OtConfig::load_mut(ctx.accounts.ot_config, ctx.program_id)?;

    // Update total_minted (checked_add)
    ot_config.total_minted = ot_config.total_minted
        .checked_add(amount)
        .ok_or_else(|| ProgramError::from(OtError::MathOverflow))?;

    // Create ATA if it doesn't exist (CreateIdempotent — no-op if exists)
    arlex_lang::associated_token::instructions::CreateIdempotent {
        funding_account: ctx.accounts.payer,
        account: ctx.accounts.recipient_token_account,
        wallet: ctx.accounts.recipient,
        mint: ctx.accounts.ot_mint,
        system_program: ctx.accounts.system_program,
        token_program: ctx.accounts.token_program,
    }.invoke()?;

    // MintTo via OtConfig PDA signer
    let bump = [ot_config.bump];
    let seeds = [
        Seed::from(b"ot_config" as &[u8]),
        Seed::from(ot_config.ot_mint.as_ref()),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    arlex_lang::token::instructions::MintTo {
        mint: ctx.accounts.ot_mint,
        account: ctx.accounts.recipient_token_account,
        mint_authority: ctx.accounts.ot_config,
        amount,
    }.invoke_signed(&[signer])?;

    // Emit event
    let clock = Clock::get()?;
    emit!(OtMinted {
        ot_mint: ot_config.ot_mint,
        recipient: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.recipient.address().as_ref());
            arr
        },
        amount,
        new_total_minted: ot_config.total_minted,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("OT minted");
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
        const SRC: &str = include_str!("mint_ot.rs");
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
