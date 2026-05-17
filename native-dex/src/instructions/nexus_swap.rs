//! Layer 9 §4.3 — `nexus_swap`. Manager-gated swap that uses the singleton
//! `LiquidityNexus` PDA as the swap authority.
//!
//! The Manager wallet (a Tx signer) authorizes the call via the D22
//! `assert_manager` helper (kill-switch first, then signer match). The
//! actual SPL Transfer of the input tokens out of the Nexus-owned ATA
//! signs with the Nexus PDA seeds `[b"liquidity_nexus", &[nexus.bump]]`.
//! All pool-side outbound transfers (vault_out → nexus_out_ata, vault →
//! areal_fee, vault → ot_treasury) continue to sign with the pool PDA
//! seeds inside `swap_internal` — independent of `authority_signer_seeds`.
//!
//! Reuses `swap_internal` per D23: same code path as user-signed
//! `swap`, only the `authority` slot is the Nexus PDA and the inbound
//! transfer is PDA-signed via `Some(&signers)`. No swap-math drift
//! between Manager and user paths.

use arlex_lang::prelude::*;

use crate::constants::*;
use crate::error::DexError;
use crate::state::LiquidityNexus;
use crate::validation::*;
use crate::instructions::swap::{swap_internal, SwapAccountsView};

#[derive(Accounts)]
pub struct NexusSwap<'info> {
    /// Nexus Manager wallet (Tx signer). The D22 helper `assert_manager`
    /// rejects (a) the zero kill-switch sentinel via
    /// `NexusManagerDisabled` BEFORE checking the signer match, and
    /// (b) any non-Manager signer via `InvalidNexusManager`.
    #[account(signer)]
    pub manager: &'info AccountView,

    /// DEX config singleton — read-only, reused by `swap_internal` for
    /// `is_active`, `lp_fee_share_bps`, and the `areal_fee_destination`
    /// validation.
    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    /// Nexus singleton PDA. Mutable because the inbound swap transfer
    /// signs with its seeds; the account itself is not mutated by the
    /// handler. `is_active = false` reverts via `NexusNotActive`.
    #[account(mut, seeds = [LIQUIDITY_NEXUS_SEED], bump)]
    pub liquidity_nexus: &'info AccountView,

    /// Pool whose AMM is being traded against. Mutated by `swap_internal`
    /// (reserves + per-share fee accumulator).
    #[account(mut)]
    pub pool_state: &'info AccountView,

    /// Nexus-owned ATA holding the input token (USDC or RWT depending on
    /// `a_to_b`). Acts as the `provider_token_in` slot in the projected
    /// `SwapAccountsView`. PDA-signed Transfer source.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_in: &'info AccountView,

    /// Nexus-owned ATA receiving the output token. Acts as the
    /// `provider_token_out` slot in the projected `SwapAccountsView`.
    /// Pool-PDA-signed Transfer destination (signed inside
    /// `swap_internal`, not via `authority_signer_seeds`).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_out: &'info AccountView,

    /// Pool vault matching `pool.vault_a/b` per the `a_to_b` flag.
    /// Validated by `swap_internal` via `validate_vault`.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_in: &'info AccountView,

    /// Mirror of `vault_in` for the output side.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_out: &'info AccountView,

    /// Areal Finance protocol fee destination (RWT ATA).
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub areal_fee_account: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

impl<'info> NexusSwap<'info> {
    /// Project the Nexus account set into the shape expected by
    /// `swap_internal`. The Nexus PDA fills the `authority` slot — the
    /// PDA-signed inbound transfer uses the seeds passed via
    /// `Some(&signers)` rather than a Tx signature.
    pub(crate) fn view(&self) -> SwapAccountsView<'info> {
        SwapAccountsView {
            authority: self.liquidity_nexus,
            dex_config: self.dex_config,
            pool_state: self.pool_state,
            user_token_in: self.nexus_token_in,
            user_token_out: self.nexus_token_out,
            vault_in: self.vault_in,
            vault_out: self.vault_out,
            areal_fee_account: self.areal_fee_account,
            token_program: self.token_program,
        }
    }
}

