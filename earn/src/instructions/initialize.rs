//! `initialize` — bootstrap the `earn` program. One-time, deployer-only.
//!
//! Creates the singleton `EarnConfig` PDA at seed `["earn_config"]`, pins the
//! authority, the `dao_fee_destination` USDC ATA, and the earn-RWT mint.
//!
//! The program does NOT custody USDC: `basket_vault` is an EXTERNAL treasury
//! token account (multisig-owned), so it is NOT created or pinned here. It is
//! left zeroed at init and set later by the authority via `update_config`.
//!
//! Bootstrap is restricted to the pinned `BOOTSTRAP_AUTHORITY`; otherwise the
//! first signer to call `initialize` could capture the singleton config PDA. The
//! earn-RWT mint MUST already be fresh and controlled by the EarnConfig PDA.
//!
//! Initial NAV is implicit ($1.00 via the `total_rwt_supply == 0` guard).

use arlex_lang::prelude::*;
use pinocchio::cpi::{Seed, Signer};
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::EarnError;
use crate::events::EarnInitialized;
use crate::state::EarnConfig;
use crate::validation::{
    read_mint_authority, read_mint_decimals, read_mint_freeze_authority, read_mint_supply,
    read_token_account_mint,
};

#[derive(Accounts)]
pub struct Initialize<'info> {
    /// Deployer. Pays for the EarnConfig PDA rent.
    #[account(mut, signer)]
    pub deployer: &'info AccountView,

    /// EarnConfig PDA — created manually after bootstrap validation.
    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,

    /// Earn-RWT mint. Created off-chain via spl-token before this ix.
    /// Mint authority MUST be set to the EarnConfig PDA before init.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    /// USDC mint (validation only; pinned into config).
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub usdc_mint: &'info AccountView,

    /// USDC ATA receiving the 1% commission (Areal revenue).
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub dao_fee_destination: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,
}

fn validate_bootstrap_inputs(deployer: &[u8], authority: [u8; 32]) -> Result<()> {
    if deployer != BOOTSTRAP_AUTHORITY.as_ref() {
        return Err(ProgramError::from(EarnError::UnauthorizedBootstrap));
    }
    if authority == [0u8; 32] {
        return Err(ProgramError::from(EarnError::ZeroDestination));
    }

    Ok(())
}

fn validate_rwt_mint_for_initialize(
    mint_authority: Option<[u8; 32]>,
    mint_supply: u64,
    mint_decimals: u8,
    freeze_authority: Option<[u8; 32]>,
    config_pda: [u8; 32],
) -> Result<()> {
    if mint_authority != Some(config_pda) {
        return Err(ProgramError::from(EarnError::InvalidMintAuthority));
    }
    if mint_supply != 0 {
        return Err(ProgramError::from(EarnError::InvalidMintSupply));
    }
    if mint_decimals != RWT_DECIMALS {
        return Err(ProgramError::from(EarnError::InvalidMintDecimals));
    }
    if freeze_authority.is_some() {
        return Err(ProgramError::from(EarnError::InvalidFreezeAuthority));
    }

    Ok(())
}

fn validate_initialize_token_accounts(dao_fee_mint: [u8; 32], usdc_mint: [u8; 32]) -> Result<()> {
    // M1: pin the deposit currency to the canonical USDC mint. The passed
    // `usdc_mint` account is only as trustworthy as the deployer; anchoring it
    // to the compiled-in constant prevents bootstrapping the singleton config
    // against a fake/attacker-controlled mint. dao_fee_destination is then
    // transitively pinned to USDC below. basket_vault is NOT validated here —
    // it is an external treasury account set later via `update_config`.
    if usdc_mint != USDC_MINT {
        return Err(ProgramError::from(EarnError::InvalidTokenAccount));
    }
    if dao_fee_mint != usdc_mint {
        return Err(ProgramError::from(EarnError::InvalidFeeDestination));
    }

    Ok(())
}

