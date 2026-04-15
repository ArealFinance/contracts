use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::PoolCreated;
use crate::state::*;
use crate::validation::*;

#[derive(Accounts)]
pub struct CreatePool<'info> {
    #[account(mut, signer)]
    pub creator: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    #[account(seeds = [b"pool_creators"], bump)]
    pub pool_creators: &'info AccountView,

    // Pool PDA: ["pool", token_a_mint, token_b_mint]
    #[account(mut)]
    pub pool_state: &'info AccountView,

    // Token mints (for PDA derivation and validation)
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_a_mint: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_b_mint: &'info AccountView,

    // Vault keypair accounts (created via CPI)
    #[account(mut, signer)]
    pub vault_a: &'info AccountView,

    #[account(mut, signer)]
    pub vault_b: &'info AccountView,

    // OT treasury PDA + RWT ATA for OT pairs — passed as remaining_accounts[0..2]

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,

    // Rent sysvar needed for InitializeAccount3 doesn't need it, but we keep system_program
}

pub fn handler(ctx: Context<CreatePool>) -> Result<()> {
    let config = DexConfig::load(ctx.accounts.dex_config, ctx.program_id)?;

    // --- Check DEX is active ---
    if !config.is_active {
        return Err(ProgramError::from(DexError::DexPaused));
    }

    // --- Validate creator is whitelisted ---
    let creators = PoolCreators::load(ctx.accounts.pool_creators, ctx.program_id)?;
    let creator_key = pubkey_bytes(ctx.accounts.creator);
    let mut found = false;
    for i in 0..creators.active_count as usize {
        if creators.creators[i] == creator_key {
            found = true;
            break;
        }
    }
    if !found {
        return Err(ProgramError::from(DexError::CreatorNotWhitelisted));
    }

    // --- Validate mint ordering and RWT presence ---
    let mint_a = pubkey_bytes(ctx.accounts.token_a_mint);
    let mint_b = pubkey_bytes(ctx.accounts.token_b_mint);

    if mint_a == mint_b {
        return Err(ProgramError::from(DexError::IdenticalMints));
    }
    if mint_a >= mint_b {
        return Err(ProgramError::from(DexError::InvalidMintOrder));
    }

    // One of the mints must be RWT
    let _a_is_rwt = token_a_is_rwt(&mint_a, &mint_b)?;

    // --- Create Pool PDA ---
    let (pool_pda, pool_bump) = arlex_lang::find_program_address(
        &[b"pool", mint_a.as_ref(), mint_b.as_ref()],
        ctx.program_id,
    );

    // Verify the passed pool_state matches the expected PDA
    if ctx.accounts.pool_state.address().as_ref() != pool_pda.as_ref() {
        return Err(ProgramError::InvalidSeeds);
    }

    // Allocate pool_state account via System Program CPI
    let rent = pinocchio::sysvars::rent::Rent::get()?;
    let lamports = rent.minimum_balance(PoolState::SPACE);
    arlex_lang::system::instructions::CreateAccount {
        from: ctx.accounts.creator,
        to: ctx.accounts.pool_state,
        lamports,
        space: PoolState::SPACE as u64,
        owner: ctx.program_id,
    }.invoke_signed(&[Signer::from(&[
        Seed::from(b"pool" as &[u8]),
        Seed::from(mint_a.as_ref()),
        Seed::from(mint_b.as_ref()),
        Seed::from(&[pool_bump]),
    ])])?;

    // --- Create vault token accounts (keypair-based, authority = pool PDA) ---
    let vault_rent = rent.minimum_balance(165); // SPL Token Account = 165 bytes

    // Vault A
    arlex_lang::system::instructions::CreateAccount {
        from: ctx.accounts.creator,
        to: ctx.accounts.vault_a,
        lamports: vault_rent,
        space: 165,
        owner: &Address::new_from_array(SPL_TOKEN_PROGRAM),
    }.invoke()?;

    arlex_lang::token::instructions::InitializeAccount3 {
        account: ctx.accounts.vault_a,
        mint: ctx.accounts.token_a_mint,
        owner: &pool_pda,
    }.invoke()?;

    // Vault B
    arlex_lang::system::instructions::CreateAccount {
        from: ctx.accounts.creator,
        to: ctx.accounts.vault_b,
        lamports: vault_rent,
        space: 165,
        owner: &Address::new_from_array(SPL_TOKEN_PROGRAM),
    }.invoke()?;

    arlex_lang::token::instructions::InitializeAccount3 {
        account: ctx.accounts.vault_b,
        mint: ctx.accounts.token_b_mint,
        owner: &pool_pda,
    }.invoke()?;

    // --- OT Treasury detection (optional remaining accounts) ---
    let mut ot_treasury_fee_destination = [0u8; 32];
    let mut has_ot_treasury = false;

    if ctx.remaining_accounts.len() >= 2 {
        let ot_treasury = &ctx.remaining_accounts[0];
        let ot_treasury_rwt_ata = &ctx.remaining_accounts[1];

        // Determine which mint is OT (the non-RWT mint)
        let ot_mint = if is_rwt_mint(&mint_a) { &mint_b } else { &mint_a };

        // Verify OT Treasury PDA derivation
        let (expected_treasury_pda, _) = arlex_lang::find_program_address(
            &[b"ot_treasury", ot_mint.as_ref()],
            &Address::new_from_array(OT_PROGRAM_ID),
        );
        if ot_treasury.address().as_ref() != expected_treasury_pda.as_ref() {
            return Err(ProgramError::from(DexError::InvalidOtTreasuryDestination));
        }

        // Verify OT Treasury is owned by OT_PROGRAM_ID
        if unsafe { ot_treasury.owner() }.as_ref() != OT_PROGRAM_ID.as_ref() {
            return Err(ProgramError::from(DexError::InvalidOtTreasuryDestination));
        }

        // Verify ot_treasury_rwt_ata is the correct ATA:
        // ATA = find_program_address([wallet, token_program, mint], ATA_PROGRAM)
        let (expected_ata, _) = arlex_lang::find_program_address(
            &[
                expected_treasury_pda.as_ref(),
                SPL_TOKEN_PROGRAM.as_ref(),
                RWT_MINT.as_ref(),
            ],
            &Address::new_from_array(ASSOCIATED_TOKEN_PROGRAM),
        );
        if ot_treasury_rwt_ata.address().as_ref() != expected_ata.as_ref() {
            return Err(ProgramError::from(DexError::InvalidOtTreasuryDestination));
        }

        ot_treasury_fee_destination.copy_from_slice(ot_treasury_rwt_ata.address().as_ref());
        has_ot_treasury = true;
    }

    // --- Initialize PoolState ---
    let pool = PoolState::init(ctx.accounts.pool_state, ctx.program_id)?;
    pool.pool_type = POOL_TYPE_STANDARD;
    pool.token_a_mint = mint_a;
    pool.token_b_mint = mint_b;
    pool.vault_a = pubkey_bytes(ctx.accounts.vault_a);
    pool.vault_b = pubkey_bytes(ctx.accounts.vault_b);
    pool.reserve_a = 0;
    pool.reserve_b = 0;
    pool.total_lp_shares = 0;
    pool.fee_bps = config.base_fee_bps; // immutable after creation
    pool.is_active = true;
    pool.total_fees_accumulated = 0;
    pool.bin_step_bps = 0; // StandardCurve
    pool.active_bin_id = 0; // StandardCurve
    pool.ot_treasury_fee_destination = ot_treasury_fee_destination;
    pool.has_ot_treasury = has_ot_treasury;
    pool.bump = pool_bump;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(PoolCreated {
        pool: pubkey_bytes(ctx.accounts.pool_state),
        token_a_mint: mint_a,
        token_b_mint: mint_b,
        pool_type: POOL_TYPE_STANDARD,
        creator: creator_key,
        ot_treasury_fee_destination,
        timestamp: clock.unix_timestamp,
    });

    arlex_lang::log("Pool created");
    Ok(())
}
