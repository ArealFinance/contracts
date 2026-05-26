//! `initialize` — bootstrap the `earn` program. One-time, deployer-only.
//!
//! Creates the singleton `EarnConfig` PDA at seed `["earn_config"]`,
//! pins the authority, pause authority, the three USDC wallet ATAs, and
//! the earn-RWT mint. Initial NAV is implicit ($1.00 via the
//! `total_invested_capital == 0 && supply == 0` guard) — no separate
//! NAV write needed.
//!
//! TODO(L1): full handler implementation (account creation + field writes
//! + event emit). This scaffold only declares the Accounts shape so the
//! crate compiles.

use arlex_lang::prelude::*;

use crate::constants::{EARN_CONFIG_SEED, SPL_TOKEN_PROGRAM, SYSTEM_PROGRAM};

#[derive(Accounts)]
pub struct Initialize<'info> {
    /// Deployer / authority. Pays for the EarnConfig PDA rent.
    #[account(mut, signer)]
    pub authority: &'info AccountView,

    /// EarnConfig PDA — to be created here.
    #[account(mut)]
    pub earn_config: &'info AccountView,

    /// New earn-RWT mint. Created off-chain via spl-token before this ix.
    /// Mint authority MUST be set to the EarnConfig PDA before init.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    /// USDC mint (just for validation; not written).
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub usdc_mint: &'info AccountView,

    /// Operator-controlled USDC ATA where the RWA-bucket share lands.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwa_wallet: &'info AccountView,

    /// USDC ATA owned by EarnConfig PDA. Counts in NAV; future versions
    /// may CPI into native-dex from here.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub liquidity_wallet: &'info AccountView,

    /// ARL Treasury USDC ATA. NOT in NAV.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub treasury_wallet: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

pub fn handler(
    _ctx: Context<Initialize>,
    _pause_authority: [u8; 32],
) -> Result<()> {
    // TODO(L1): implement
    // 1. PDA derive check on `earn_config` against ["earn_config"]
    // 2. Validate mints (usdc, rwt)
    // 3. Validate ATA owners + mints (rwa/liquidity/treasury all USDC; liquidity owned by PDA)
    // 4. CreateAccount for EarnConfig (signed seeds)
    // 5. Write all fields with defaults from constants
    // 6. Emit EarnInitialized
    let _ = EARN_CONFIG_SEED;
    Ok(())
}