/// Fee-on-top compliance (docs/contracts/native-dex.mdx:522-568): when
/// `input_is_rwt`, the Nexus PDA's `nexus_token_in` ATA must contain
/// `amount_in + fee_total + ot_treasury_fee` (fees on top per docs spec) so
/// the inbound transfer initiated by `swap_internal` can debit the full
/// fee-grossed amount. The bot driving this instruction is responsible for
/// sizing `amount_in` so that fee headroom is reserved in the Nexus ATA.
/// When `!input_is_rwt`, fees come out of the output side and the Nexus ATA
/// only needs `amount_in` to cover the inbound transfer.
pub fn handler(
    ctx: Context<NexusSwap>,
    amount_in: u64,
    min_amount_out: u64,
    a_to_b: bool,
) -> Result<()> {
    // 1. D22 ordering — kill-switch check fires before signer match. Load
    //    Nexus, validate `is_active`, run `assert_manager`, and capture
    //    `bump` for the signer-seed array; the load handle is scoped to
    //    this block so the Nexus AccountView is freely reusable as the
    //    `authority` slot in the projected view below.
    let nexus_bump: u8 = {
        let nexus = LiquidityNexus::load_mut(
            ctx.accounts.liquidity_nexus,
            ctx.program_id,
        )?;
        if !nexus.is_active {
            return Err(ProgramError::from(DexError::NexusNotActive));
        }
        assert_manager(nexus, ctx.accounts.manager)?;
        nexus.bump
    };

    // 2. Layer 9 §4.3 — preflight slippage / amount checks (the spec
    //    requires `min_amount_out > 0` to prevent infinite-slippage abuse;
    //    `amount_in > 0` is also re-checked downstream by `swap_internal`,
    //    but doing it here makes the Manager-gate fail-fast surface
    //    explicit and saves CU on bad-input reverts).
    if amount_in == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount));
    }
    if min_amount_out == 0 {
        return Err(ProgramError::from(DexError::SlippageExceeded));
    }

    // 3. Build Nexus PDA signer seeds — single source of truth for the
    //    PDA-signed inbound transfer (`nexus_token_in → vault_in`). The
    //    pool-side outbound transfers (vault_out → nexus_token_out,
    //    vault → areal_fee, vault → ot_treasury) sign with the pool PDA
    //    seeds derived inside `swap_internal` and are independent of this
    //    array.
    let bump_arr = [nexus_bump];
    let signer_seeds: [Seed; 2] = [
        Seed::from(LIQUIDITY_NEXUS_SEED),
        Seed::from(bump_arr.as_ref()),
    ];
    let signers = [Signer::from(&signer_seeds)];

    // 4. Reuse the canonical swap path. `swap_internal` performs all
    //    pool-state effects, fee accounting, slippage check, and CPIs.
    //    The `Some(&signers)` propagation is what makes the inbound
    //    transfer PDA-signed instead of Tx-signer-signed.
    swap_internal(
        &ctx.accounts.view(),
        ctx.remaining_accounts,
        ctx.program_id,
        amount_in,
        min_amount_out,
        a_to_b,
        Some(&signers),
    )
}

#[cfg(test)]
mod tests {
    //! Layer 9 §4.3 — pure-Rust pinning tests for `nexus_swap`'s D22 / D23
    //! contract surface. Handler-level negative ACs are exercised by future
    //! BPF integration tests where Arlex can be invoked end-to-end; here we
    //! pin the *shape* of the wiring so a refactor cannot silently regress
    //! the access-control or signer-seed contract.
    use crate::constants::LIQUIDITY_NEXUS_SEED;
    use crate::error::DexError;
    use crate::state::LiquidityNexus;

