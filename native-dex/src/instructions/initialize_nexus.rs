//! Layer 9 §4.1 — `initialize_nexus`. One-shot bootstrap for the Nexus
//! singleton PDA. Authority-gated (`dex_config.authority`); double-init
//! is prevented by the Arlex `init` constraint, which fails if the PDA
//! already holds the account discriminator.
//!
//! Sets the initial Manager wallet, zeros the principal counters, flips
//! `is_active = true`, and emits `NexusInitialized`. Layer 9 ix that act
//! on the Nexus (`nexus_swap`, `nexus_add_liquidity`, `nexus_remove_liquidity`,
//! `nexus_withdraw_profits`, `nexus_claim_rewards`, `nexus_record_deposit`)
//! all assume `is_active == true`; this handler is the only path that
//! flips the flag.
//!
//! Note on D22 / kill-switch interaction: this handler does NOT reject
//! `manager == [0u8; 32]`. The plan permits initialising the Nexus directly
//! into the disabled state — operationally equivalent to "init then
//! immediately kill-switch via `update_nexus_manager`", and the kill-switch
//! guard (`assert_manager`) reverts every Manager-gated ix with
//! `NexusManagerDisabled` regardless. Authority always retains the ability
//! to rotate the manager away from zero via `update_nexus_manager`. We
//! intentionally avoid adding a one-off "non-zero on init" check to keep
//! the kill-switch / manager-set flow uniform with `update_nexus_manager`.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::events::NexusInitialized;
use crate::state::{DexConfig, LiquidityNexus};

#[derive(Accounts)]
pub struct InitializeNexus<'info> {
    /// DEX authority (`dex_config.authority`). Mut because pays rent on the
    /// freshly created `liquidity_nexus` PDA. `has_one = authority` on
    /// `dex_config` enforces the access-control gate.
    #[account(mut, signer)]
    pub authority: &'info AccountView,

    /// Singleton DEX config PDA. Read-only here; `has_one` on `authority`
    /// is the Layer 9 §4.1 access-control rule.
    #[account(
        has_one = authority, account_type = "DexConfig",
        seeds = [b"dex_config"], bump
    )]
    pub dex_config: &'info AccountView,

    /// New Nexus singleton PDA. `init` ensures one-time creation —
    /// Arlex's framework reverts with `AccountAlreadyInitialized` (or
    /// equivalent) if the account already carries the discriminator.
    #[account(
        init, payer = authority, space = LiquidityNexus::SPACE,
        seeds = [LIQUIDITY_NEXUS_SEED], bump
    )]
    pub liquidity_nexus: &'info AccountView,

    /// SystemProgram for the rent-paying CPI inside `init`.
    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<InitializeNexus>,
    manager: [u8; 32],
) -> Result<()> {
    // Recompute the Nexus PDA bump locally — Arlex's `init` validates the
    // PDA derivation via the `seeds = [...]` attribute, but the runtime
    // bump value is needed in the persisted `LiquidityNexus.bump` slot so
    // downstream Manager-gated ix can rebuild signer-seeds without a
    // dynamic `find_program_address` call (R23 / D23 — signer-seeds
    // reuse).
    let (_, nexus_bump) = arlex_lang::find_program_address(
        &[LIQUIDITY_NEXUS_SEED],
        ctx.program_id,
    );

    // Initialise the Nexus singleton. `init` writes the discriminator and
    // returns a typed mutable reference; we then populate the data slots.
    let nexus = LiquidityNexus::init(ctx.accounts.liquidity_nexus, ctx.program_id)?;
    nexus.manager = manager;
    nexus.total_deposited_usdc = 0;
    nexus.total_deposited_rwt = 0;
    nexus.is_active = true;
    nexus.bump = nexus_bump;

    let clock = Clock::get()?;
    emit!(NexusInitialized {
        manager,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Nexus initialized");
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Pure-Rust assertions covering the layout invariants of
    //! `initialize_nexus`. Handler-level negative ACs (double-init revert,
    //! wrong-authority revert) are exercised by future BPF integration
    //! tests where the Arlex framework can be invoked end-to-end.
    //!
    //! What we CAN check here without a BPF runtime:
    //!  - the `LiquidityNexus` post-state shape after we mirror the
    //!    handler's struct writes onto an in-memory buffer;
    //!  - that the seed used by the `#[account(init, seeds = ...)]` attribute
    //!    matches the canonical `LIQUIDITY_NEXUS_SEED` constant (catches
    //!    drift from the singleton seed convention).

    use super::*;
    use crate::state::LiquidityNexus;

    /// Reproduce the handler's writes onto a zero-init `LiquidityNexus`
    /// and assert each field lands as documented in Layer 9 §4.1. The
    /// handler can't be invoked directly without a BPF context (it borrows
    /// account data through `LiquidityNexus::init`), so we model only the
    /// data-write side — the access-control + PDA-init side is the
    /// framework's contract and is tested at the integration layer.
    #[test]
    fn initialize_nexus_post_state_matches_spec() {
        // SAFETY: zero-init is valid for `LiquidityNexus` (all-primitive
        // fields, `#[repr(C, packed)]`). See `state::tests` for the
        // size/layout pin.
        let buf = [0u8; core::mem::size_of::<LiquidityNexus>()];
        let mut nexus: LiquidityNexus =
            unsafe { core::ptr::read(buf.as_ptr() as *const LiquidityNexus) };

        // Replicate the handler's writes (Layer 9 §4.1 logic steps 2-6).
        let manager = [0xABu8; 32];
        let bump = 0xFD;
        nexus.manager = manager;
        nexus.total_deposited_usdc = 0;
        nexus.total_deposited_rwt = 0;
        nexus.is_active = true;
        nexus.bump = bump;

        // Layer 9 §4.1 — Manager set as supplied.
        assert_eq!(nexus.manager, manager);
        // Counters start at 0 (monotonically non-decreasing thereafter).
        assert_eq!({ nexus.total_deposited_usdc }, 0);
        assert_eq!({ nexus.total_deposited_rwt }, 0);
        // is_active flips to true — the Nexus is "live" after init.
        assert!(nexus.is_active);
        // Bump persisted for signer-seed reconstruction (D23 / R23).
        assert_eq!(nexus.bump, bump);
    }

    /// The `#[account(init, seeds = [LIQUIDITY_NEXUS_SEED])]` derivation
    /// must use the singleton `b"liquidity_nexus"` seed. If anyone
    /// changes the seed (e.g. introduces a per-mint suffix), the on-chain
    /// PDA migrates and existing Nexus state becomes orphaned — this
    /// test pins the seed constant value.
    #[test]
    fn initialize_nexus_uses_singleton_seed() {
        assert_eq!(LIQUIDITY_NEXUS_SEED, b"liquidity_nexus");
    }

    /// `LiquidityNexus::SPACE` must be exactly 58 bytes (8 disc + 50 data).
    /// The handler uses `space = LiquidityNexus::SPACE` in the `init`
    /// constraint; if the struct grows and `SPACE` follows but no
    /// migration is staged, on-chain accounts become unreadable. State
    /// already pins this at compile-time + via `state::tests`; we
    /// duplicate the runtime check here so the failure surface includes
    /// the `initialize_nexus` test set (faster failure attribution).
    #[test]
    fn initialize_nexus_space_matches_pinned_layout() {
        assert_eq!(LiquidityNexus::SPACE, 58);
    }
}
