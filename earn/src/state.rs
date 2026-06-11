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
}
// SIZE = 16 + 32 + 32 + 1 + 2 + 32 + 32 + 32 + 32 + 8 + 1 = 220
// SPACE = 8 + 220 = 228
//   running: 16,48,80,81,83,115,147,179,211,219,220

const _: () = assert!(core::mem::size_of::<EarnConfig>() == 220);

impl EarnConfig {
    pub fn assert_account_size(account: &AccountView) -> Result<()> {
        if account.data_len() != Self::SPACE {
            return Err(ProgramError::InvalidAccountData);
        }

        Ok(())
    }
}
