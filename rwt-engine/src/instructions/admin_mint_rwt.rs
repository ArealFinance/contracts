//! admin_mint_rwt — privileged mint backed by off-chain capital.
//!
//! # Trust model (M-5 audit note)
//!
//! This instruction lets the vault `authority` mint an arbitrary amount of RWT
//! tokens as long as they assert a corresponding `backing_capital_usd`. NO
//! on-chain source confirms that capital was actually deposited (e.g. an audit
//! attestation, bank statement, or real-world asset custody proof).
//!
//! Consequence: anyone with the vault authority key can inflate RWT supply
//! without matching real-world backing. The protocol's NAV invariant depends
//! entirely on the authority's off-chain honesty here.
//!
//! Mitigations in place:
//! - 2-step authority transfer (prevents instant takeover)
//! - `is_active` can be flipped to pause the vault independently
//! - Events emitted with full amount + capital for auditors
//!
//! Mitigations NOT in place (room for future work):
//! - Per-call cap on `rwt_amount` / `backing_capital_usd`
//! - Multi-sig on vault authority
//! - On-chain oracle attestation (reserves proof)
//!
//! Operators: restrict vault authority to a hardware-signer multi-sig
//! before any mainnet deployment.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::SPL_TOKEN_PROGRAM;
use crate::error::RwtError;
use crate::events::RwtMinted;
use crate::nav::calculate_nav;
use crate::state::RwtVault;

#[derive(Accounts)]
pub struct AdminMintRwt<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "RwtVault",
        seeds = [b"rwt_vault"], bump
    )]
    pub rwt_vault: &'info AccountView,

    // RWT mint, authority = vault PDA
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    // Recipient RWT ATA
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub recipient_rwt: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<AdminMintRwt>,
    rwt_amount: u64,
    backing_capital_usd: u64,
) -> Result<()> {
    // CHECKS + EFFECTS in a scope that drops the RwtVault borrow guard before
    // the MintTo CPI below (rwt_vault is the mint_authority signer). Copy out the
    // bump and post-update NAV; the guard's Drop releases the borrow flag here.
    let (nav_after, vault_bump);
    {
    let mut vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    // --- Checks ---
    // NOTE: admin_mint_rwt is NOT blocked by mint_paused (design decision per spec)
    if rwt_amount == 0 {
        return Err(ProgramError::from(RwtError::ZeroAmount));
    }
    if backing_capital_usd == 0 {
        return Err(ProgramError::from(RwtError::ZeroBackingCapital));
    }

    // Validate mint matches vault
    if ctx.accounts.rwt_mint.address().as_ref() != vault.rwt_mint.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // SECURITY: Validate recipient_rwt holds vault's RWT mint (defense-in-depth)
    let recipient_mint = crate::validation::read_token_account_mint(ctx.accounts.recipient_rwt)?;
    if recipient_mint != vault.rwt_mint {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // --- Effects: update vault state ---
    vault.total_invested_capital = vault.total_invested_capital
        .checked_add(backing_capital_usd as u128)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    vault.total_rwt_supply = vault.total_rwt_supply
        .checked_add(rwt_amount)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    vault.nav_book_value = calculate_nav(vault.total_invested_capital, vault.total_rwt_supply)?;

    nav_after = vault.nav_book_value;
    vault_bump = vault.bump;
    } // RwtVault guard dropped here — borrow flag released before the MintTo CPI.

    // --- Interactions: Vault PDA mints RWT to recipient ---
    let bump = [vault_bump];
    let seeds = [
        Seed::from(b"rwt_vault" as &[u8]),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    arlex_lang::token::instructions::MintTo {
        mint: ctx.accounts.rwt_mint,
        account: ctx.accounts.recipient_rwt,
        mint_authority: ctx.accounts.rwt_vault,
        amount: rwt_amount,
    }.invoke_signed(&[signer])?;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(RwtMinted {
        user: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.authority.address().as_ref());
            arr
        },
        deposit_amount: backing_capital_usd,
        rwt_amount,
        fee_vault: 0,
        fee_dao: 0,
        nav_after,
        is_admin: true,
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
        const SRC: &str = include_str!("admin_mint_rwt.rs");
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
