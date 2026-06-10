use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::ArealFeeDestinationUpdated;
use crate::state::DexConfig;
use crate::validation::{pubkey_bytes, read_token_account_mint};

/// Authority-only rotation of `DexConfig.areal_fee_destination`.
///
/// Companion ix to `update_dex_config` — kept as a SEPARATE instruction so
/// existing callers of `update_dex_config` (bootstrap-init, zero-out-dex-fee,
/// cu-profile) are unaffected, and authorities must consciously choose to
/// rotate the destination rather than touching it accidentally during a
/// fee-bps tweak.
///
/// Validates on-chain that the new destination's mint == `RWT_MINT`. This
/// matches the runtime guard applied by `swap.rs` / `zap_liquidity.rs` after
/// the P0-2 fix (commit `695ff7e`): the silent mint-mismatch fallback was
/// removed, so swaps brick if `areal_fee_destination` is not an RWT ATA.
/// Validating here means misconfiguration cannot be re-introduced via this
/// path, and the runtime mint-checks become defence-in-depth rather than a
/// guard against bootstrap mistakes.
#[derive(Accounts)]
pub struct UpdateArealFeeDestination<'info> {
    #[account(signer)]
    pub authority: &'info AccountView,

    #[account(
        mut, has_one = authority, account_type = "DexConfig",
        seeds = [b"dex_config"], bump
    )]
    pub dex_config: &'info AccountView,

    /// New protocol fee destination — must be an SPL Token Account whose
    /// mint equals `RWT_MINT`. The mint is validated on-chain below.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub new_areal_fee_account: &'info AccountView,
}

pub fn handler(
    ctx: Context<UpdateArealFeeDestination>,
) -> Result<()> {
    // Validate that the new destination's mint is RWT. Same guard that
    // swap.rs / zap_liquidity.rs apply at runtime — applying it here
    // means the on-chain stored destination is always an RWT ATA.
    let dest_mint = read_token_account_mint(ctx.accounts.new_areal_fee_account)?;
    if dest_mint != RWT_MINT {
        return Err(ProgramError::from(DexError::InvalidProtocolFeeDestination));
    }

    let new_destination = pubkey_bytes(ctx.accounts.new_areal_fee_account);

    // Reject the zero pubkey explicitly. `initialize_dex` rejects zero in
    // `areal_fee_destination`; mirror that invariant here so rotation
    // cannot brick the protocol either. (Defense-in-depth — a zero-mint
    // token account would already fail the RWT_MINT check above, but the
    // explicit guard documents the invariant.)
    if new_destination == [0u8; 32] {
        return Err(ProgramError::from(DexError::InvalidFeeDestination));
    }

    // `mut` binding: field write goes through the guard's DerefMut. No CPI.
    let mut config = DexConfig::load_mut(ctx.accounts.dex_config, ctx.program_id)?;

    // Capture the OLD destination for the event so observers can verify
    // the rotation off-chain (audit trail).
    let old_destination = config.areal_fee_destination;

    if old_destination == new_destination {
        // No-op — caller passed the current destination. Idempotent; skip
        // the event to avoid noise.
        return Ok(());
    }

    config.areal_fee_destination = new_destination;

    let clock = Clock::get()?;
    emit!(ArealFeeDestinationUpdated {
        old_destination,
        new_destination,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
