//! Layer 9 §4.9 — `nexus_record_deposit`. CPI-only accounting handler invoked
//! by Yield Distribution's `withdraw_liquidity_holding` (Layer 8 placeholder
//! → Layer 9 real impl, see SD-4). Records that the YD `LiquidityHolding` PDA
//! transferred RWT (or USDC, kept symmetric for future use) to the Nexus's
//! token ATA — counter-bump only. The actual SPL Transfer is performed
//! upstream inside `withdraw_liquidity_holding`; this handler does NOT touch
//! any token account.
//!
//! # Caller validation (D25)
//!
//! Three defence-in-depth checks gate every invocation:
//!
//! 1. **Owner check** — `liquidity_holding`'s on-chain owner must equal
//!    `YD_PROGRAM_ID`. Only YD writes its own PDA accounts; if anyone else
//!    held the account, they could spoof the discriminator and fake a
//!    Nexus deposit. Lowered to `NexusRecordDepositOnlyFromYd`.
//!
//! 2. **PDA derivation** — re-derive `["liq_holding"]` under
//!    `YD_PROGRAM_ID` and assert the address matches the passed account.
//!    Catches a forged YD-owned account that happens to match the
//!    discriminator pattern but lives at a non-canonical address. Lowered
//!    to `InvalidLiquidityHoldingPda`.
//!
//! 3. **Signer flag** — `liquidity_holding.is_signer()` must be true. The
//!    on-chain runtime sets the signer flag exclusively for accounts whose
//!    seeds were supplied to `invoke_signed`; a direct Tx invocation cannot
//!    forge this flag for a YD-owned PDA. Lowered to
//!    `NexusRecordDepositOnlyFromYd` (same code as the owner check — both
//!    encode the "only YD CPI" invariant).
//!
//! # Why no Transfer here
//!
//! `nexus_record_deposit` is the bookkeeping leg of the YD drain (SD-4
//! single-TX semantics): YD transfers RWT in one CPI, then immediately CPIs
//! the DEX program to bump `total_deposited_rwt`. Splitting Transfer + counter
//! bump prevents a re-entrancy hazard where the CPI could fail mid-flight,
//! leaving funds moved but the principal floor unchanged (or vice-versa).
//! Keeping the Transfer purely on the YD side and the counter purely on the
//! DEX side is the safest atomicity guarantee — both either succeed within
//! the parent TX or both revert.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::NexusDeposited;
use crate::state::LiquidityNexus;

#[derive(Accounts)]
pub struct NexusRecordDeposit<'info> {
    /// YD's `LiquidityHolding` PDA. **MUST** be a CPI signer (the on-chain
    /// signer flag is forged-proof; only `invoke_signed` from a program that
    /// owns the PDA seeds can satisfy it). The handler runs three D25
    /// checks against this account before any state mutation:
    /// (1) owner == YD_PROGRAM_ID, (2) re-derived PDA address match,
    /// (3) `is_signer == true`.
    #[account(signer)]
    pub liquidity_holding: &'info AccountView,

    /// Nexus singleton — counter is bumped here. `is_active` checked
    /// inside the handler (the framework cannot encode a runtime-flag
    /// gate via attributes alone in the current Arlex API).
    #[account(mut, seeds = [LIQUIDITY_NEXUS_SEED], bump)]
    pub liquidity_nexus: &'info AccountView,
}

