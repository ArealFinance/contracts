//! `initialize` — one-time bootstrap of the `staking` program.
//!
//! Creates the singleton `StakingConfig` PDA at seed `["staking_config"]`,
//! initializes the stRWT mint (mint authority = StakingConfig PDA), and
//! creates the RWT pool vault (an RWT ATA owned by the StakingConfig PDA).
//!
//! Per staking.mdx §"initialize constraints":
//!   - `authority` MUST be the pinned bootstrap authority.
//!   - `rwt_mint` MUST be the canonical earn-RWT mint.
//!   - `strwt_mint` created with mint authority = StakingConfig PDA.
//!   - `pool_vault` created as an RWT ATA owned by the StakingConfig PDA.

use arlex_lang::prelude::*;
use pinocchio::cpi::{Seed, Signer};
use pinocchio::sysvars::{clock::Clock, rent::Rent, Sysvar};

use crate::constants::*;
use crate::error::StakingError;
use crate::events::StakingInitialized;
use crate::state::StakingConfig;
use crate::token::{read_token_account_amount, read_token_account_mint, read_token_account_owner};

#[derive(Accounts)]
pub struct Initialize<'info> {
    /// Deployer / initial authority. Pays rent for the config PDA + stRWT mint.
    #[account(mut, signer)]
    pub authority: &'info AccountView,

    /// StakingConfig PDA — created manually after bootstrap validation.
    #[account(mut, seeds = [STAKING_CONFIG_SEED], bump)]
    pub staking_config: &'info AccountView,

    /// Staked token: the earn-RWT mint. Read-only; pinned into config.
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    /// stRWT share mint — created here as a deployer-signed keypair, then
    /// InitializeMint2 sets the mint authority to the StakingConfig PDA.
    #[account(mut, signer)]
    pub strwt_mint: &'info AccountView,

    /// RWT pool vault — RWT ATA owned by the StakingConfig PDA. Created via
    /// the Associated Token Program CPI below.
    #[account(mut)]
    pub pool_vault: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,

    #[account(constraint = ata_program.address() == &Address::new_from_array(ASSOCIATED_TOKEN_PROGRAM))]
    pub ata_program: &'info AccountView,
}

fn validate_initialize_inputs(
    deployer: &[u8],
    reward_depositor: [u8; 32],
    rwt_mint: [u8; 32],
) -> Result<()> {
    if deployer != BOOTSTRAP_AUTHORITY.as_ref() {
        return Err(ProgramError::from(StakingError::UnauthorizedBootstrap));
    }
    if reward_depositor == [0u8; 32] {
        return Err(ProgramError::from(StakingError::ZeroAddress));
    }
    if rwt_mint == [0u8; 32] || rwt_mint != EARN_RWT_MINT {
        return Err(ProgramError::from(StakingError::InvalidRwtMint));
    }

    Ok(())
}

fn validate_pool_vault_account(
    owner_program: [u8; 32],
    pool_mint: [u8; 32],
    pool_owner: [u8; 32],
    pool_amount: u64,
    rwt_mint: [u8; 32],
    config_pda: [u8; 32],
) -> Result<u64> {
    if owner_program != SPL_TOKEN_PROGRAM || pool_mint != rwt_mint || pool_owner != config_pda {
        return Err(ProgramError::from(StakingError::InvalidTokenAccount));
    }

    Ok(pool_amount)
}

