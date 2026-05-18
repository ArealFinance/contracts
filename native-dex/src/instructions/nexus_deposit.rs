//! Layer 9 §4.2 — `nexus_deposit`. Permissionless deposit of USDC or RWT into
//! the Nexus singleton's token ATA. Bumps the principal counter for the matching
//! `token_kind` and emits `NexusDeposited` with `source_kind = SOURCE_DIRECT`.
//!
//! Counter semantics (SD-2 / D16): `total_deposited_<token>` is monotonically
//! non-decreasing; it never reflects impermanent loss or fee accrual. The
//! counter is the on-chain principal floor consumed by `nexus_withdraw_profits`
//! to enforce that profits-only flow to Treasury (principal-lock).
//!
//! Caller signs the inbound SPL Transfer (no PDA seeds required); the Nexus
//! is **not** the authority of the source ATA. The destination ATA must be
//! owned by the Nexus PDA — we validate the owner field directly to guard
//! against a malicious caller passing an ATA that happens to match the
//! Nexus's mint but is owned by an arbitrary wallet (which would inflate the
//! counter without putting funds under Nexus control — MED-class issue).

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::NexusDeposited;
use crate::state::LiquidityNexus;
use crate::validation::{pubkey_bytes, read_token_account_mint, read_token_account_owner};

#[derive(Accounts)]
pub struct NexusDeposit<'info> {
    /// External depositor — any wallet may call. Pays gas and signs the
    /// inbound SPL Transfer (the source ATA's authority).
    #[account(mut, signer)]
    pub depositor: &'info AccountView,

    /// Nexus singleton — counter is bumped here. `is_active` must be true;
    /// otherwise we revert before touching any token account so the deposit
    /// is not silently absorbed into a disabled Nexus.
    #[account(mut, seeds = [LIQUIDITY_NEXUS_SEED], bump)]
    pub liquidity_nexus: &'info AccountView,

    /// Source ATA. SPL Token program ownership constraint ensures we are
    /// looking at a genuine Token account; the inner Transfer CPI re-checks
    /// authority against `depositor.address()` per SPL semantics.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub depositor_token_ata: &'info AccountView,

    /// Destination ATA — must be owned by the Nexus PDA at the SPL level
    /// (i.e. the Token-account `owner` field, NOT the on-chain account
    /// owner). Handler validates this directly against
    /// `liquidity_nexus.address()` so a malicious depositor cannot bump
    /// `total_deposited_<token>` against an ATA that the Nexus does not
    /// control.
    //
    // Note: the SPL Token program account is required in the on-chain TX
    // accounts list (Solana runtime must load it for the inner Transfer
    // CPI to dispatch). It is intentionally NOT a named field of this
    // Accounts struct because (a) `arlex_lang::token::instructions::Transfer`
    // (pinocchio_token) hardcodes the program ID and (b) BPF stack-frame
    // budget (entrypoint dispatcher's max frame size) is binding for this
    // program — every saved 8-byte slot keeps the dispatcher within the
    // 4096-byte cap. Callers pass the token program via remaining_accounts.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_ata: &'info AccountView,
}