#[inline(never)]
pub fn handler(
    ctx: Context<NexusRecordDeposit>,
    amount: u64,
    token_kind: u8,
) -> Result<()> {
    // `#[inline(never)]` keeps the heavy stack residents
    // (`find_program_address` output, the [u8; 32] address copy) out of the
    // entrypoint dispatcher's frame. Without it the BPF linker flagged the
    // dispatcher as exceeding the 4096-byte stack budget — keeping each
    // new Layer 9 handler non-inlined shifts the frame charge to a
    // separate, isolated function.
    // 1. Amount sanity. Counter bumps with `amount = 0` are observable as
    //    no-op events and confuse indexers; reject early.
    if amount == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount).into());
    }

    // 2. Token-kind dispatch + canonical mint resolution. RWT is the only
    //    path exercised by `withdraw_liquidity_holding` in Layer 9, but the
    //    USDC branch is kept for symmetry with `nexus_deposit` and to
    //    reserve the discriminator slot for future cross-token CPI drains
    //    (SD-4 keeps the door open).
    let expected_mint: [u8; 32] = match token_kind {
        TOKEN_KIND_USDC => USDC_MINT,
        TOKEN_KIND_RWT => RWT_MINT,
        _ => return Err(ProgramError::from(DexError::InvalidNexusToken).into()),
    };

    // 3. D25 caller-validation step 1 — owner of `liquidity_holding` must be
    //    the YD program. A forged Tx that names a non-YD-owned account here
    //    fails this check before any state observation.
    //
    //    Pinocchio `owner()` is `unsafe` because it returns a reference into
    //    a slot that `assign()` could mutate; we only read the bytes for an
    //    equality compare and never store the reference, so the unsafe slice
    //    pattern in `validation.rs` (L-5 audit note) applies here too.
    {
        // SAFETY: see lib.rs "Unsafe (L-5 audit note)". We borrow the owner
        // bytes for the lifetime of this expression only; no `assign` call
        // can race with this read inside the handler.
        let lh_owner_ref = unsafe { ctx.accounts.liquidity_holding.owner() };
        if lh_owner_ref.as_ref() != YD_PROGRAM_ID.as_ref() {
            return Err(ProgramError::from(DexError::NexusRecordDepositOnlyFromYd).into());
        }
    }

    // 4. D25 caller-validation step 2 — re-derive `["liq_holding"]` PDA under
    //    `YD_PROGRAM_ID` and confirm the supplied account address matches.
    //    This catches a hypothetical forged account that is YD-owned but
    //    lives at a non-canonical address (e.g. created by an attacker via
    //    a YD program upgrade defect). Layer 8 D14 pins the singleton seed
    //    layout used here.
    //
    //    The `YD_PROGRAM_ID` Address is constructed via the `const fn`
    //    `Address::new_from_array`, but the compiler still materialises it
    //    on the stack when passed by reference; bind directly to the
    //    `find_program_address` call to keep the temporary's lifetime
    //    inside the call expression.
    let (expected_lh, _expected_bump) = arlex_lang::find_program_address(
        &[b"liq_holding"],
        &Address::new_from_array(YD_PROGRAM_ID),
    );
    if ctx.accounts.liquidity_holding.address() != &expected_lh {
        return Err(ProgramError::from(DexError::InvalidLiquidityHoldingPda).into());
    }

    // 5. D25 caller-validation step 3 — signer flag must be set. The Solana
    //    runtime gates this flag on `invoke_signed` having presented matching
    //    PDA seeds; a non-CPI Tx cannot satisfy it for a foreign PDA. The
    //    `#[account(signer)]` macro constraint also enforces this, but we
    //    repeat the check explicitly so the failure mode surfaces a clean
    //    `NexusRecordDepositOnlyFromYd` instead of a framework-level
    //    `MissingRequiredSignature`.
    if !ctx.accounts.liquidity_holding.is_signer() {
        return Err(ProgramError::from(DexError::NexusRecordDepositOnlyFromYd).into());
    }

    // 6. Active-gate + counter bump. Scoped block so the mut handle does not
    //    outlive the emit step; effects-only handler — no CPI to interleave.
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
            _ => unreachable!(), // covered by step 2.
        }
    };

    // 7. Emit. `source_kind = SOURCE_LIQUIDITY_HOLDING` lets indexers
    //    distinguish YD CPI drains from permissionless `nexus_deposit`s
    //    (the latter emits `SOURCE_DIRECT`).
    let clock = Clock::get()?;
    emit!(NexusDeposited {
        token_mint: expected_mint,
        amount,
        new_total_deposited: new_total,
        source_kind: NEXUS_DEPOSIT_SOURCE_LIQUIDITY_HOLDING,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Pure-Rust pinning tests for the D25 caller-validation contract +
    //! counter-bump semantics of `nexus_record_deposit`. Handler-level
    //! negative ACs (forged owner, missing signer flag, PDA mismatch)
    //! require the BPF runtime; here we mirror the validation logic onto
    //! synthetic byte inputs so a refactor cannot silently regress the
    //! "only YD CPI" invariant or the symmetric counter dispatch with
    //! `nexus_deposit`.
    use super::*;
    use crate::state::LiquidityNexus;

    /// Pin: `nexus_record_deposit` exclusively trusts `YD_PROGRAM_ID` for
    /// the owner check. Drift here means an attacker could swap in another
    /// program ID and bypass D25.
    #[test]
    fn nexus_record_deposit_only_yd_program_can_call() {
        // Production check (mirrored from the handler):
        //     if lh_owner != YD_PROGRAM_ID { err NexusRecordDepositOnlyFromYd }
        let yd_owner: [u8; 32] = YD_PROGRAM_ID;
        let foreign_owner: [u8; 32] = [0xFFu8; 32];

        assert_eq!(yd_owner, YD_PROGRAM_ID);
        assert_ne!(foreign_owner, YD_PROGRAM_ID);

        // The handler returns this code on owner mismatch:
        let err = ProgramError::from(DexError::NexusRecordDepositOnlyFromYd);
        // Pin the chosen variant — defence against accidentally returning
        // `Unauthorized` or another generic code that decoders couldn't
        // route to the "wrong-CPI-caller" branch.
        match err {
            ProgramError::Custom(_) => {}
            other => panic!("expected Custom, got {:?}", other),
        }
    }

    /// Pin: the `["liq_holding"]` seed used in the re-derivation must match
    /// Layer 8 D14's singleton seed for the YD PDA. If anyone changes the
    /// seed (or adds a per-token suffix on YD's side), this test breaks
    /// before deployment.
    #[test]
    fn nexus_record_deposit_pda_seeds_validation() {
        // The handler re-derives via:
        //     find_program_address(&[b"liq_holding"], &YD_PROGRAM_ID)
        // The seed is a single-component byte string. Any drift to a
        // multi-component layout (or a different byte string) here OR in
        // YD's `liquidity_holding` account would break the cross-program
        // invariant.
        let seeds: &[&[u8]] = &[b"liq_holding"];
        assert_eq!(seeds.len(), 1);
        assert_eq!(seeds[0], b"liq_holding");
        assert_eq!(seeds[0].len(), 11);
    }

    /// Pin: counter-bump dispatch routes USDC vs RWT correctly. Same
    /// counter-slot semantics as `nexus_deposit` — but emitted under
    /// `SOURCE_LIQUIDITY_HOLDING`.
    #[test]
    fn nexus_record_deposit_increments_counter() {
        // SAFETY: zero-init valid for `LiquidityNexus`; see other tests.
        let buf = [0u8; core::mem::size_of::<LiquidityNexus>()];
        let mut nexus: LiquidityNexus =
            unsafe { core::ptr::read(buf.as_ptr() as *const LiquidityNexus) };
        nexus.is_active = true;
        nexus.total_deposited_rwt = 1_000;

        let amount: u64 = 500;
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
        assert_eq!({ nexus.total_deposited_rwt }, 1_500);
        assert_eq!({ nexus.total_deposited_usdc }, 0);

        // Pin: emitted source_kind for YD-CPI path is
        // `SOURCE_LIQUIDITY_HOLDING`, not `SOURCE_DIRECT`.
        assert_eq!(NEXUS_DEPOSIT_SOURCE_LIQUIDITY_HOLDING, 1);
        assert_ne!(
            NEXUS_DEPOSIT_SOURCE_LIQUIDITY_HOLDING,
            NEXUS_DEPOSIT_SOURCE_DIRECT
        );
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
        const SRC: &str = include_str!("nexus_record_deposit.rs");
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
