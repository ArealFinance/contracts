use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::DexConfigUpdated;
use crate::state::DexConfig;

#[derive(Accounts)]
pub struct UpdateDexConfig<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "DexConfig",
        seeds = [b"dex_config"], bump
    )]
    pub dex_config: &'info AccountView,
}

pub fn handler(
    ctx: Context<UpdateDexConfig>,
    base_fee_bps: u16,
    lp_fee_share_bps: u16,
    rebalancer: [u8; 32],
    is_active: bool,
) -> Result<()> {
    // Fee bounds: [0, MAX_FEE_BPS]. base_fee_bps == 0 is accepted by design (L-3);
    // operators may waive protocol fees during promotions or emergency liquidity.
    // Existing pools retain their own base_fee_bps captured at create time, so a
    // global change does not affect active pools — only new pools (N-12).
    if base_fee_bps > MAX_FEE_BPS {
        return Err(ProgramError::from(DexError::InvalidFee));
    }
    if lp_fee_share_bps > 10_000 {
        return Err(ProgramError::from(DexError::InvalidFeeShare));
    }

    // `mut` binding: field writes go through the guard's DerefMut. No CPI.
    let mut config = DexConfig::load_mut(ctx.accounts.dex_config, ctx.program_id)?;

    // areal_fee_destination and pause_authority remain immutable
    config.base_fee_bps = base_fee_bps;
    config.lp_fee_share_bps = lp_fee_share_bps;
    // CP-12.5: Setting rebalancer to REBALANCER_KILL_SWITCH ([0u8;32]) is
    // permitted by design — the cryptographic impossibility of signing for
    // the zero pubkey freezes grow_liquidity / compress_liquidity until
    // the authority restores a real Rebalancer. Symmetric with
    // LiquidityNexus.manager kill-switch.
    config.rebalancer = rebalancer;
    config.is_active = is_active;

    let clock = Clock::get()?;
    emit!(DexConfigUpdated {
        base_fee_bps,
        lp_fee_share_bps,
        rebalancer,
        is_active,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
