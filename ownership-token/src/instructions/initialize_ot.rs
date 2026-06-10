use arlex_lang::prelude::*;
use arlex_lang::token::instructions::{SetAuthority, AuthorityType};
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::OtError;
use crate::events::OtInitialized;
use crate::state::*;

#[derive(Accounts)]
pub struct InitializeOt<'info> {
    #[account(mut, signer)]
    pub deployer: &'info AccountView,

    // Existing SPL mint (vanity address OK)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    // USDC mint — must be owned by SPL Token Program
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub usdc_mint: &'info AccountView,

    #[account(
        init, payer = deployer, space = OtConfig::SPACE,
        seeds = [b"ot_config", ot_mint.address().as_ref()], bump
    )]
    pub ot_config: &'info AccountView,

    #[account(
        init, payer = deployer, space = RevenueAccount::SPACE,
        seeds = [b"revenue", ot_mint.address().as_ref()], bump
    )]
    pub revenue_account: &'info AccountView,

    // Revenue USDC ATA — created via CPI, validated by ATA program
    #[account(mut)]
    pub revenue_token_account: &'info AccountView,

    #[account(
        init, payer = deployer, space = RevenueConfig::SPACE,
        seeds = [b"revenue_config", ot_mint.address().as_ref()], bump
    )]
    pub revenue_config: &'info AccountView,

    #[account(
        init, payer = deployer, space = OtGovernance::SPACE,
        seeds = [b"ot_governance", ot_mint.address().as_ref()], bump
    )]
    pub ot_governance: &'info AccountView,

    #[account(
        init, payer = deployer, space = OtTreasury::SPACE,
        seeds = [b"ot_treasury", ot_mint.address().as_ref()], bump
    )]
    pub ot_treasury: &'info AccountView,

    // Areal fee destination — validated as real SPL Token Account at init time
    // Prevents deployer from accidentally setting an invalid address (permanently bricking distributions)
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub areal_fee_destination_account: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,

    #[account(constraint = ata_program.address() == &Address::new_from_array(ASSOCIATED_TOKEN_PROGRAM))]
    pub ata_program: &'info AccountView,
}

