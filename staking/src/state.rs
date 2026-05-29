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

#[account]
pub struct StakingConfig {
    pub authority: [u8; 32],          // 32 — V1: single key, V2: multisig
    pub pending_authority: [u8; 32],  // 32 — zeroed = no pending transfer
    pub has_pending: bool,            // 1
    pub pause_authority: [u8; 32],    // 32 — immutable after init
    pub is_paused: bool,              // 1  — gates stake/unstake (not deposit_rewards)
    pub rwt_mint: [u8; 32],           // 32 — staked token (earn-RWT)
    pub strwt_mint: [u8; 32],         // 32 — share token (mint authority = this PDA)
    pub reward_depositor: [u8; 32],   // 32 — only caller of deposit_rewards
    pub pool_vault: [u8; 32],         // 32 — RWT ATA owned by this PDA (active + reserved)
    pub total_rwt_active: u64,        // 8  — RWT earning rewards (rate numerator)
    pub total_rwt_reserved: u64,      // 8  — RWT locked in cooldown (not earning)
    pub cooldown_seconds: i64,        // 8  — default 1_814_400 (21d), tunable
    pub min_stake_amount: u64,        // 8  — anti-dust floor, tunable
    pub bump: u8,                     // 1  — PDA bump
}
// SIZE = 32+32+1+32+1+32+32+32+32+8+8+8+8+1 = 259
// SPACE = 8 (discriminator) + 259 = 267

const _: () = assert!(core::mem::size_of::<StakingConfig>() == 259);

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
    pub owner: [u8; 32],   // 32
    pub amount_rwt: u64,   // 8  — fixed at initiation
    pub unlock_ts: i64,    // 8  — now + cooldown_seconds
    pub nonce: u64,        // 8  — client-supplied (no per-user counter in state)
    pub bump: u8,          // 1
}
// SIZE = 32 + 8 + 8 + 8 + 1 = 57
// SPACE = 8 + 57 = 65

const _: () = assert!(core::mem::size_of::<UnstakeTicket>() == 57);
