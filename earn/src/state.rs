use arlex_lang::prelude::*;

// =============================================================================
// EarnConfig — singleton config PDA for the `earn` program.
// PDA Seed: ["earn_config"]
//
// NAV is derived, not stored:
//   NAV = total_invested_capital × NAV_SCALE / total_rwt_supply
// (with INITIAL_NAV = $1.00 guard when total_rwt_supply == 0)
//
// `total_invested_capital` (Book NAV numerator) changes via:
//   - mint_rwt        — += body (supply grows in lockstep → NAV unchanged).
//                        The 1% fee is DAO revenue, EXCLUDED from capital.
//   - add_to_basket   — += amount, supply unchanged → NAV grows.
//   - writedown_capital — -= amount, supply unchanged → NAV falls.
//
// One vault, not three: liquidity provisioning and the income split are
// off-chain (see docs/architecture/earn-layers.mdx). The contract holds
// only basket capital — `basket_vault` (USDC, EarnConfig-PDA-owned).
//
// NOTE: Option<Pubkey> → [u8;32]+bool for repr(C,packed) compatibility.
// Arlex #[account] uses repr(C,packed) which doesn't support Option<T>.
// =============================================================================

// ── Forward-compatibility: schema_version + _reserved ───────────────────────
// To add a new field AFTER mainnet WITHOUT changing the account size or the
// program ID:
//   1. Shrink `_reserved` by sizeof(new field): [u8; 128] → [u8; 128 - K].
//   2. Insert the new field immediately BEFORE `_reserved`.
//   3. Bump `schema_version` (1 → 2 → …).
//   4. Total SIZE/SPACE is UNCHANGED → `assert_account_size` keeps passing and
//      old + new accounts are both exactly SPACE bytes.
//   5. Accounts written by an older version read the new field as ZERO (the old
//      reserve bytes were zero). If 0 is NOT a safe default, gate the field's
//      use on `schema_version >= N` and lazily initialize it.
// FROZEN INVARIANTS: never reorder/resize an EXISTING field, never grow the
// struct past SPACE, `_reserved` stays the LAST field. Exhausting `_reserved`
// or restructuring an existing field requires a fresh program ID + audited
// migration (there is intentionally no general migrate_config rewrite lever).
#[account]
pub struct EarnConfig {
    pub total_invested_capital: u128,  // 16 — Book NAV numerator
    pub authority: [u8; 32],           // 32 — V1: single key, V2: multisig
    pub pending_authority: [u8; 32],   // 32 (zeroed = no pending transfer)
    pub has_pending: bool,             // 1
    pub mint_fee_bps: u16,             // 2 — default 100 (1%); tunable via update_config
    pub basket_vault: [u8; 32],        // 32 — USDC vault, EarnConfig-PDA-owned (immutable)
    pub dao_fee_destination: [u8; 32], // 32 — USDC ATA for the 1% commission (tunable)
    pub rwt_mint: [u8; 32],            // 32 — the earn-RWT mint (mint authority = EarnConfig PDA)
    pub usdc_mint: [u8; 32],           // 32 — deposit currency
    pub min_mint_amount: u64,          // 8 — anti-dust floor
    pub bump: u8,                      // 1 — PDA bump
    pub schema_version: u8,            // 1 — layout version, = 1 at initialize
    pub _reserved: [u8; 128],          // 128 — reserved padding; future fields carve from its FRONT
}
// SIZE = 16 + 32 + 32 + 1 + 2 + 32 + 32 + 32 + 32 + 8 + 1 + 1 + 128 = 349
// SPACE = 8 + 349 = 357
//   running: 16,48,80,81,83,115,147,179,211,219,220,221,349

const _: () = assert!(core::mem::size_of::<EarnConfig>() == 349);

impl EarnConfig {
    pub fn assert_account_size(account: &AccountView) -> Result<()> {
        if account.data_len() != Self::SPACE {
            return Err(ProgramError::InvalidAccountData);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Layout proof: SIZE/SPACE follow the struct size (auto-derived by
    // #[account]). The schema_version + _reserved padding sit at the END so
    // every prior field offset is unchanged versus the pre-forward-compat
    // layout (SIZE 220).
    #[test]
    fn layout_size_and_space_are_pinned() {
        assert_eq!(EarnConfig::SIZE, 349);
        assert_eq!(EarnConfig::SPACE, 357);
    }

    // `_reserved` must remain the trailing 128 bytes: a future field carves
    // from its front, keeping total SIZE constant. The forward-compat invariant
    // is that the discriminator-relative tail is exactly schema_version (1) +
    // _reserved (128) = 129 bytes.
    #[test]
    fn reserved_padding_is_the_trailing_tail() {
        assert_eq!(core::mem::size_of::<[u8; 128]>(), 128);
        // bump(1) + schema_version(1) + _reserved(128) = 130 trailing bytes
        // over the 219-byte head (everything up to and including min_mint_amount).
        assert_eq!(EarnConfig::SIZE, 219 + 1 + 1 + 128);
    }
}
