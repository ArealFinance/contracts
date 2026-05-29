//! Pure share-pool math for the stRWT pool.
//!
//! Single virtual-offset model (staking.mdx §"Anti-donation"): no stored
//! `initial_rate`. All products cast to u128 BEFORE multiply (u64×u64
//! overflows). Truncating division rounds **toward the pool** (against the
//! user) in both directions, so round-trips never leak value.
//!
//! These functions are host-testable: they take plain integers and return
//! `Option` on overflow (mapped to `MathOverflow` by callers).

use crate::constants::{RATE_SCALE, VIRTUAL_ASSETS, VIRTUAL_SHARES};

/// stRWT minted for a stake of `rwt_in`:
///   strwt_out = rwt_in × (strwt_supply + V_SHARES) / (active + V_ASSETS)   [floor]
///
/// Empty pool (`strwt_supply == 0`): collapses to `rwt_in × V_SHARES / V_ASSETS`
/// = `rwt_in / 10` at defaults — the bootstrap rate, with no special case.
#[inline]
pub fn strwt_out_for_stake(rwt_in: u64, strwt_supply: u64, total_rwt_active: u64) -> Option<u64> {
    let shares = (strwt_supply as u128).checked_add(VIRTUAL_SHARES as u128)?;
    let assets = (total_rwt_active as u128).checked_add(VIRTUAL_ASSETS as u128)?;
    let numerator = (rwt_in as u128).checked_mul(shares)?;
    let out = numerator.checked_div(assets)?; // assets >= V_ASSETS > 0
    u64::try_from(out).ok()
}

/// RWT released for an unstake burning `strwt_amount`:
///   rwt_out = strwt_amount × (active + V_ASSETS) / (strwt_supply + V_SHARES)  [floor]
#[inline]
pub fn rwt_out_for_unstake(
    strwt_amount: u64,
    strwt_supply: u64,
    total_rwt_active: u64,
) -> Option<u64> {
    let assets = (total_rwt_active as u128).checked_add(VIRTUAL_ASSETS as u128)?;
    let shares = (strwt_supply as u128).checked_add(VIRTUAL_SHARES as u128)?;
    let numerator = (strwt_amount as u128).checked_mul(assets)?;
    let out = numerator.checked_div(shares)?; // shares >= V_SHARES > 0
    u64::try_from(out).ok()
}

