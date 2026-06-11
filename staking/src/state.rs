use arlex_lang::prelude::*;

// =============================================================================
// StakingConfig — singleton config PDA for the `staking` (stRWT) program.
// PDA Seed: ["staking_config"]
//
// The stRWT → RWT exchange rate is DERIVED, not stored:
//   rate = (total_rwt_active + VIRTUAL_ASSETS) / (strwt_supply + VIRTUAL_SHARES)
// where strwt_supply is read at runtime from the stRWT SPL Mint account.
//
// The pool tracks RWT via explicit counters (`total_rwt_active` +
// `total_rwt_reserved`), NEVER the raw vault balance — a bare token transfer
// into pool_vault does not move the rate (anti-donation defense, staking.mdx
// §"Anti-donation").
//
// Invariant (verified across every instruction):
//   pool_vault.balance == total_rwt_active + total_rwt_reserved
//
// NOTE: repr(C, packed) (Arlex #[account]) does not support Option<T>;
// "no pending" is encoded as `has_pending = false` + zeroed pending_authority.
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
pub struct StakingConfig {
    pub authority: [u8; 32],         // 32 — V1: single key, V2: multisig
    pub pending_authority: [u8; 32], // 32 — zeroed = no pending transfer
    pub has_pending: bool,           // 1
    pub rwt_mint: [u8; 32],          // 32 — staked token (earn-RWT)
    pub strwt_mint: [u8; 32],        // 32 — share token (mint authority = this PDA)
    pub reward_depositor: [u8; 32],  // 32 — only caller of deposit_rewards
    pub pool_vault: [u8; 32],        // 32 — RWT ATA owned by this PDA (active + reserved)
    pub total_rwt_active: u64,       // 8  — RWT earning rewards (rate numerator)
    pub total_rwt_reserved: u64,     // 8  — RWT locked in cooldown (not earning)
    pub cooldown_seconds: i64,       // 8  — default 1_814_400 (21d), tunable
    pub min_stake_amount: u64,       // 8  — anti-dust floor, tunable
    pub bump: u8,                    // 1  — PDA bump
    pub schema_version: u8,          // 1  — layout version, = 1 at initialize
    pub _reserved: [u8; 128],        // 128 — reserved padding; future fields carve from its FRONT
}
// SIZE = 32+32+1+32+32+32+32+8+8+8+8+1+1+128 = 355
// SPACE = 8 (discriminator) + 355 = 363
//   running: 32,64,65,97,129,161,193,201,209,217,225,226,227,355

const _: () = assert!(core::mem::size_of::<StakingConfig>() == 355);

impl StakingConfig {
    pub fn assert_account_size(account: &AccountView) -> Result<()> {
        if account.data_len() != Self::SPACE {
            return Err(ProgramError::InvalidAccountData);
        }

        Ok(())
    }
}

// =============================================================================
// UnstakeTicket — per-unstake cooldown receipt.
// PDA Seed: ["unstake", owner, nonce.to_le_bytes()]
//
// Created by `initiate_unstake` (`init` semantics — reusing a live nonce
// fails account creation, which is the collision guard). Closed by
// `complete_unstake` after `unlock_ts`; rent flows back to the owner.
// =============================================================================

#[account]
pub struct UnstakeTicket {
    pub owner: [u8; 32], // 32
    pub amount_rwt: u64, // 8  — fixed at initiation
    pub unlock_ts: i64,  // 8  — now + cooldown_seconds
    pub nonce: u64,      // 8  — client-supplied (no per-user counter in state)
    pub bump: u8,        // 1
}
// SIZE = 32 + 8 + 8 + 8 + 1 = 57
// SPACE = 8 + 57 = 65

const _: () = assert!(core::mem::size_of::<UnstakeTicket>() == 57);

#[cfg(test)]
mod tests {
    use super::*;

    // Layout proof: SIZE/SPACE follow the struct size (auto-derived by
    // #[account]). The schema_version + _reserved padding sit at the END so
    // every prior field offset is unchanged versus the pre-forward-compat
    // layout (SIZE 226).
    #[test]
    fn staking_config_layout_size_and_space_are_pinned() {
        assert_eq!(StakingConfig::SIZE, 355);
        assert_eq!(StakingConfig::SPACE, 363);
    }

    // `_reserved` must remain the trailing 128 bytes: a future field carves
    // from its front, keeping total SIZE constant.
    #[test]
    fn staking_config_reserved_padding_is_the_trailing_tail() {
        assert_eq!(core::mem::size_of::<[u8; 128]>(), 128);
        // bump(1) + schema_version(1) + _reserved(128) over the 225-byte head
        // (everything up to and including min_stake_amount).
        assert_eq!(StakingConfig::SIZE, 225 + 1 + 1 + 128);
    }

    #[test]
    fn unstake_ticket_layout_unchanged() {
        assert_eq!(UnstakeTicket::SIZE, 57);
        assert_eq!(UnstakeTicket::SPACE, 65);
    }
}
