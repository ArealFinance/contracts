use arlex_lang::prelude::*;

// =============================================================================
// EarnConfig — singleton config PDA for the `earn` program.
// PDA Seed: ["earn_config"]
//
// NAV is derived, not stored:
//   NAV = total_invested_capital × NAV_SCALE / total_rwt_supply
//
// `total_invested_capital` grows from:
//   - mint_rwt (rwa_amount + liquidity_amount added; treasury_amount excluded)
//   - stream_inflow (full amount added)
// and shrinks from:
//   - writedown_capital (authority-only, for RWA depreciation)
//
// Wallets are USDC ATAs. `liquidity_wallet` is owned by this program's PDA
// so future versions can CPI directly into native-dex; `rwa_wallet` and
// `treasury_wallet` are owned by authority/operator and Areal multisig
// respectively.
// =============================================================================

#[account]
pub struct EarnConfig {
    pub total_invested_capital: u128,    // 16 — drives NAV numerator
    // total_rwt_supply lives on the SPL Mint account (read at runtime)

    pub authority: [u8; 32],             // 32 — V1: single key, V2: multisig
    pub pending_authority: [u8; 32],     // 32 (zeroed = no pending transfer)
    pub has_pending: bool,               // 1
    pub pause_authority: [u8; 32],       // 32 (immutable after init)
    pub is_paused: bool,                 // 1

    // Mint split (must sum to 10_000)
    pub split_rwa_bps: u16,              // 2
    pub split_liquidity_bps: u16,        // 2
    pub split_treasury_bps: u16,         // 2

    // USDC ATAs
    pub rwa_wallet: [u8; 32],            // 32 — operator-controlled, buys underlying
    pub liquidity_wallet: [u8; 32],      // 32 — counts in NAV, future LP source
    pub treasury_wallet: [u8; 32],       // 32 — ARL revenue, NOT in NAV

    // Token mints
    pub rwt_mint: [u8; 32],              // 32 — the earn-RWT mint
    pub usdc_mint: [u8; 32],             // 32 — deposit currency

    pub min_mint_amount: u64,            // 8 — anti-dust floor
    pub bump: u8,                        // 1
}
// SIZE = 16 + 32 + 32 + 1 + 32 + 1 + 2 + 2 + 2 + 32 + 32 + 32 + 32 + 32 + 8 + 1
//      = 289
// SPACE = 8 + 289 = 297

const _: () = assert!(core::mem::size_of::<EarnConfig>() == 289);