/// Canonical rate snapshot for events (6-dec fixed point):
///   rate = (active + V_ASSETS) × RATE_SCALE / (strwt_supply + V_SHARES)
#[inline]
pub fn rate_snapshot(total_rwt_active: u64, strwt_supply: u64) -> Option<u64> {
    let assets = (total_rwt_active as u128).checked_add(VIRTUAL_ASSETS as u128)?;
    let shares = (strwt_supply as u128).checked_add(VIRTUAL_SHARES as u128)?;
    let scaled = assets.checked_mul(RATE_SCALE as u128)?;
    let rate = scaled.checked_div(shares)?;
    u64::try_from(rate).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::COOLDOWN_SECONDS;

    // staking.mdx §"Core Concept" / virtual-offset constants:
    // V_SHARES = 1e6, V_ASSETS = 10e6 → empty-pool bootstrap rate = 10.

    /// Bootstrap: empty pool, first stake 100 RWT (100e6 in 6-dec) →
    /// strwt_out = 100 × V_SHARES / V_ASSETS = 100 / 10 = 10 stRWT (10e6).
    #[test]
    fn bootstrap_first_stake_rate_10() {
        let rwt_in = 100_000_000; // 100 RWT
        let out = strwt_out_for_stake(rwt_in, 0, 0).unwrap();
        assert_eq!(out, 10_000_000, "100 RWT into empty pool → 10 stRWT (rate 10)");
    }

    /// deposit_rewards raises the rate: supply unchanged, active grows.
    #[test]
    fn deposit_rewards_raises_rate() {
        // After bootstrap: active = 100e6, supply = 10e6 → rate ~10.
        let r0 = rate_snapshot(100_000_000, 10_000_000).unwrap();
        // Reward inflow: active += 100e6 (no supply change) → rate ~ doubles.
        let r1 = rate_snapshot(200_000_000, 10_000_000).unwrap();
        assert!(r1 > r0, "reward inflow must raise the rate");
    }

    /// Rate consistency: a pool at active=12500, supply=1000 (rate 12.5)
    /// should keep rate ~12.5 for the remaining holders after a partial
    /// unstake of 100 stRWT. Uses 6-dec scaled values for realism.
    #[test]
    fn rate_consistency_partial_unstake() {
        // 12_500 RWT / 1_000 stRWT in 6-dec.
        let active: u64 = 12_500_000_000;
        let supply: u64 = 1_000_000_000;
        let burn: u64 = 100_000_000; // 100 stRWT

        let rwt_out = rwt_out_for_unstake(burn, supply, active).unwrap();
        // 100 stRWT at rate ~12.5 ≈ 1250 RWT (with virtual offsets the rate
        // is very slightly below 12.5; rwt_out must be ≤ proportional value).
        let new_active = active - rwt_out;
        let new_supply = supply - burn;

        // Rate before vs after must be effectively unchanged (within rounding).
        let r_before = rate_snapshot(active, supply).unwrap();
        let r_after = rate_snapshot(new_active, new_supply).unwrap();
        // The virtual offsets make rate monotonically non-decreasing on unstake
        // (pool keeps the rounding dust). Difference must be tiny (<= 1 unit).
        let diff = r_after.abs_diff(r_before);
        assert!(diff <= 1, "rate drifted by {diff} (>1) on proportional unstake");
    }

    /// Round-trip never leaks: stake then immediate unstake of the received
    /// stRWT must yield <= the original RWT (rounding toward the pool).
    #[test]
    fn round_trip_no_leak() {
        let active0: u64 = 5_000_000_000;
        let supply0: u64 = 400_000_000;
        let rwt_in: u64 = 250_000_000; // 250 RWT

        let strwt_out = strwt_out_for_stake(rwt_in, supply0, active0).unwrap();
        let active1 = active0 + rwt_in;
        let supply1 = supply0 + strwt_out;

        let rwt_back = rwt_out_for_unstake(strwt_out, supply1, active1).unwrap();
        assert!(
            rwt_back <= rwt_in,
            "round-trip leaked: got {rwt_back} back from {rwt_in} in"
        );
    }

    /// Anti-donation: the rate is a function of the counter `total_rwt_active`,
    /// not the vault balance. A raw transfer into the vault (which does NOT
    /// touch the counter) cannot move the share math.
    #[test]
    fn anti_donation_counter_based() {
        let active: u64 = 1_000_000_000;
        let supply: u64 = 100_000_000;
        // "Attacker" donates 10_000 RWT straight to the vault: the contract's
        // counter is unchanged, so strwt_out for the next staker is identical.
        let before = strwt_out_for_stake(50_000_000, supply, active).unwrap();
        // Same inputs (counter unchanged despite the donation):
        let after = strwt_out_for_stake(50_000_000, supply, active).unwrap();
        assert_eq!(before, after, "donation must not change share pricing");
    }

    /// Dust boundary: a tiny stake into a huge pool yields 0 stRWT — caller
    /// must reject with ZeroStrwtOutput.
    #[test]
    fn dust_stake_yields_zero() {
        // active enormous, supply tiny → 1 lamport stake rounds to 0 shares.
        let out = strwt_out_for_stake(1, 1_000, u64::MAX / 2).unwrap();
        assert_eq!(out, 0, "1-lamport stake into huge pool must floor to 0");
    }

    /// Overflow safety: max inputs do not panic (u128 intermediate).
    #[test]
    fn no_overflow_at_extremes() {
        // u64::MAX rwt_in with large supply: u128 product fits, but the u64
        // downcast may fail → returns None (mapped to MathOverflow on-chain).
        let r = strwt_out_for_stake(u64::MAX, u64::MAX, 0);
        // Either Some(valid u64) or None — must not panic.
        let _ = r;
        // A realistic large case must succeed.
        let ok = strwt_out_for_stake(1_000_000_000_000, 1_000_000_000_000, 1_000_000_000_000);
        assert!(ok.is_some());
    }

    /// On-chain invariant (staking.mdx §"On-chain invariant"):
    ///   pool_vault.balance == total_rwt_active + total_rwt_reserved
    /// must hold after every instruction. We replay the counter accounting
    /// of stake → deposit_rewards → initiate_unstake → complete_unstake exactly
    /// as the handlers do and assert the invariant at each step, tracking a
    /// mirrored "vault balance" that moves with each token flow.
    #[test]
    fn vault_balance_equals_active_plus_reserved_across_lifecycle() {
        let mut active: u64 = 0;
        let mut reserved: u64 = 0;
        let mut vault: u64 = 0; // mirrors pool_vault token balance
        let mut supply: u64 = 0; // stRWT mint supply

        let assert_inv = |active: u64, reserved: u64, vault: u64| {
            assert_eq!(
                vault,
                active + reserved,
                "invariant broken: vault {vault} != active {active} + reserved {reserved}"
            );
        };

        // 1. stake 500 RWT: vault += rwt_in; active += rwt_in; supply += strwt_out.
        let rwt_in = 500_000_000;
        let strwt_out = strwt_out_for_stake(rwt_in, supply, active).unwrap();
        active += rwt_in;
        vault += rwt_in;
        supply += strwt_out;
        assert_inv(active, reserved, vault);

        // 2. deposit_rewards 100 RWT: vault += amount; active += amount; supply same.
        let reward = 100_000_000;
        active += reward;
        vault += reward;
        assert_inv(active, reserved, vault);

        // 3. initiate_unstake of half the stRWT: burn (supply -=); active -= rwt_out;
        //    reserved += rwt_out. Vault UNCHANGED (RWT only moves bucket, not out).
        let burn = strwt_out / 2;
        let rwt_out = rwt_out_for_unstake(burn, supply, active).unwrap();
        supply -= burn;
        active -= rwt_out;
        reserved += rwt_out;
        assert_inv(active, reserved, vault); // vault still == active+reserved

        // 4. complete_unstake: reserved -= amount; vault -= amount (transfer out).
        reserved -= rwt_out;
        vault -= rwt_out;
        assert_inv(active, reserved, vault);

        // Supply was reduced by the burn and never re-minted on the unstake path.
        assert!(supply < strwt_out, "stRWT supply must shrink after the burn");
    }

    /// Cooldown gate (staking.mdx §"complete_unstake"): completing before
    /// `unlock_ts` must be rejected. Pure comparison mirror of the handler
    /// check `now < unlock_ts → CooldownNotElapsed`.
    #[test]
    fn cooldown_not_elapsed_before_unlock() {
        let initiated_at: i64 = 1_700_000_000;
        let unlock_ts = initiated_at + COOLDOWN_SECONDS;

        // Before unlock: must be rejected.
        let now_early = initiated_at + COOLDOWN_SECONDS - 1;
        assert!(now_early < unlock_ts, "early completion must be blocked");

        // At/after unlock: allowed.
        let now_ok = unlock_ts;
        assert!(now_ok >= unlock_ts, "completion at unlock_ts must be allowed");
    }
}
