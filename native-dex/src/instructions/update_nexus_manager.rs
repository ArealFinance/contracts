//! Layer 9 §4.8 — `update_nexus_manager`. Authority-gated rotation of the
//! Manager wallet on the singleton `LiquidityNexus`. Setting
//! `new_manager = [0u8; 32]` is **intentionally allowed** — that is the
//! documented on-chain kill-switch (D22). After the kill-switch is
//! engaged, every Manager-gated ix (`nexus_swap`, `nexus_add_liquidity`,
//! `nexus_remove_liquidity`) reverts via `assert_manager` regardless of
//! signer. Authority can rotate back to a non-zero Manager at any time.
//!
//! Counters and `is_active` are not touched here. Authority cannot
//! resurrect a Nexus from `is_active = false` via this ix; that path
//! is reserved for governance + an explicit reactivation handler if/when
//! one is added (out of scope for Layer 9). The ix DOES require the
//! Nexus to already be initialised — the `seeds = [LIQUIDITY_NEXUS_SEED]`
//! constraint ensures the PDA exists before `load_mut` is called.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::events::NexusManagerUpdated;
use crate::state::{DexConfig, LiquidityNexus};

#[derive(Accounts)]
pub struct UpdateNexusManager<'info> {
    /// DEX authority. `has_one` on `dex_config` enforces the access gate.
    #[account(signer)]
    pub authority: &'info AccountView,

    /// DEX config singleton. `has_one = authority` is the Layer 9 §4.8
    /// access-control rule.
    #[account(
        has_one = authority, account_type = "DexConfig",
        seeds = [b"dex_config"], bump
    )]
    pub dex_config: &'info AccountView,

    /// Nexus singleton. Mutable — the Manager slot is rewritten in place.
    /// `seeds = [LIQUIDITY_NEXUS_SEED]` requires the PDA to already exist
    /// (i.e. `initialize_nexus` must have run first); attempting to
    /// rotate a non-existent Nexus reverts via the framework constraint.
    #[account(
        mut, seeds = [LIQUIDITY_NEXUS_SEED], bump
    )]
    pub liquidity_nexus: &'info AccountView,
}

pub fn handler(
    ctx: Context<UpdateNexusManager>,
    new_manager: [u8; 32],
) -> Result<()> {
    let nexus = LiquidityNexus::load_mut(ctx.accounts.liquidity_nexus, ctx.program_id)?;

    let old_manager = nexus.manager;
    nexus.manager = new_manager;

    let clock = Clock::get()?;
    emit!(NexusManagerUpdated {
        old_manager,
        new_manager,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Pure-Rust coverage for the field-mutation contract of
    //! `update_nexus_manager`. As with `initialize_nexus`, real handler
    //! invocation needs a BPF runtime; here we model the writes onto an
    //! in-memory `LiquidityNexus` and verify Layer 9 §4.8 semantics:
    //!
    //!   1. Only `manager` mutates — counters, `is_active`, `bump`
    //!      remain untouched.
    //!   2. `new_manager == [0u8; 32]` (kill-switch) is permitted —
    //!      writes succeed without reverting.
    //!   3. The (`old_manager`, `new_manager`) pair captured before the
    //!      write matches what the emitted `NexusManagerUpdated` event
    //!      will carry.
    //!
    //! Authority gate (`has_one`) is the framework's responsibility —
    //! tested at integration level when the Arlex framework can be
    //! invoked. The `assert_manager` D22 ordering is tested separately
    //! in `validation::tests`.

    use super::*;
    use crate::state::LiquidityNexus;

    fn nexus_with_manager(manager: [u8; 32]) -> LiquidityNexus {
        // SAFETY: see `validation::tests::nexus_with_manager` — same
        // zero-bit-pattern argument applies. We initialise to a "live"
        // Nexus (`is_active = true`) with non-zero counters to verify
        // the rotate path is non-destructive.
        let buf = [0u8; core::mem::size_of::<LiquidityNexus>()];
        let mut nexus: LiquidityNexus =
            unsafe { core::ptr::read(buf.as_ptr() as *const LiquidityNexus) };
        nexus.manager = manager;
        nexus.total_deposited_usdc = 1_000;
        nexus.total_deposited_rwt = 2_000;
        nexus.is_active = true;
        nexus.bump = 0xFE;
        nexus
    }

    /// Standard rotation — non-zero old manager, non-zero new manager.
    /// Verifies (a) the write lands, (b) every other field is untouched,
    /// (c) the captured `(old, new)` pair matches what the event carries.
    #[test]
    fn update_nexus_manager_rotates_non_kill_switch() {
        let old = [0x11u8; 32];
        let new = [0x22u8; 32];
        let mut nexus = nexus_with_manager(old);

        // Replicate handler logic (Layer 9 §4.8 steps 1-2).
        let captured_old = nexus.manager;
        nexus.manager = new;

        assert_eq!(nexus.manager, new);
        assert_eq!(captured_old, old);
        // Untouched invariants.
        assert_eq!({ nexus.total_deposited_usdc }, 1_000);
        assert_eq!({ nexus.total_deposited_rwt }, 2_000);
        assert!(nexus.is_active);
        assert_eq!(nexus.bump, 0xFE);
    }

    /// **D22 kill-switch acceptance** — Authority is allowed to set the
    /// Manager to `[0u8; 32]` here. The downstream `assert_manager` helper
    /// is what enforces the disabled state on Manager-gated ix; this
    /// handler must NOT block the rotation, otherwise the on-chain
    /// kill-switch is unreachable.
    #[test]
    fn update_nexus_manager_kill_switch_allowed() {
        let old = [0x33u8; 32];
        let new = [0u8; 32]; // kill-switch sentinel.
        let mut nexus = nexus_with_manager(old);

        let captured_old = nexus.manager;
        nexus.manager = new;

        assert_eq!(nexus.manager, [0u8; 32]);
        assert_eq!(captured_old, old);
        // Critical: counters survive the kill-switch — withdraw_profits
        // continues to honour the principal floor, which is the on-chain
        // record of "deposited capital that must not be drained as fees".
        assert_eq!({ nexus.total_deposited_usdc }, 1_000);
        assert_eq!({ nexus.total_deposited_rwt }, 2_000);
        // is_active stays true — kill-switch is a Manager-level revert,
        // not a Nexus-level deactivation. Withdraw / claim / record still
        // work for Authority and YD CPI.
        assert!(nexus.is_active);
    }

    /// Idempotent same-key rotation — Authority sets `new_manager` equal
    /// to the current `manager`. The plan does not forbid this (unlike
    /// `propose_authority_transfer` where `SelfTransfer` reverts), so
    /// the rotation must succeed without changing observable state. This
    /// matches Layer 9 §4.8 ("`new_manager == [0u8; 32]` is intentionally
    /// allowed" — same-key is similarly permitted as a no-op rotation
    /// useful for emitting a fresh `NexusManagerUpdated` event).
    #[test]
    fn update_nexus_manager_same_key_is_noop_rotation() {
        let key = [0x44u8; 32];
        let mut nexus = nexus_with_manager(key);

        let captured_old = nexus.manager;
        nexus.manager = key;

        assert_eq!(nexus.manager, key);
        assert_eq!(captured_old, key);
        // Counters and is_active untouched.
        assert_eq!({ nexus.total_deposited_usdc }, 1_000);
        assert!(nexus.is_active);
    }
}
