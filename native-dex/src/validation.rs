use arlex_lang::prelude::*;

use crate::error::DexError;
use crate::state::LiquidityNexus;

/// Read mint field (first 32 bytes) from an SPL Token Account.
/// SPL Token Account layout: [mint: 32][owner: 32][amount: u64][...]
///
/// L-5: `unsafe` is the standard Pinocchio pattern for zero-copy account access.
/// The `AccountView` exposes `data_ptr()` and `data_len()` as-is from the Solana
/// BPF loader; raw slice construction is sound as long as we respect `data_len()`
/// (Solana guarantees the region is valid for the lifetime of the instruction).
/// The explicit length check below ensures no out-of-bounds slicing.
///
/// N-6 audit note: duplicated in `rwt-engine` and `yield-distribution`
/// (identical semantics). Consolidation into a shared arlex helper is deferred
/// to the next arlex minor release.
pub fn read_token_account_mint(account: &AccountView) -> core::result::Result<[u8; 32], ProgramError> {
    let data = unsafe {
        core::slice::from_raw_parts(account.data_ptr(), account.data_len())
    };
    if data.len() < 32 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut mint = [0u8; 32];
    mint.copy_from_slice(&data[0..32]);
    Ok(mint)
}

/// Read owner field (bytes 32..64) from an SPL Token Account.
///
/// L-5: same Pinocchio zero-copy pattern as `read_token_account_mint`. Length
/// check prevents OOB reads.
pub fn read_token_account_owner(account: &AccountView) -> core::result::Result<[u8; 32], ProgramError> {
    let data = unsafe {
        core::slice::from_raw_parts(account.data_ptr(), account.data_len())
    };
    if data.len() < 64 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut owner = [0u8; 32];
    owner.copy_from_slice(&data[32..64]);
    Ok(owner)
}

/// Validate vault account matches the stored vault address in pool_state.
pub fn validate_vault(vault: &AccountView, expected: &[u8; 32]) -> core::result::Result<(), ProgramError> {
    if vault.address().as_ref() != expected.as_ref() {
        return Err(ProgramError::from(crate::error::DexError::InvalidVault));
    }
    Ok(())
}

/// Extract pubkey bytes from an AccountView into a [u8; 32].
pub fn pubkey_bytes(account: &AccountView) -> [u8; 32] {
    let mut arr = [0u8; 32];
    arr.copy_from_slice(account.address().as_ref());
    arr
}

/// Check if an address is the RWT mint.
pub fn is_rwt_mint(addr: &[u8; 32]) -> bool {
    *addr == crate::constants::RWT_MINT
}

/// Determine which side of a pool pair is RWT. Returns true if token_a is RWT.
pub fn token_a_is_rwt(token_a_mint: &[u8; 32], token_b_mint: &[u8; 32]) -> core::result::Result<bool, ProgramError> {
    if is_rwt_mint(token_a_mint) {
        Ok(true)
    } else if is_rwt_mint(token_b_mint) {
        Ok(false)
    } else {
        Err(ProgramError::from(crate::error::DexError::MissingRwtMint))
    }
}

