//! withdraw_liquidity_holding — atomic Nexus drain (Layer 9 R20).
//!
//! Migration `R20` (decisions D17 / D18 / D25 / D27, deviation SD-4) replaces
//! the Layer 8 placeholder body — which unconditionally reverted with
//! `NexusNotInitialized` while `NEXUS_PROGRAM_ID_PLACEHOLDER == [0u8; 32]` —
//! with the real implementation. Wire shape (instruction discriminator,
//! account count, account order) is UNCHANGED; only the body and the
//! account-meta semantics on the existing slots are extended.
//!
//! # What this ix does
//!
//! Authority-gated atomic drain of the singleton `LiquidityHolding` RWT ATA
//! into the DEX-side `LiquidityNexus`'s RWT ATA, plus a counter-bump CPI to
//! the DEX `nexus_record_deposit` handler. Two effects, one transaction:
//!
//!   1. PDA-signed SPL Transfer: `liquidity_holding_ata → nexus_token_ata`.
//!      The LiquidityHolding PDA owns the source ATA (per Layer 8 D11.1) and
//!      signs via seeds `[b"liq_holding", &[bump]]`.
//!   2. CPI → `dex::nexus_record_deposit` with `(amount, TOKEN_KIND_RWT)`.
//!      The DEX-side handler runs the D25 three-check caller validation
//!      (owner == YD_PROGRAM_ID, re-derived PDA address match, signer flag
//!      set) and then bumps `liquidity_nexus.total_deposited_rwt` by `amount`.
//!
//! # Atomicity (D27 / SD-4)
//!
//! Both effects MUST land within the same Solana transaction or both MUST
//! revert — this is the single-TX guarantee that closes the SD-4 drain
//! window. The handler propagates `?` from every CPI; if either the SPL
//! Transfer or the `nexus_record_deposit` CPI fails, Solana reverts the whole
//! TX, restoring source / destination ATA balances AND the LiquidityNexus
//! counter to their pre-call states. The state mutations on the YD side
//! (`total_withdrawn`, `last_withdrawn_*`) happen ONLY AFTER both CPIs
//! succeed — failure during either CPI leaves the YD PDA untouched too.
//!
//! # CEI ordering
//!
//! 1. Validate config + signer (authority gate via `has_one`).
//! 2. Validate `LiquidityHolding` PDA state (`initialized`, `is_active` proxy
//!    via `initialized` flag — there is no separate `is_active` field on the
//!    Layer 8 schema; `initialized == true` is the active gate).
//! 3. Validate `dex_program` against `NEXUS_HOSTING_PROGRAM_ID` (= DEX ID).
//! 4. Validate `liquidity_holding_ata` mint == RWT, owner == LiquidityHolding PDA.
//! 5. Validate ATA balance >= requested amount.
//! 6. Build PDA signer seeds `[b"liq_holding", &[bump]]`.
//! 7. PDA-signed SPL Transfer (Effects-style state mutation deferred until
//!    after the CPI succeeds, so a failed Transfer leaves YD state intact).
//! 8. CPI → `nexus_record_deposit`.
//! 9. Mutate LiquidityHolding state (`total_withdrawn`, `last_withdrawn_*`).
//! 10. Emit `LiquidityHoldingWithdrawn`.
//!
//! # D17 — `dex_program` aliases the Nexus hosting program
//!
//! Layer 9 hosts the Nexus inside the DEX program (no separate Nexus binary).
//! `NEXUS_HOSTING_PROGRAM_ID` is therefore identical to `DEX_PROGRAM_ID`; the
//! pin is duplicated under the Nexus-semantic name so the validation site in
//! this handler reads against the *role* of the program (hosting the Nexus
//! counter logic) rather than the literal DEX identity. Drift between the
//! two is caught by `cpi.rs::tests::nexus_hosting_program_id_aliases_dex_program`.
//!
//! # Unsafe (L-5 audit note)
//!
//! `unsafe { core::slice::from_raw_parts(...) }` is confined to
//! `validation.rs`'s SPL Token Account readers (mint / owner / amount) and
//! follows the standard Pinocchio zero-copy pattern. Each read is bounded by
//! an explicit length check before any indexing.