pub fn handler(ctx: Context<Initialize>, reward_depositor: [u8; 32]) -> Result<()> {
    // --- Validate inputs before any CPI / account initialization ---
    let mut rwt_mint = [0u8; 32];
    rwt_mint.copy_from_slice(ctx.accounts.rwt_mint.address().as_ref());
    validate_initialize_inputs(
        ctx.accounts.authority.address().as_ref(),
        reward_depositor,
        rwt_mint,
    )?;

    // --- Canonical config bump ---
    let (config_address, config_bump) =
        arlex_lang::find_program_address(&[STAKING_CONFIG_SEED], ctx.program_id);
    let mut config_pda = [0u8; 32];
    config_pda.copy_from_slice(config_address.as_ref());

    // --- Create the config PDA after all bootstrap gates pass ---
    let rent = Rent::get()?;
    let config_bump_arr = [config_bump];
    let config_seeds = [
        Seed::from(STAKING_CONFIG_SEED),
        Seed::from(config_bump_arr.as_ref()),
    ];
    let config_signer = Signer::from(&config_seeds);
    arlex_lang::system::create_account_with_minimum_balance_signed(
        ctx.accounts.staking_config,
        StakingConfig::SPACE,
        ctx.program_id,
        ctx.accounts.authority,
        None,
        &[config_signer],
    )?;

    // --- Create the stRWT mint (82 bytes, SPL Token owner) ---
    let mint_lamports = rent.try_minimum_balance(82)?;
    arlex_lang::system::instructions::CreateAccount {
        from: ctx.accounts.authority,
        to: ctx.accounts.strwt_mint,
        lamports: mint_lamports,
        space: 82,
        owner: &Address::new_from_array(SPL_TOKEN_PROGRAM),
    }
    .invoke()?;

    // Mint authority = StakingConfig PDA; no freeze authority.
    arlex_lang::token::instructions::InitializeMint2 {
        mint: ctx.accounts.strwt_mint,
        decimals: STRWT_DECIMALS,
        mint_authority: &config_address,
        freeze_authority: None,
    }
    .invoke()?;

    // --- Create or accept the RWT pool vault ATA (owned by StakingConfig PDA) ---
    arlex_lang::associated_token::instructions::CreateIdempotent {
        funding_account: ctx.accounts.authority,
        account: ctx.accounts.pool_vault,
        wallet: ctx.accounts.staking_config,
        mint: ctx.accounts.rwt_mint,
        system_program: ctx.accounts.system_program,
        token_program: ctx.accounts.token_program,
    }
    .invoke()?;

    let mut pool_owner_program = [0u8; 32];
    {
        // SAFETY: `owner()` returns a transient reference into account
        // metadata. We only compare the bytes immediately and never store it.
        let owner_ref = unsafe { ctx.accounts.pool_vault.owner() };
        pool_owner_program.copy_from_slice(owner_ref.as_ref());
    }
    // SECURITY (L2): a PRE-FUNDED pool vault at bootstrap is intentional, not a
    // bug. Per staking.mdx §"initialize constraints", the vault's current RWT
    // balance is recorded as the initial `total_rwt_active`, preserving the
    // invariant `pool_vault.balance == active + reserved` from genesis. Only the
    // pinned BOOTSTRAP_AUTHORITY (validated above) can call this, so seeding a
    // non-empty pool is a trusted deployer action. We therefore do NOT require an
    // empty vault here (unlike earn, which mints against a derived NAV and must
    // start empty to avoid first-minter capture). The stake/unstake rate uses
    // explicit counters + virtual offsets, so a non-zero genesis active balance
    // does not create a capture opportunity.
    let initial_rwt_active = validate_pool_vault_account(
        pool_owner_program,
        read_token_account_mint(ctx.accounts.pool_vault)?,
        read_token_account_owner(ctx.accounts.pool_vault)?,
        read_token_account_amount(ctx.accounts.pool_vault)?,
        rwt_mint,
        config_pda,
    )?;

    // --- Initialize StakingConfig ---
    // `mut` binding: field writes go through the guard's DerefMut. No CPI
    // follows this init (the account was created by the CPI above, before init).
    let mut config = StakingConfig::init(ctx.accounts.staking_config, ctx.program_id)?;
    config
        .authority
        .copy_from_slice(ctx.accounts.authority.address().as_ref());
    config.pending_authority = [0u8; 32];
    config.has_pending = false;
    config.rwt_mint = rwt_mint;
    config
        .strwt_mint
        .copy_from_slice(ctx.accounts.strwt_mint.address().as_ref());
    config.reward_depositor = reward_depositor;
    config
        .pool_vault
        .copy_from_slice(ctx.accounts.pool_vault.address().as_ref());
    config.total_rwt_active = initial_rwt_active;
    config.total_rwt_reserved = 0;
    config.cooldown_seconds = COOLDOWN_SECONDS;
    config.min_stake_amount = MIN_STAKE_AMOUNT;
    config.bump = config_bump;

    // --- Emit event ---
    let clock = Clock::get()?;
    let mut authority_arr = [0u8; 32];
    authority_arr.copy_from_slice(ctx.accounts.authority.address().as_ref());
    let mut strwt_mint_arr = [0u8; 32];
    strwt_mint_arr.copy_from_slice(ctx.accounts.strwt_mint.address().as_ref());
    let mut pool_vault_arr = [0u8; 32];
    pool_vault_arr.copy_from_slice(ctx.accounts.pool_vault.address().as_ref());

    emit!(StakingInitialized {
        authority: authority_arr,
        rwt_mint,
        strwt_mint: strwt_mint_arr,
        pool_vault: pool_vault_arr,
        cooldown_seconds: config.cooldown_seconds,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Staking initialized");
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

    fn assert_err_code<T>(result: Result<T>, expected: StakingError) {
        let err = match result {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(custom_code(err), custom_code(ProgramError::from(expected)),);
    }

    #[test]
    fn initialize_rejects_unpinned_bootstrap_authority_first() {
        assert_err_code(
            validate_initialize_inputs(&[9u8; 32], [2u8; 32], EARN_RWT_MINT),
            StakingError::UnauthorizedBootstrap,
        );
    }

    #[test]
    fn initialize_rejects_zero_role_addresses() {
        assert_err_code(
            validate_initialize_inputs(BOOTSTRAP_AUTHORITY.as_ref(), [0u8; 32], EARN_RWT_MINT),
            StakingError::ZeroAddress,
        );
    }

    #[test]
    fn initialize_rejects_non_canonical_rwt_mint() {
        assert_err_code(
            validate_initialize_inputs(BOOTSTRAP_AUTHORITY.as_ref(), [2u8; 32], [3u8; 32]),
            StakingError::InvalidRwtMint,
        );
        assert_err_code(
            validate_initialize_inputs(BOOTSTRAP_AUTHORITY.as_ref(), [2u8; 32], [0u8; 32]),
            StakingError::InvalidRwtMint,
        );
    }

    #[test]
    fn initialize_requires_pool_vault_for_rwt_and_config_pda() {
        let rwt_mint = EARN_RWT_MINT;
        let config_pda = [7u8; 32];
        assert_eq!(
            validate_pool_vault_account(
                SPL_TOKEN_PROGRAM,
                rwt_mint,
                config_pda,
                42,
                rwt_mint,
                config_pda,
            )
            .unwrap(),
            42,
        );

        assert_err_code(
            validate_pool_vault_account([9u8; 32], rwt_mint, config_pda, 0, rwt_mint, config_pda),
            StakingError::InvalidTokenAccount,
        );
        assert_err_code(
            validate_pool_vault_account(
                SPL_TOKEN_PROGRAM,
                [3u8; 32],
                config_pda,
                0,
                rwt_mint,
                config_pda,
            ),
            StakingError::InvalidTokenAccount,
        );
        assert_err_code(
            validate_pool_vault_account(
                SPL_TOKEN_PROGRAM,
                rwt_mint,
                [4u8; 32],
                0,
                rwt_mint,
                config_pda,
            ),
            StakingError::InvalidTokenAccount,
        );
    }

    #[cfg(feature = "devnet")]
    #[test]
    fn initialize_accepts_devnet_bootstrap_and_canonical_rwt_mint() {
        assert!(
            validate_initialize_inputs(BOOTSTRAP_AUTHORITY.as_ref(), [2u8; 32], EARN_RWT_MINT,)
                .is_ok()
        );
    }
}