pub fn handler(
    ctx: Context<InitializeOt>,
    name: [u8; 32],
    symbol: [u8; 10],
    uri: [u8; 200],
    initial_authority: [u8; 32],
) -> Result<()> {
    // --- Compute canonical bumps via find_program_address ---
    let ot_mint_ref = ctx.accounts.ot_mint.address().as_ref();
    let (_, ot_config_bump) = arlex_lang::find_program_address(
        &[b"ot_config", ot_mint_ref], ctx.program_id,
    );
    let (_, revenue_bump) = arlex_lang::find_program_address(
        &[b"revenue", ot_mint_ref], ctx.program_id,
    );
    let (_, revenue_config_bump) = arlex_lang::find_program_address(
        &[b"revenue_config", ot_mint_ref], ctx.program_id,
    );
    let (_, governance_bump) = arlex_lang::find_program_address(
        &[b"ot_governance", ot_mint_ref], ctx.program_id,
    );
    let (_, treasury_bump) = arlex_lang::find_program_address(
        &[b"ot_treasury", ot_mint_ref], ctx.program_id,
    );

    // --- Validate mint ---
    let mint_data = unsafe {
        core::slice::from_raw_parts(
            ctx.accounts.ot_mint.data_ptr(),
            ctx.accounts.ot_mint.data_len(),
        )
    };
    if mint_data.len() < 82 {
        return Err(ProgramError::InvalidAccountData);
    }

    // supply (offset 36..44) must be 0
    let supply = u64::from_le_bytes(mint_data[36..44].try_into().unwrap());
    if supply != 0 {
        return Err(ProgramError::from(OtError::InvalidMintSupply));
    }

    // decimals (offset 44)
    let decimals = mint_data[44];
    if decimals == 0 || decimals > MAX_DECIMALS {
        return Err(ProgramError::from(OtError::InvalidDecimals));
    }

    // mint_authority: COption<Pubkey> at offset 0..36
    let mint_auth_tag = u32::from_le_bytes(mint_data[0..4].try_into().unwrap());
    if mint_auth_tag != 1 {
        return Err(ProgramError::from(OtError::InvalidMintAuthority));
    }
    if &mint_data[4..36] != ctx.accounts.deployer.address().as_ref() {
        return Err(ProgramError::from(OtError::InvalidMintAuthority));
    }

    // freeze_authority: COption<Pubkey> at offset 46..82
    let freeze_auth_tag = u32::from_le_bytes(mint_data[46..50].try_into().unwrap());
    if freeze_auth_tag != 0 {
        return Err(ProgramError::from(OtError::FreezeAuthoritySet));
    }

    // Validate name and symbol are non-empty
    if name.iter().all(|&b| b == 0) {
        return Err(ProgramError::from(OtError::InvalidName));
    }
    if symbol.iter().all(|&b| b == 0) {
        return Err(ProgramError::from(OtError::InvalidSymbol));
    }

    // Validate initial_authority is not zero address (would permanently lock governance)
    if initial_authority == [0u8; 32] {
        return Err(ProgramError::from(OtError::InvalidInitialAuthority));
    }

    // Read areal_fee_destination from the validated account
    let mut areal_fee_destination = [0u8; 32];
    areal_fee_destination.copy_from_slice(ctx.accounts.areal_fee_destination_account.address().as_ref());

    // Each init is scoped to its own block so the RAII guard drops immediately
    // after the writes — releasing every borrow flag before the CPIs below. In
    // particular the Revenue USDC ATA Create CPI passes revenue_account as the
    // ATA wallet, which the checked invoke would reject while that account is
    // still mutably borrowed. No init account is written after the CPIs, so no
    // re-load is needed.

    // --- Initialize OtConfig (canonical bump from find_program_address) ---
    {
        let mut ot_config = OtConfig::init(ctx.accounts.ot_config, ctx.program_id)?;
        ot_config.ot_mint.copy_from_slice(ot_mint_ref);
        ot_config.name = name;
        ot_config.symbol = symbol;
        ot_config.decimals = decimals;
        ot_config.total_minted = 0;
        ot_config.uri = uri;
        ot_config.bump = ot_config_bump;
    }

    // --- Initialize RevenueAccount ---
    {
        let mut revenue = RevenueAccount::init(ctx.accounts.revenue_account, ctx.program_id)?;
        revenue.ot_mint.copy_from_slice(ot_mint_ref);
        revenue.revenue_token_account.copy_from_slice(
            ctx.accounts.revenue_token_account.address().as_ref()
        );
        revenue.total_distributed = 0;
        revenue.distribution_count = 0;
        revenue.last_distribution_ts = 0;
        revenue.min_distribution_amount = MIN_DISTRIBUTION_AMOUNT;
        revenue.is_distributing = false;
        revenue.bump = revenue_bump;
    }

    // --- Initialize RevenueConfig ---
    {
        let mut rev_config = RevenueConfig::init(ctx.accounts.revenue_config, ctx.program_id)?;
        rev_config.ot_mint.copy_from_slice(ot_mint_ref);
        for i in 0..MAX_DESTINATIONS {
            rev_config.destinations[i] = RevenueDestination::zeroed();
        }
        rev_config.active_count = 0;
        rev_config.config_version = 0;
        rev_config.areal_fee_destination = areal_fee_destination;
        rev_config.bump = revenue_config_bump;
    }

    // --- Initialize OtGovernance ---
    {
        let mut governance = OtGovernance::init(ctx.accounts.ot_governance, ctx.program_id)?;
        governance.ot_mint.copy_from_slice(ot_mint_ref);
        governance.authority = initial_authority;
        governance.pending_authority = [0u8; 32];
        governance.has_pending = false;
        governance.is_active = true;
        governance.bump = governance_bump;
    }

    // --- Initialize OtTreasury ---
    {
        let mut treasury = OtTreasury::init(ctx.accounts.ot_treasury, ctx.program_id)?;
        treasury.ot_mint.copy_from_slice(ot_mint_ref);
        treasury.bump = treasury_bump;
    }

    // --- Create Revenue USDC ATA (owned by RevenueAccount PDA) ---
    arlex_lang::associated_token::instructions::Create {
        funding_account: ctx.accounts.deployer,
        account: ctx.accounts.revenue_token_account,
        wallet: ctx.accounts.revenue_account,
        mint: ctx.accounts.usdc_mint,
        system_program: ctx.accounts.system_program,
        token_program: ctx.accounts.token_program,
    }.invoke()?;

    // --- Transfer mint authority from deployer to OtConfig PDA (LAST CPI) ---
    let ot_config_address = {
        let (addr, _) = arlex_lang::find_program_address(
            &[b"ot_config", ot_mint_ref], ctx.program_id,
        );
        addr
    };

    SetAuthority {
        account: ctx.accounts.ot_mint,
        authority: ctx.accounts.deployer,
        authority_type: AuthorityType::MintTokens,
        new_authority: Some(&ot_config_address),
    }.invoke()?;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(OtInitialized {
        ot_mint: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ot_mint_ref);
            arr
        },
        authority: initial_authority,
        decimals,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("OT initialized");
    Ok(())
}
