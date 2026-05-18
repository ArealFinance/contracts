use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::RwtError;
use crate::events::RwtMinted;
use crate::nav::calculate_nav;
use crate::validation::read_token_account_mint;
use crate::state::RwtVault;

#[derive(Accounts)]
pub struct MintRwt<'info> {
    #[account(signer)]
    pub user: &'info AccountView,

    // NOTE: account_type requires has_one (Arlex constraint). Discriminator checked by load_mut.
    #[account(mut, seeds = [b"rwt_vault"], bump)]
    pub rwt_vault: &'info AccountView,

    // RWT mint, authority = vault PDA
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    // User's USDC ATA (source of deposit)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_deposit: &'info AccountView,

    // User's RWT ATA (receives minted RWT)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_rwt: &'info AccountView,

    // Capital Accumulator USDC ATA (vault's USDC)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub capital_acc: &'info AccountView,

    // Areal fee destination ATA
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub dao_fee_account: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(ctx: Context<MintRwt>, amount: u64, min_rwt_out: u64) -> Result<()> {
    let vault = RwtVault::load_mut(ctx.accounts.rwt_vault, ctx.program_id)?;

    // --- Checks ---
    if vault.mint_paused {
        return Err(ProgramError::from(RwtError::MintPaused));
    }
    if amount == 0 {
        return Err(ProgramError::from(RwtError::ZeroAmount));
    }
    if amount < MIN_MINT_AMOUNT {
        return Err(ProgramError::from(RwtError::BelowMinMint));
    }

    // Validate accounts match vault state
    if ctx.accounts.rwt_mint.address().as_ref() != vault.rwt_mint.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }
    if ctx.accounts.capital_acc.address().as_ref() != vault.capital_accumulator_ata.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }
    if ctx.accounts.dao_fee_account.address().as_ref() != vault.areal_fee_destination.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // SECURITY: Validate token account mints (defense-in-depth, SPL Transfer also checks)
    // user_deposit must hold the same mint as capital_acc (USDC)
    let deposit_mint = read_token_account_mint(ctx.accounts.user_deposit)?;
    let capital_mint = read_token_account_mint(ctx.accounts.capital_acc)?;
    if deposit_mint != capital_mint {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }
    // SECURITY (M-6): dao_fee_account must also hold USDC (same mint as capital)
    let dao_fee_mint = read_token_account_mint(ctx.accounts.dao_fee_account)?;
    if dao_fee_mint != capital_mint {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }
    // user_rwt must hold vault's RWT mint
    let user_rwt_mint = read_token_account_mint(ctx.accounts.user_rwt)?;
    if user_rwt_mint != vault.rwt_mint {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // --- Fee math (use constants, not hardcoded /100) ---
    // Recalculate NAV from state to prevent stale-cache bugs (critical invariant)
    let nav = calculate_nav(vault.total_invested_capital, vault.total_rwt_supply)?;
    // CU-hotfix (2026-05-18): `ok_or(ProgramError::from(E))` eagerly evaluates
    // `From<E> for ProgramError`, which invokes `arlex_lang::log(msg)` even on
    // the success path (the value is constructed before `ok_or` checks the
    // Option's discriminant). On the smoke-3 mint-route path each call to
    // `mint_rwt` therefore emitted 8 spurious `"Arithmetic overflow"` log
    // syscalls and burned ~8 × (100 + 19) CUs of headroom — enough, combined
    // with the rest of the CPI cascade, to exhaust the 200K CU budget. The
    // fix is `ok_or_else(|| ...)` (closure → lazy), which only constructs
    // (and logs) the error on the actual overflow branch. See commit
    // message for the full smoke-3 trace and the 8-call fingerprint match.
    let fee_total = arlex_lang::math::mul_div_u64(amount, MINT_FEE_BPS, BPS_DENOMINATOR)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    // N-11: checked_div(2) can't fail for u64 / 2; use arithmetic shift for clarity and CU.
    let dao_fee = fee_total >> 1;
    let vault_fee = fee_total.checked_sub(dao_fee)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    let net_deposit = amount.checked_sub(fee_total)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;

    // RWT output: net_deposit * NAV_SCALE / nav
    let rwt_out = arlex_lang::math::mul_div_u64(net_deposit, NAV_SCALE, nav)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;

    // SECURITY: reject mint that would produce 0 RWT (user pays fee, gets nothing)
    if rwt_out == 0 {
        return Err(ProgramError::from(RwtError::ZeroRwtOutput));
    }

    // SECURITY: slippage protection — user MUST specify minimum acceptable output
    if min_rwt_out == 0 {
        return Err(ProgramError::from(RwtError::ZeroSlippage));
    }
    if rwt_out < min_rwt_out {
        return Err(ProgramError::from(RwtError::SlippageExceeded));
    }

    // --- Effects: update vault state BEFORE CPIs ---
    let capital_increase = (net_deposit as u128)
        .checked_add(vault_fee as u128)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    vault.total_invested_capital = vault.total_invested_capital
        .checked_add(capital_increase)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    vault.total_rwt_supply = vault.total_rwt_supply
        .checked_add(rwt_out)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    vault.nav_book_value = calculate_nav(vault.total_invested_capital, vault.total_rwt_supply)?;

    let nav_after = vault.nav_book_value;

    // --- Interactions: CPIs ---

    // 1. User transfers (net_deposit + vault_fee) → capital_acc
    let capital_transfer_amount = net_deposit.checked_add(vault_fee)
        .ok_or_else(|| ProgramError::from(RwtError::MathOverflow))?;
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.user_deposit,
        to: ctx.accounts.capital_acc,
        authority: ctx.accounts.user,
        amount: capital_transfer_amount,
    }.invoke()?;

    // 2. User transfers dao_fee → dao_fee_account
    if dao_fee > 0 {
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.user_deposit,
            to: ctx.accounts.dao_fee_account,
            authority: ctx.accounts.user,
            amount: dao_fee,
        }.invoke()?;
    }

    // 3. Vault PDA mints RWT to user
    let bump = [vault.bump];
    let seeds = [
        Seed::from(b"rwt_vault" as &[u8]),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    arlex_lang::token::instructions::MintTo {
        mint: ctx.accounts.rwt_mint,
        account: ctx.accounts.user_rwt,
        mint_authority: ctx.accounts.rwt_vault,
        amount: rwt_out,
    }.invoke_signed(&[signer])?;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(RwtMinted {
        user: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.user.address().as_ref());
            arr
        },
        deposit_amount: amount,
        rwt_amount: rwt_out,
        fee_vault: vault_fee,
        fee_dao: dao_fee,
        nav_after,
        is_admin: false,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Regression coverage for the 2026-05-18 CU-budget bug surfaced by the
    //! `smoke-swap.ts` Smoke 3 "Master pool USDC→RWT (mint-route)" scenario.
    //!
    //! Background. `Option::ok_or(err)` evaluates `err` eagerly — the
    //! `ProgramError::from(RwtError::MathOverflow)` expression runs on every
    //! call, even when the Option is `Some`. The arlex-derive `#[error_code]`
    //! lowering invokes `arlex_lang::log(msg)` inside the `From` impl, so
    //! every eager call emits a `sol_log_` syscall (~100 CUs base + 1 CU per
    //! byte of the message). `mint_rwt::handler` had exactly 8 such call
    //! sites — the same `8` that appeared as duplicated "Arithmetic overflow"
    //! lines in the failing trace, immediately before `Program DEX consumed
    //! 200000 of 200000 compute units`.
    //!
    //! Fix. Convert `ok_or(ProgramError::from(...))` to `ok_or_else(|| ...)`
    //! so the conversion (and its log) runs only on the real overflow branch.
    //!
    //! These tests pin the **arithmetic** of the Smoke 3 scenario so any
    //! future regression of the fee/RWT-output math is caught at `cargo test`
    //! without a BPF runtime. The CU regression itself is exercised end-to-end
    //! by `scripts/smoke-swap.ts` Smoke 3 in `verify-fresh-deploy.sh`.
    use super::*;

    /// Smoke 3 fixture replay: 10 USDC deposit at NAV ≈ 1.000309 must produce
    /// a non-zero RWT output without any overflow on the happy path.
    #[test]
    fn smoke3_mint_rwt_arithmetic_does_not_overflow() {
        let amount: u64 = 10_000_000; // 10 USDC (6 decimals)
        let nav: u64 = 1_000_309;     // freshly-bootstrapped vault NAV

        // Fee math (mirrors the handler line-for-line).
        let fee_total = arlex_lang::math::mul_div_u64(amount, MINT_FEE_BPS, BPS_DENOMINATOR)
            .expect("fee_total math");
        assert_eq!(fee_total, 100_000, "1% mint fee on 10 USDC");
        let dao_fee = fee_total >> 1;
        let vault_fee = fee_total.checked_sub(dao_fee).expect("vault_fee math");
        let net_deposit = amount.checked_sub(fee_total).expect("net_deposit math");
        assert_eq!(dao_fee, 50_000);
        assert_eq!(vault_fee, 50_000);
        assert_eq!(net_deposit, 9_900_000);

        // RWT output: net_deposit × NAV_SCALE / nav.
        let rwt_out = arlex_lang::math::mul_div_u64(net_deposit, NAV_SCALE, nav)
            .expect("rwt_out math");
        // 9_900_000 × 1_000_000 / 1_000_309 ≈ 9_896_942 (integer truncation).
        // Spot-check the order of magnitude — exact value within ±1 of the
        // Python reference computation is sufficient to pin the formula.
        assert!(
            (rwt_out as i64 - 9_896_942).abs() <= 1,
            "expected rwt_out ≈ 9_896_942, got {rwt_out}"
        );

        // Capital_increase: u128 cast prevents u64 overflow at high vault TVL.
        let capital_increase = (net_deposit as u128)
            .checked_add(vault_fee as u128)
            .expect("capital_increase math");
        assert_eq!(capital_increase, 9_950_000u128);

        // Final outbound transfer amount: net_deposit + vault_fee = full
        // capital that flows into capital_acc (DAO fee flows out separately).
        let capital_transfer_amount = net_deposit
            .checked_add(vault_fee)
            .expect("capital_transfer_amount math");
        assert_eq!(capital_transfer_amount, 9_950_000);
    }

    /// Sanity: the documented 8-call fingerprint of the bug. Counts the
    /// eager-pattern occurrences in this file's source so a future regression
    /// (adding a new eager call site) trips the test instead of silently
    /// re-introducing CU pressure.
    ///
    /// Implementation: `include_str!` pulls the file contents at compile
    /// time. We strip line-comments and any line that mentions the literal
    /// "ProgramError" inside a string/macro context (the regression test
    /// itself describes the pattern in prose and would otherwise self-trip).
    /// The detection key — `BAD_NEEDLE` — is reassembled from two halves so
    /// the source-byte search for the assembled needle does not match this
    /// test's own definition of it.
    #[test]
    fn mint_rwt_has_no_eager_ok_or_program_error() {
        const SRC: &str = include_str!("mint_rwt.rs");
        const HALF_1: &str = ".ok_or(ProgramError";
        const HALF_2: &str = "::from(";
        let bad_needle = alloc::format!("{HALF_1}{HALF_2}");
        // Walk the file line-by-line. Drop the comment tail and skip lines
        // whose code portion mentions the literal "BAD_NEEDLE" inside a
        // string (the assert_eq message below) — they would otherwise
        // self-trigger. A line is treated as commentary if it both contains
        // the bad needle and contains a quote character before it.
        let mut hits = 0usize;
        for raw_line in SRC.lines() {
            let line = match raw_line.find("//") {
                Some(idx) => &raw_line[..idx],
                None => raw_line,
            };
            // Skip lines that mention the bad needle inside a string literal
            // (i.e. a `"` appears before the needle). Production call sites
            // do not have a `"` to the left of the bad needle on the same
            // line — only the regression test's own message does.
            if let Some(needle_pos) = line.find(&bad_needle) {
                if line[..needle_pos].contains('"') {
                    continue;
                }
                hits += 1;
            }
        }
        assert_eq!(
            hits, 0,
            "found {hits} eager .ok_or(ProgramError-from(...)) calls in mint_rwt.rs — \
             use .ok_or_else closure form to keep the error construction \
             (and its arlex_lang-log syscall) off the success path",
        );
    }
}