#[inline(never)]
pub fn handler(
    ctx: Context<NexusDeposit>,
    amount: u64,
    token_kind: u8,
) -> Result<()> {
    // `#[inline(never)]` keeps the handler's locals (mint/owner reads, CPI
    // builder) out of the BPF entrypoint dispatcher's stack frame. The
    // dispatcher emits one match arm per ix; each arm references the
    // handler's locals through a tail-call boundary unless inlining
    // collapses them into the dispatcher frame. With 4 new ix added in
    // Layer 9, the dispatcher would otherwise overflow its 4096-byte
    // budget — `inline(never)` keeps each handler's frame separate.
    // 1. Amount sanity — fail-fast on `amount = 0` so the SPL Transfer is
    //    never even attempted (saves CU + avoids an event with 0 amount
    //    bumping the counter into a no-op).
    if amount == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount));
    }

    // 2. Validate token_kind tag and select the canonical mint that the
    //    destination ATA must hold. `token_kind` is the public-API tag the
    //    caller passes; we verify the destination ATA's `mint` matches that
    //    tag's canonical pubkey to prevent a caller from depositing an
    //    arbitrary SPL token and bumping a counter that does not reflect
    //    the actual deposit.
    let expected_mint: [u8; 32] = match token_kind {
        TOKEN_KIND_USDC => USDC_MINT,
        TOKEN_KIND_RWT => RWT_MINT,
        _ => return Err(ProgramError::from(DexError::InvalidNexusToken).into()),
    };

    // 3. Destination ATA mint must equal the canonical mint for `token_kind`.
    //    Note: in current `constants.rs`, `USDC_MINT` is all-zero (set per
    //    deployment, see file header). For the USDC path we still require the
    //    on-chain ATA mint to equal that canonical value at runtime, so this
    //    check is meaningful once the mint is pinned at deployment. RWT path
    //    hits the `RWT_MINT` vanity bytes already pinned in `constants.rs`.
    let nexus_ata_mint = read_token_account_mint(ctx.accounts.nexus_token_ata)?;
    if nexus_ata_mint != expected_mint {
        return Err(ProgramError::from(DexError::InvalidNexusToken).into());
    }

    // 4. Destination ATA SPL-owner must equal Nexus PDA. Without this guard
    //    a malicious caller could pass a Nexus-mint-matching ATA they own,
    //    walk away with the funds, and leave the on-chain
    //    `total_deposited_<token>` falsely inflated — locking ARL revenue
    //    via the principal-floor invariant in `nexus_withdraw_profits`.
    let nexus_ata_owner = read_token_account_owner(ctx.accounts.nexus_token_ata)?;
    let nexus_addr = pubkey_bytes(ctx.accounts.liquidity_nexus);
    if nexus_ata_owner != nexus_addr {
        return Err(ProgramError::from(DexError::InvalidTokenAccount).into());
    }

    // 5. Activate-gate + counter bump. Scoped block so the mut handle is
    //    dropped before the CPI; the CEI ordering (Effects-before-Interactions)
    //    matters for re-entrancy safety even though SPL Transfer cannot
    //    re-enter this program — defence-in-depth against future CPIs landing
    //    in this handler.
    let new_total: u64 = {
        let nexus = LiquidityNexus::load_mut(
            ctx.accounts.liquidity_nexus,
            ctx.program_id,
        )?;
        if !nexus.is_active {
            return Err(ProgramError::from(DexError::NexusNotActive).into());
        }
        match token_kind {
            TOKEN_KIND_USDC => {
                nexus.total_deposited_usdc = nexus
                    .total_deposited_usdc
                    .checked_add(amount)
                    .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
                nexus.total_deposited_usdc
            }
            TOKEN_KIND_RWT => {
                nexus.total_deposited_rwt = nexus
                    .total_deposited_rwt
                    .checked_add(amount)
                    .ok_or_else(|| ProgramError::from(DexError::MathOverflow))?;
                nexus.total_deposited_rwt
            }
            _ => unreachable!(), // covered by step 2 above.
        }
    };

    // 6. Inbound SPL Transfer — depositor signs (no PDA seeds; the Nexus is
    //    NOT the authority of the source ATA). Fails on insufficient balance,
    //    frozen mint, etc., per SPL Token semantics.
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.depositor_token_ata,
        to: ctx.accounts.nexus_token_ata,
        authority: ctx.accounts.depositor,
        amount,
    }
    .invoke()?;

    // 7. Emit. `source_kind = SOURCE_DIRECT` distinguishes this path from the
    //    YD-CPI `nexus_record_deposit` path (`SOURCE_LIQUIDITY_HOLDING`).
    let clock = Clock::get()?;
    emit!(NexusDeposited {
        token_mint: expected_mint,
        amount,
        new_total_deposited: new_total,
        source_kind: NEXUS_DEPOSIT_SOURCE_DIRECT,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Pure-Rust counter-bump invariants for `nexus_deposit`. Handler-level
    //! negative ACs (signer absent, ATA owner mismatch) require the BPF
    //! runtime; here we pin the counter math + token-kind dispatch so a
    //! refactor cannot silently regress monotonicity or route to the wrong
    //! counter slot.
    use super::*;
    use crate::state::LiquidityNexus;

    fn make_nexus(usdc: u64, rwt: u64) -> LiquidityNexus {
        // SAFETY: `LiquidityNexus` is `#[repr(C, packed)]` with all-primitive
        // fields summing to 50 bytes; an all-zero bit pattern is valid for
        // every field (see `state::tests::liquidity_nexus_default_uninitialized`).
        let buf = [0u8; core::mem::size_of::<LiquidityNexus>()];
        let mut nexus: LiquidityNexus =
            unsafe { core::ptr::read(buf.as_ptr() as *const LiquidityNexus) };
        nexus.is_active = true;
        nexus.total_deposited_usdc = usdc;
        nexus.total_deposited_rwt = rwt;
        nexus.bump = 0xFD;
        nexus
    }

    /// USDC deposit increments `total_deposited_usdc` and leaves
    /// `total_deposited_rwt` untouched.
    #[test]
    fn nexus_deposit_increments_counter_usdc() {
        let mut nexus = make_nexus(/* usdc */ 100, /* rwt */ 50);
        let amount: u64 = 250;

        // Mirror the handler's counter bump for `TOKEN_KIND_USDC`.
        let token_kind = TOKEN_KIND_USDC;
        match token_kind {
            TOKEN_KIND_USDC => {
                nexus.total_deposited_usdc = nexus
                    .total_deposited_usdc
                    .checked_add(amount)
                    .unwrap();
            }
            TOKEN_KIND_RWT => {
                nexus.total_deposited_rwt = nexus
                    .total_deposited_rwt
                    .checked_add(amount)
                    .unwrap();
            }
            _ => unreachable!(),
        }

        assert_eq!({ nexus.total_deposited_usdc }, 350);
        assert_eq!({ nexus.total_deposited_rwt }, 50);
    }

    /// RWT deposit increments `total_deposited_rwt` and leaves
    /// `total_deposited_usdc` untouched.
    #[test]
    fn nexus_deposit_increments_counter_rwt() {
        let mut nexus = make_nexus(/* usdc */ 100, /* rwt */ 50);
        let amount: u64 = 999;

        let token_kind = TOKEN_KIND_RWT;
        match token_kind {
            TOKEN_KIND_USDC => {
                nexus.total_deposited_usdc = nexus
                    .total_deposited_usdc
                    .checked_add(amount)
                    .unwrap();
            }
            TOKEN_KIND_RWT => {
                nexus.total_deposited_rwt = nexus
                    .total_deposited_rwt
                    .checked_add(amount)
                    .unwrap();
            }
            _ => unreachable!(),
        }

        assert_eq!({ nexus.total_deposited_usdc }, 100);
        assert_eq!({ nexus.total_deposited_rwt }, 1_049);
    }

    /// Counter overflow path — `checked_add` saturating at `u64::MAX` must
    /// surface `MathOverflow` rather than silently wrapping. Pin the error
    /// variant the handler returns so dashboards/bots decode it correctly.
    #[test]
    fn nexus_deposit_overflow_returns_math_overflow() {
        let mut nexus = make_nexus(/* usdc */ u64::MAX, /* rwt */ 0);
        // Mirror handler step: any non-zero `amount` overflows.
        let result = nexus
            .total_deposited_usdc
            .checked_add(1)
            .ok_or_else(|| ProgramError::from(DexError::MathOverflow));
        assert!(result.is_err());
        match result.unwrap_err() {
            ProgramError::Custom(code) => {
                let want: u32 = match ProgramError::from(DexError::MathOverflow) {
                    ProgramError::Custom(c) => c,
                    _ => panic!("MathOverflow must lower to Custom"),
                };
                assert_eq!(code, want);
            }
            other => panic!("expected Custom, got {:?}", other),
        }
    }


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
        const SRC: &str = include_str!("nexus_deposit.rs");
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