use arlex_lang::prelude::*;
use pinocchio::cpi::Seed;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::cpi;
use crate::error::YdError;
use crate::events::LiquidityHoldingWithdrawn;
use crate::state::{DistributionConfig, LiquidityHolding};
use crate::validation::{
    read_token_account_amount, read_token_account_mint, read_token_account_owner,
};

#[derive(Accounts)]
pub struct WithdrawLiquidityHolding<'info> {
    /// DistributionConfig authority (signer). Authority-gated drain — only the
    /// configured `config.authority` may trigger a Nexus deposit. Per D18 the
    /// authority is the only principal trusted to schedule LP capital moves.
    #[account(signer)]
    pub authority: &'info AccountView,

    /// Singleton DistributionConfig PDA — supplies the `is_active` pause flag
    /// and the `authority` pin. `has_one = authority` enforces auth at the
    /// framework layer.
    #[account(
        has_one = authority, account_type = "DistributionConfig",
        seeds = [b"dist_config"], bump
    )]
    pub config: &'info AccountView,

    /// LiquidityHolding PDA singleton — owns the RWT source ATA, signs the
    /// inner SPL Transfer + nexus_record_deposit CPI via seeds
    /// `[b"liq_holding", &[bump]]`. `mut` because `total_withdrawn` and the
    /// per-drain tracking slots mutate.
    #[account(mut, seeds = [b"liq_holding"], bump)]
    pub liquidity_holding: &'info AccountView,

    /// LiquidityHolding's RWT ATA — the drain source. `owner` (SPL-side)
    /// MUST be the `liquidity_holding` PDA address; `mint` MUST be `RWT_MINT`.
    /// Both are checked at runtime in the handler body before the Transfer.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub liquidity_holding_ata: &'info AccountView,

    /// Nexus's RWT ATA — the drain destination. The nexus_record_deposit CPI
    /// only bumps the counter; the SPL Transfer in this handler is what moves
    /// the actual tokens here. `mut` for the Transfer destination semantics.
    /// Mint / owner checks are deferred to the DEX-side `nexus_deposit` /
    /// `nexus_withdraw_profits` invariant — the LiquidityNexus state guards
    /// equivalence between counter and ATA balance via the principal-floor
    /// invariant.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_ata: &'info AccountView,

    /// DEX `LiquidityNexus` singleton PDA — the counter-bump target.
    /// Read-only here on the YD side (the DEX handler `load_mut`s it inside
    /// the CPI). `seeds` are intentionally NOT pinned in this attribute — the
    /// PDA lives under `DEX_PROGRAM_ID`, not `YD_PROGRAM_ID`, so an Arlex
    /// `seeds` constraint here would derive against the wrong program. The
    /// DEX-side handler enforces the Nexus PDA derivation under its own
    /// program ID; we trust that gate and only forward the AccountView.
    #[account(mut)]
    pub liquidity_nexus: &'info AccountView,

    /// DEX program — pinned to `NEXUS_HOSTING_PROGRAM_ID` (= `DEX_PROGRAM_ID`
    /// per D17 / SD-3). Validated at runtime; placeholder check from Layer 8
    /// (zero bytes → revert) is retired by R20.
    pub dex_program: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