/// Verify Manager-gated access to Layer 9 Nexus instructions (single source of
/// truth, D22). Used by `nexus_swap`, `nexus_add_liquidity`,
/// `nexus_remove_liquidity` (future Layer 9 work). NOT used by `nexus_claim_rewards`
/// (Authority-gated, not Manager).
///
/// **Order matters (D22):** the kill-switch check fires BEFORE the signer
/// check. Setting `nexus.manager = [0u8; 32]` is the documented on-chain
/// kill-switch (`update_nexus_manager` allows it). Even if a wallet whose
/// bytes happened to be `[0u8; 32]` were able to sign (it cannot — this is
/// the system pubkey), the call would still revert with
/// `NexusManagerDisabled`. This collapses the kill-switch into a structural
/// invariant rather than a wallet-key check.
///
/// Returns:
/// - `NexusManagerDisabled` when `nexus.manager == [0u8; 32]`.
/// - `InvalidNexusManager` when `signer.address() != nexus.manager`.
/// - `Ok(())` when both checks pass.
///
/// `nexus.is_active` is intentionally NOT checked here. Layer 9 §4.3-§4.5
/// rely on the `is_active` gate being enforced upstream by the Arlex account
/// constraint (or by the calling handler's preflight); the helper's sole
/// concern is Manager auth.
pub fn assert_manager(
    nexus: &LiquidityNexus,
    signer: &AccountView,
) -> core::result::Result<(), ProgramError> {
    // D22 step 1: kill-switch first — fires regardless of signer.
    if nexus.manager == [0u8; 32] {
        return Err(ProgramError::from(DexError::NexusManagerDisabled));
    }
    // D22 step 2: signer must match Manager.
    if signer.address().as_ref() != nexus.manager {
        return Err(ProgramError::from(DexError::InvalidNexusManager));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for `assert_manager` (D22 helper). The helper is the single
    //! source of truth for Manager auth across the 3 Manager-gated Nexus ix
    //! (`nexus_swap`, `nexus_add_liquidity`, `nexus_remove_liquidity`); we
    //! exercise its happy path and both revert paths, with explicit ordering
    //! coverage for the kill-switch precedence rule (the most security-critical
    //! invariant of the helper).
    //!
    //! `assert_manager` takes `&AccountView`, but the tests can't fabricate
    //! a real `AccountView` without a BPF runtime. We mirror the helper's
    //! pure logic in a test-local twin (`check_manager_bytes`) that takes the
    //! same byte inputs the helper extracts via `signer.address().as_ref()`.
    //! The twin and the helper are kept in lockstep by reading the helper's
    //! source in code review — any divergence would also surface in the
    //! future negative-AC tests where the real handler is invoked.
    //!
    //! When BPF-based test harnesses become available (future BPF tests), these
    //! tests should be replaced with handler-level negative ACs invoking the
    //! real helper through a constructed `AccountView`.

    use super::*;
    use crate::state::LiquidityNexus;

    /// Test-local twin of `assert_manager` operating on raw bytes. Mirrors
    /// the helper's two-step decision (kill-switch first, then signer match).
    /// Any logic drift in the production helper must be reflected here AND
    /// caught by handler-level tests.
    ///
    /// Returns the `ProgramError` produced by the production helper so the
    /// numeric error code is what's compared (the `DexError` enum has no
    /// `PartialEq`; the `ProgramError::Custom(u32)` payload does).
    fn check_manager_bytes(
        nexus: &LiquidityNexus,
        signer_bytes: &[u8; 32],
    ) -> core::result::Result<(), ProgramError> {
        if nexus.manager == [0u8; 32] {
            return Err(ProgramError::from(DexError::NexusManagerDisabled));
        }
        if &nexus.manager != signer_bytes {
            return Err(ProgramError::from(DexError::InvalidNexusManager));
        }
        Ok(())
    }

    /// Convenience: extract the `Custom(u32)` code from a `ProgramError` for
    /// equality comparison. Panics if the error is not a `Custom` variant
    /// (which would itself indicate a regression — every `DexError` lowering
    /// goes through `ProgramError::Custom`).
    fn custom_code(err: ProgramError) -> u32 {
        match err {
            ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    /// Resolve a `DexError` variant to its numeric error code by lowering
    /// through the same `From<DexError> for ProgramError` impl the production
    /// helper uses. Keeps the test asserting on the same codes the on-chain
    /// runtime sees.
    fn code_of(err: DexError) -> u32 {
        custom_code(ProgramError::from(err))
    }

    /// Build a Nexus value with a given manager. Initialised + counters at 0.
    fn nexus_with_manager(manager: [u8; 32]) -> LiquidityNexus {
        // SAFETY: `LiquidityNexus` is `#[repr(C, packed)]` with all-primitive
        // fields summing to 50 bytes; an all-zero bit pattern is valid for
        // every field. We then overwrite `manager` and `is_active` to model
        // an initialised Nexus (only the kill-switch state varies between
        // tests).
        let buf = [0u8; core::mem::size_of::<LiquidityNexus>()];
        let mut nexus: LiquidityNexus =
            unsafe { core::ptr::read(buf.as_ptr() as *const LiquidityNexus) };
        nexus.manager = manager;
        nexus.is_active = true;
        nexus
    }

    /// Happy path — signer bytes match `nexus.manager` (and manager is not
    /// the kill-switch sentinel) → returns Ok.
    #[test]
    fn assert_manager_happy_path() {
        let manager = [0x42u8; 32];
        let nexus = nexus_with_manager(manager);
        assert!(check_manager_bytes(&nexus, &manager).is_ok());
    }

    /// Wrong signer → `InvalidNexusManager`. Manager is non-zero, signer is
    /// any other 32-byte value.
    #[test]
    fn assert_manager_rejects_wrong_signer() {
        let manager = [0x42u8; 32];
        let signer = [0x99u8; 32];
        let nexus = nexus_with_manager(manager);
        let err = check_manager_bytes(&nexus, &signer).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::InvalidNexusManager));
    }

    /// **D22 critical ordering test** — kill-switch must fire BEFORE the
    /// signer check. Construct a Nexus with `manager == [0u8; 32]` and a
    /// signer whose bytes also happen to equal `[0u8; 32]`. Without the
    /// ordering rule, the signer check would pass (manager == signer) and
    /// the call would succeed, defeating the kill-switch entirely. With
    /// the ordering rule, `NexusManagerDisabled` fires first and the call
    /// reverts regardless.
    ///
    /// In production, no real wallet has the all-zero pubkey (it is the
    /// System Program / sentinel), so this scenario is structural rather
    /// than reachable, but the ordering protects us if the helper is ever
    /// reused in a context where signer bytes can be forged.
    #[test]
    fn assert_manager_kill_switch_fires_before_signer_check() {
        let nexus = nexus_with_manager([0u8; 32]);
        let signer_zero = [0u8; 32];
        // signer bytes match nexus.manager bytes — but kill-switch wins.
        let err = check_manager_bytes(&nexus, &signer_zero).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NexusManagerDisabled));
    }

    /// Kill-switch with a non-zero signer → still `NexusManagerDisabled`.
    /// Confirms the kill-switch revert is independent of the signer entirely.
    #[test]
    fn assert_manager_kill_switch_with_nonzero_signer() {
        let nexus = nexus_with_manager([0u8; 32]);
        let signer = [0x42u8; 32];
        let err = check_manager_bytes(&nexus, &signer).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::NexusManagerDisabled));
    }

    /// Boundary check — manager differing from signer in only one byte still
    /// reverts. Guards against partial-equality bugs (e.g. truncated compare).
    #[test]
    fn assert_manager_rejects_off_by_one_byte() {
        let mut manager = [0u8; 32];
        manager[0] = 0x01;
        manager[31] = 0xFE;
        let mut signer = manager;
        signer[31] = 0xFF; // differs in the last byte only
        let nexus = nexus_with_manager(manager);
        let err = check_manager_bytes(&nexus, &signer).unwrap_err();
        assert_eq!(custom_code(err), code_of(DexError::InvalidNexusManager));
    }
}