pub fn handler(ctx: Context<Initialize>, authority: [u8; 32]) -> Result<()> {
    // --- Validate inputs ---
    validate_bootstrap_inputs(ctx.accounts.deployer.address().as_ref(), authority)?;

    let mut usdc_mint = [0u8; 32];
    usdc_mint.copy_from_slice(ctx.accounts.usdc_mint.address().as_ref());

    // --- Compute canonical PDA + bump ---
    let (config_address, config_bump) =
        arlex_lang::find_program_address(&[EARN_CONFIG_SEED], ctx.program_id);
    let mut config_pda = [0u8; 32];
    config_pda.copy_from_slice(config_address.as_ref());

    // The earn-RWT mint must be freshly initialized and controlled solely by
    // EarnConfig PDA before this singleton config is pinned.
    validate_rwt_mint_for_initialize(
        read_mint_authority(ctx.accounts.rwt_mint)?,
        read_mint_supply(ctx.accounts.rwt_mint)?,
        read_mint_decimals(ctx.accounts.rwt_mint)?,
        read_mint_freeze_authority(ctx.accounts.rwt_mint)?,
        config_pda,
    )?;

    // dao_fee_destination must hold USDC. basket_vault is NOT validated here —
    // it is set later via `update_config` (external treasury account).
    let dao_fee_mint = read_token_account_mint(ctx.accounts.dao_fee_destination)?;
    validate_initialize_token_accounts(dao_fee_mint, usdc_mint)?;

    // SECURITY (L1): `dao_fee_destination` is authority-trusted. It is pinned by
    // mint (must be USDC, checked above) and must be non-zero, but its SPL
    // token-account owner is intentionally NOT constrained — the destination is
    // the Areal revenue wallet, chosen by the deployer at init and re-targetable
    // by the authority via `update_config`. There is no canonical on-chain owner
    // to pin (it is not a program PDA), so owner is left unconstrained by design.
    let mut dao_fee_destination = [0u8; 32];
    dao_fee_destination.copy_from_slice(ctx.accounts.dao_fee_destination.address().as_ref());
    if dao_fee_destination == [0u8; 32] {
        return Err(ProgramError::from(EarnError::InvalidFeeDestination));
    }

    // NOTE: the init-time `FeeDestinationIsBasketVault` check is removed — the
    // basket vault is zero at init (set later via `update_config`), so the
    // dao_fee != basket_vault invariant is enforced there against the two
    // incoming values, not here.

    // --- Create the config PDA after all bootstrap gates pass ---
    let config_bump_arr = [config_bump];
    let config_seeds = [
        Seed::from(EARN_CONFIG_SEED),
        Seed::from(config_bump_arr.as_ref()),
    ];
    let config_signer = Signer::from(&config_seeds);
    arlex_lang::system::create_account_with_minimum_balance_signed(
        ctx.accounts.earn_config,
        EarnConfig::SPACE,
        ctx.program_id,
        ctx.accounts.deployer,
        None,
        &[config_signer],
    )?;

    // --- Initialize EarnConfig ---
    // `mut` binding: field writes go through the guard's DerefMut. No CPI
    // follows this init (the account was created by the CPI above, before init).
    let mut config = EarnConfig::init(ctx.accounts.earn_config, ctx.program_id)?;
    config.total_invested_capital = 0;
    config.authority = authority;
    config.pending_authority = [0u8; 32];
    config.has_pending = false;
    config.mint_fee_bps = DEFAULT_MINT_FEE_BPS;
    // basket_vault is left zeroed at init: the program does NOT custody USDC.
    // The authority sets the external treasury account later via update_config.
    config.basket_vault = [0u8; 32];
    config.dao_fee_destination = dao_fee_destination;
    config
        .rwt_mint
        .copy_from_slice(ctx.accounts.rwt_mint.address().as_ref());
    config.usdc_mint = usdc_mint;
    config.min_mint_amount = MIN_MINT_AMOUNT;
    config.bump = config_bump;
    config.schema_version = 1;
    // `_reserved` is zero by construction (the System Program zeroes the new
    // account body; `init` writes only the discriminator), so no memset here.

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(EarnInitialized {
        authority,
        rwt_mint: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.rwt_mint.address().as_ref());
            arr
        },
        // basket_vault is unset at init (external treasury, set via
        // update_config). Emit zero for event-schema stability.
        basket_vault: [0u8; 32],
        dao_fee_destination,
        initial_nav: INITIAL_NAV,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Earn config initialized");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom_code(err: ProgramError) -> u32 {
        match err {
            ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    fn assert_err_code<T>(result: Result<T>, expected: EarnError) {
        let err = match result {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(custom_code(err), custom_code(ProgramError::from(expected)),);
    }

    #[test]
    fn bootstrap_inputs_require_pinned_deployer_and_nonzero_authority() {
        let authority = [1u8; 32];
        assert!(validate_bootstrap_inputs(BOOTSTRAP_AUTHORITY.as_ref(), authority,).is_ok());

        let wrong_deployer = [9u8; 32];
        assert_err_code(
            validate_bootstrap_inputs(&wrong_deployer, authority),
            EarnError::UnauthorizedBootstrap,
        );
        assert_err_code(
            validate_bootstrap_inputs(BOOTSTRAP_AUTHORITY.as_ref(), [0u8; 32]),
            EarnError::ZeroDestination,
        );
    }

    #[test]
    fn initialize_requires_fresh_rwt_mint_owned_by_config_pda() {
        let config_pda = [7u8; 32];
        assert!(validate_rwt_mint_for_initialize(
            Some(config_pda),
            0,
            RWT_DECIMALS,
            None,
            config_pda,
        )
        .is_ok());

        assert_err_code(
            validate_rwt_mint_for_initialize(Some([8u8; 32]), 0, RWT_DECIMALS, None, config_pda),
            EarnError::InvalidMintAuthority,
        );
        assert_err_code(
            validate_rwt_mint_for_initialize(Some(config_pda), 1, RWT_DECIMALS, None, config_pda),
            EarnError::InvalidMintSupply,
        );
        assert_err_code(
            validate_rwt_mint_for_initialize(
                Some(config_pda),
                0,
                RWT_DECIMALS + 1,
                None,
                config_pda,
            ),
            EarnError::InvalidMintDecimals,
        );
        assert_err_code(
            validate_rwt_mint_for_initialize(
                Some(config_pda),
                0,
                RWT_DECIMALS,
                Some([9u8; 32]),
                config_pda,
            ),
            EarnError::InvalidFreezeAuthority,
        );
    }

    #[test]
    fn initialize_pins_usdc_mint_and_dao_fee_currency() {
        // M1: usdc_mint must equal the canonical USDC_MINT constant; the dao fee
        // destination must hold USDC. basket_vault is NOT validated at init — it
        // is an external treasury account set later via update_config.
        let usdc_mint = USDC_MINT;
        assert!(validate_initialize_token_accounts(usdc_mint, usdc_mint).is_ok());

        // M1: a non-canonical deposit mint is rejected.
        assert_err_code(
            validate_initialize_token_accounts(usdc_mint, [1u8; 32]),
            EarnError::InvalidTokenAccount,
        );
        // dao_fee_destination not holding USDC is rejected.
        assert_err_code(
            validate_initialize_token_accounts([5u8; 32], usdc_mint),
            EarnError::InvalidFeeDestination,
        );
    }
}