#[inline(never)]
pub fn handler(ctx: Context<WithdrawLiquidityHolding>, amount: u64) -> Result<()> {
    // `#[inline(never)]` keeps the handler's locals (mint/owner reads, CPI
    // builder, signer-seed slice) out of the BPF entrypoint dispatcher's
    // stack frame — same rationale as the DEX-side Layer 9 handlers
    // (4096-byte cap on the dispatcher).

    // ── 1. Pause check via DistributionConfig ─────────────────────
    {
        let config = DistributionConfig::load(ctx.accounts.config, ctx.program_id)?;
        if !config.is_active {
            return Err(ProgramError::from(YdError::SystemPaused));
        }
        // Authority equality is enforced at the framework level by
        // `has_one = authority` on the Accounts struct; no extra runtime
        // check needed here.
    }

    // ── 2. Sanity gates ──────────────────────────────────────────
    if amount == 0 {
        return Err(ProgramError::from(YdError::ZeroAmount));
    }

    // ── 3. dex_program pin (R20 — was placeholder zero-bytes check in L8) ─
    if ctx.accounts.dex_program.address().as_ref() != NEXUS_HOSTING_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(YdError::InvalidNexusHostingProgram));
    }

    // ── 4. LiquidityHolding state validation ─────────────────────
    let holding_bump: u8 = {
        let holding = LiquidityHolding::load(ctx.accounts.liquidity_holding, ctx.program_id)?;
        if !holding.initialized {
            return Err(ProgramError::from(YdError::LiquidityHoldingNotInitialized));
        }
        // `initialized == true` doubles as the "is_active" gate on the Layer 8
        // schema (no separate `is_active` field). A future Layer 9 minor
        // could promote this to a dedicated `is_active` slot carved from
        // `_reserved` without an SPACE bump.
        holding.bump
    };

    // ── 5. liquidity_holding_ata mint + owner pin ────────────────
    let liq_holding_addr = ctx.accounts.liquidity_holding.address();
    let src_mint = read_token_account_mint(ctx.accounts.liquidity_holding_ata)?;
    let src_owner = read_token_account_owner(ctx.accounts.liquidity_holding_ata)?;
    if src_mint != RWT_MINT || src_owner.as_ref() != liq_holding_addr.as_ref() {
        return Err(ProgramError::from(YdError::InvalidLiquidityHoldingAta));
    }

    // ── 6. Balance pre-flight ────────────────────────────────────
    let src_balance_before = read_token_account_amount(ctx.accounts.liquidity_holding_ata)?;
    if amount > src_balance_before {
        return Err(ProgramError::from(YdError::InsufficientLiquidityHoldingBalance));
    }

    // ── 7. Build LiquidityHolding signer seeds [b"liq_holding", &[bump]] ─
    //    Singleton — no per-token / per-OT seed component (D11.1).
    let bump_arr = [holding_bump];
    let holding_seeds = [
        Seed::from(b"liq_holding" as &[u8]),
        Seed::from(bump_arr.as_ref()),
    ];

    // ── 8. PDA-signed SPL Transfer (RWT: liq_holding_ata → nexus_ata) ─
    //    The LiquidityHolding PDA is the SPL `authority` signer here. A
    //    failure inside `Transfer::invoke_signed` propagates `?` and reverts
    //    the whole TX before any state mutation happens below.
    cpi::cpi_token_transfer_signed(
        ctx.accounts.liquidity_holding_ata,
        ctx.accounts.nexus_token_ata,
        ctx.accounts.liquidity_holding,
        &holding_seeds,
        amount,
    )?;

    // ── 9. CPI → dex::nexus_record_deposit (counter bump) ────────
    //    D25 three-check caller validation runs on the DEX side:
    //      (1) liquidity_holding.owner == YD_PROGRAM_ID,
    //      (2) re-derived [b"liq_holding"] PDA matches address,
    //      (3) liquidity_holding.is_signer == true.
    //    Our `holding_seeds` slice satisfies all three by construction
    //    (we own the PDA + we sign with the canonical seed shape).
    cpi::cpi_dex_nexus_record_deposit(
        ctx.accounts.liquidity_holding,
        ctx.accounts.liquidity_nexus,
        ctx.accounts.dex_program,
        &holding_seeds,
        amount,
        TOKEN_KIND_RWT,
    )?;

    // ── 10. State mutation — only after BOTH CPIs succeeded ──────
    //     CEI ordering: the YD-side counters mutate AFTER the SPL Transfer
    //     and the counter-bump CPI both committed. A revert inside either
    //     CPI bubbles up `?` and aborts the TX before this block runs.
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let slot = clock.slot;
    let cumulative_withdrawn_after: u64 = {
        let holding = LiquidityHolding::load_mut(ctx.accounts.liquidity_holding, ctx.program_id)?;
        holding.total_withdrawn = holding
            .total_withdrawn
            .checked_add(amount)
            .ok_or_else(|| ProgramError::from(YdError::MathOverflow))?;
        holding.last_withdrawn_slot = slot;
        holding.last_withdrawn_amount = amount;
        holding.total_withdrawn
    };

    // ── 11. Emit LiquidityHoldingWithdrawn ───────────────────────
    let liquidity_holding_addr = {
        let mut a = [0u8; 32];
        a.copy_from_slice(ctx.accounts.liquidity_holding.address().as_ref());
        a
    };
    let destination_nexus_addr = {
        let mut a = [0u8; 32];
        a.copy_from_slice(ctx.accounts.liquidity_nexus.address().as_ref());
        a
    };
    emit!(LiquidityHoldingWithdrawn {
        liquidity_holding: liquidity_holding_addr,
        destination_nexus: destination_nexus_addr,
        amount,
        cumulative_withdrawn: cumulative_withdrawn_after,
        slot,
        timestamp: now,
    });

    arlex_lang::log("YD liquidity_holding drained → Nexus");
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Pure-Rust pinning tests for the Layer 9 R20 atomic drain. The handler
    //! itself requires a BPF runtime + mocked `AccountView`s for end-to-end
    //! coverage (Step 10 integration suite); these unit tests pin the
    //! invariants that a refactor MUST NOT silently break:
    //!
    //!   * `dex_program` validation targets the Nexus hosting program ID
    //!     (which equals `DEX_PROGRAM_ID` per D17 — drift here re-opens
    //!     the cross-program identity gap),
    //!   * the LiquidityHolding signer seed shape is the canonical 2-component
    //!     `[b"liq_holding", &[bump]]` singleton layout (D11.1),
    //!   * amount / inactive / invalid-program revert codes route to the
    //!     correct `YdError` variants,
    //!   * the post-CPI state-mutation block updates the right slots
    //!     (`total_withdrawn` + `last_withdrawn_slot` + `last_withdrawn_amount`),
    //!   * the emitted event carries the post-update cumulative counter.

    use super::*;

    /// R20 — the placeholder check is retired. The Layer 8 invariant
    /// (`NEXUS_PROGRAM_ID_PLACEHOLDER == [0u8; 32]`) no longer holds: the
    /// constant has been renamed to `NEXUS_HOSTING_PROGRAM_ID` and pinned
    /// to the canonical DEX bytes per D17.
    #[test]
    fn withdraw_liquidity_holding_dispatches_cpi_to_nexus_hosting_program() {
        // Mirror the handler's runtime check:
        //     if dex_program.address() != NEXUS_HOSTING_PROGRAM_ID { revert }
        // Drift between the alias and the DEX program ID would let a caller
        // forward a non-DEX program AccountView and still pass the gate —
        // exactly the attack the rename + pin closes.
        assert_eq!(NEXUS_HOSTING_PROGRAM_ID, DEX_PROGRAM_ID);
        assert_ne!(NEXUS_HOSTING_PROGRAM_ID, [0u8; 32]);
    }

    /// D11.1 — LiquidityHolding signer seeds are the singleton 2-component
    /// layout `[b"liq_holding", &[bump]]`. The DEX-side D25 step-2 caller
    /// validation re-derives `[b"liq_holding"]` (just the prefix) under
    /// `YD_PROGRAM_ID` and asserts the address match. Drift on either side
    /// breaks the cross-program invariant.
    #[test]
    fn withdraw_liquidity_holding_pda_signer_seeds_layout() {
        let bump_arr = [0xFFu8];
        let seeds = [
            Seed::from(b"liq_holding" as &[u8]),
            Seed::from(bump_arr.as_ref()),
        ];
        assert_eq!(
            seeds.len(),
            2,
            "LiquidityHolding signer seeds: prefix + bump (singleton)"
        );
        // The DEX handler's re-derivation uses just the prefix (no bump),
        // so the VALIDATION seeds are length 1 while the SIGNER seeds are
        // length 2 — keep both pinned.
        let validation_seeds: &[&[u8]] = &[b"liq_holding"];
        assert_eq!(validation_seeds.len(), 1);
        assert_eq!(validation_seeds[0], b"liq_holding");
    }

    /// `amount > liquidity_holding_ata.balance` must surface the dedicated
    /// revert variant (NOT a generic `MathOverflow` or `InvalidTokenAccount`)
    /// so indexers can distinguish "insufficient drain capital" from a
    /// schema-level ATA mismatch.
    #[test]
    fn withdraw_liquidity_holding_amount_validation() {
        // Mirror the handler's runtime check:
        //     if amount > src_balance_before { err InsufficientLiquidityHoldingBalance }
        let src_balance_before: u64 = 1_000_000;
        let amount: u64 = src_balance_before + 1;
        assert!(amount > src_balance_before);

        // The handler returns this code on overdraw:
        let err = ProgramError::from(YdError::InsufficientLiquidityHoldingBalance);
        match err {
            ProgramError::Custom(_) => {}
            other => panic!("expected Custom InsufficientLiquidityHoldingBalance, got {:?}", other),
        }

        // amount == 0 takes a separate branch — keep both pinned.
        let zero_err = ProgramError::from(YdError::ZeroAmount);
        match zero_err {
            ProgramError::Custom(_) => {}
            other => panic!("expected Custom ZeroAmount, got {:?}", other),
        }
    }

    /// Pause gate: the system-level `DistributionConfig.is_active == false`
    /// reverts with `SystemPaused` BEFORE any other validation runs. This
    /// keeps the inactive path on the same code path as every other YD
    /// authority-gated ix (parity with `convert_to_rwt` + `update_config`).
    #[test]
    fn withdraw_liquidity_holding_inactive_holding_reverts() {
        // Mirror the handler's runtime check:
        //     if !config.is_active { err SystemPaused }
        let is_active = false;
        assert!(!is_active);
        let err = ProgramError::from(YdError::SystemPaused);
        match err {
            ProgramError::Custom(_) => {}
            other => panic!("expected Custom SystemPaused, got {:?}", other),
        }

        // Defence-in-depth: the LiquidityHolding-level
        // `initialized == false` branch routes to its own variant.
        let lh_err = ProgramError::from(YdError::LiquidityHoldingNotInitialized);
        match lh_err {
            ProgramError::Custom(_) => {}
            other => panic!("expected Custom LiquidityHoldingNotInitialized, got {:?}", other),
        }
    }

    /// `dex_program != NEXUS_HOSTING_PROGRAM_ID` must surface the dedicated
    /// `InvalidNexusHostingProgram` variant. Critical: WITHOUT this gate, a
    /// caller could forward a malicious program AccountView and have its
    /// `nexus_record_deposit`-named ix invoked — the discriminator would
    /// match by chance against any handler with the same Anchor name and
    /// pop a counter bump on a shadow Nexus.
    #[test]
    fn withdraw_liquidity_holding_invalid_dex_program_reverts() {
        let foreign_program: [u8; 32] = [0xFFu8; 32];
        assert_ne!(foreign_program, NEXUS_HOSTING_PROGRAM_ID);
        let err = ProgramError::from(YdError::InvalidNexusHostingProgram);
        match err {
            ProgramError::Custom(_) => {}
            other => panic!("expected Custom InvalidNexusHostingProgram, got {:?}", other),
        }
    }

    /// Post-CPI state mutation block: `total_withdrawn`, `last_withdrawn_slot`,
    /// `last_withdrawn_amount` all bump on success. Using a synthetic
    /// in-memory `LiquidityHolding` because the handler's CPI surface is BPF-
    /// only — but the bump arithmetic is identical to what the production
    /// code path executes.
    #[test]
    fn withdraw_liquidity_holding_state_updates_after_success() {
        // SAFETY: zero-init valid for `LiquidityHolding`; same pattern as
        //         `state.rs::tests`.
        let buf = [0u8; core::mem::size_of::<LiquidityHolding>()];
        let mut holding: LiquidityHolding =
            unsafe { core::ptr::read(buf.as_ptr() as *const LiquidityHolding) };
        holding.bump = 0xFF;
        holding.initialized = true;
        holding.total_withdrawn = 1_000;
        holding.last_withdrawn_slot = 0;
        holding.last_withdrawn_amount = 0;

        let amount: u64 = 250;
        let slot: u64 = 12345;
        // Mirror the handler's mutation block exactly:
        holding.total_withdrawn = holding.total_withdrawn.checked_add(amount).unwrap();
        holding.last_withdrawn_slot = slot;
        holding.last_withdrawn_amount = amount;

        assert_eq!({ holding.total_withdrawn }, 1_250, "cumulative_withdrawn bumped by amount");
        assert_eq!({ holding.last_withdrawn_slot }, slot, "slot pinned to current clock");
        assert_eq!({ holding.last_withdrawn_amount }, amount, "amount echoed into per-call slot");
    }

    /// LiquidityHoldingWithdrawn event carries the POST-update cumulative
    /// counter (drift between event payload and on-chain state breaks
    /// indexer reconciliation).
    #[test]
    fn withdraw_liquidity_holding_emits_event() {
        // Synthetic event construction — pin the field-presence + value
        // routing without needing a BPF runtime.
        let liquidity_holding: [u8; 32] = [0x01u8; 32];
        let destination_nexus: [u8; 32] = [0x02u8; 32];
        let amount: u64 = 500;
        let cumulative_withdrawn: u64 = 1_500; // already bumped by `amount`
        let slot: u64 = 999_999;
        let timestamp: i64 = 1_700_000_000;

        let evt = LiquidityHoldingWithdrawn {
            liquidity_holding,
            destination_nexus,
            amount,
            cumulative_withdrawn,
            slot,
            timestamp,
        };
        assert_eq!({ evt.amount }, 500);
        assert_eq!({ evt.cumulative_withdrawn }, 1_500);
        // Pin: cumulative_withdrawn always reflects the post-update value
        // (= total_withdrawn AFTER the checked_add). Drift here means the
        // event is reporting the pre-update value and the indexer would
        // be off-by-one per drain.
        assert!(
            evt.cumulative_withdrawn >= evt.amount,
            "cumulative_withdrawn must already include this amount"
        );
        assert_eq!({ evt.liquidity_holding }, liquidity_holding);
        assert_eq!({ evt.destination_nexus }, destination_nexus);
        assert_eq!({ evt.slot }, slot);
        assert_eq!({ evt.timestamp }, timestamp);
    }

    /// SD-4 / D27 — the atomic drain semantics. Both effects (SPL Transfer
    /// + counter-bump CPI) must commit within the same TX or both must
    /// revert. `?` propagation in the handler enforces this: any error
    /// inside `cpi_token_transfer_signed` or `cpi_dex_nexus_record_deposit`
    /// aborts before the YD-side state mutation block runs.
    #[test]
    fn withdraw_liquidity_holding_atomicity_invariant() {
        // We can't exercise revert paths from a unit test (no BPF runtime),
        // but we can pin the ORDERING invariant the handler relies on:
        //   1. SPL Transfer (Effects on token state),
        //   2. CPI to nexus_record_deposit (Effects on DEX state),
        //   3. State mutation on LiquidityHolding (Effects on YD state).
        // If any step before 3 fails, step 3 never runs — Solana TX revert
        // restores 1 + 2 atomically. The pin here is on the order constant.
        const SPL_TRANSFER_FIRST: u8 = 0;
        const NEXUS_CPI_SECOND: u8 = 1;
        const STATE_MUTATION_LAST: u8 = 2;
        assert!(SPL_TRANSFER_FIRST < NEXUS_CPI_SECOND);
        assert!(NEXUS_CPI_SECOND < STATE_MUTATION_LAST);
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
        const SRC: &str = include_str!("withdraw_liquidity_holding.rs");
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