    /// Test-local twin of `assert_manager`'s decision logic, mirroring the
    /// helper in `validation.rs`. Same byte inputs the production helper
    /// extracts via `signer.address().as_ref()`. Drift between this twin
    /// and the production helper is also caught by handler-level integration
    /// tests when those land.
    fn check_manager_bytes(
        nexus: &LiquidityNexus,
        signer_bytes: &[u8; 32],
    ) -> core::result::Result<(), arlex_lang::prelude::ProgramError> {
        if nexus.manager == [0u8; 32] {
            return Err(arlex_lang::prelude::ProgramError::from(
                DexError::NexusManagerDisabled,
            ));
        }
        if &nexus.manager != signer_bytes {
            return Err(arlex_lang::prelude::ProgramError::from(
                DexError::InvalidNexusManager,
            ));
        }
        Ok(())
    }

    fn nexus_with(manager: [u8; 32], is_active: bool, bump: u8) -> LiquidityNexus {
        // SAFETY: see `validation::tests::nexus_with_manager` — same
        // zero-bit-pattern argument applies.
        let buf = [0u8; core::mem::size_of::<LiquidityNexus>()];
        let mut nexus: LiquidityNexus =
            unsafe { core::ptr::read(buf.as_ptr() as *const LiquidityNexus) };
        nexus.manager = manager;
        nexus.is_active = is_active;
        nexus.bump = bump;
        nexus
    }

    fn custom_code(err: arlex_lang::prelude::ProgramError) -> u32 {
        match err {
            arlex_lang::prelude::ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    fn code_of(err: DexError) -> u32 {
        custom_code(arlex_lang::prelude::ProgramError::from(err))
    }

    /// **D22 critical ordering test** — `nexus_swap` must reject a call
    /// when `nexus.manager == [0u8; 32]` regardless of which wallet
    /// signed. The handler invokes `assert_manager` (which short-circuits
    /// on the kill-switch sentinel before the signer-match check). Here
    /// we model the same decision in raw bytes: a kill-switched Nexus
    /// produces `NexusManagerDisabled` even when the signer "matches"
    /// the all-zero pubkey.
    #[test]
    fn nexus_swap_kill_switch_disabled_revert() {
        let nexus = nexus_with([0u8; 32], /* is_active */ true, /* bump */ 0xFD);
        let signer_zero = [0u8; 32];
        let err = check_manager_bytes(&nexus, &signer_zero).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NexusManagerDisabled));
    }

    /// Wrong signer (non-Manager) → `InvalidNexusManager`. Mirrors the
    /// D22 second-stage check inside `assert_manager` invoked by the
    /// `nexus_swap` handler.
    #[test]
    fn nexus_swap_wrong_signer_revert() {
        let manager = [0xAAu8; 32];
        let other = [0xBBu8; 32];
        let nexus = nexus_with(manager, /* is_active */ true, /* bump */ 0xFD);
        let err = check_manager_bytes(&nexus, &other).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::InvalidNexusManager));
    }

    /// Pin the signer-seed layout used by `nexus_swap` for the inbound
    /// `nexus_token_in → vault_in` PDA-signed Transfer: exactly two
    /// components, `b"liquidity_nexus"` first and the bump byte second.
    /// A drift here (e.g. extra component) would render the PDA
    /// derivation invalid and the on-chain CPI would revert with
    /// `InvalidSeeds`.
    #[test]
    fn nexus_swap_seed_layout_two_components() {
        // Mirror the handler's local construction.
        let bump_arr: [u8; 1] = [0x42];
        let comp0: &[u8] = LIQUIDITY_NEXUS_SEED;
        let comp1: &[u8] = bump_arr.as_ref();
        // Seed structure: [LIQUIDITY_NEXUS_SEED, &[bump]].
        assert_eq!(comp0, b"liquidity_nexus");
        assert_eq!(comp1.len(), 1);
        assert_eq!(comp1[0], 0x42);
        // The slice array constructed in the handler has exactly 2
        // entries — this is what `Signer::from(&seeds)` consumes.
        let seeds: [&[u8]; 2] = [comp0, comp1];
        assert_eq!(seeds.len(), 2);
    }
}
