//! create_distributor — create a perpetual distributor for an OT project.
//!
//! Creates four accounts atomically:
//!   1. MerkleDistributor PDA   — seeds: ["merkle_dist", ot_mint]
//!   2. Reward Vault RWT ATA    — owner = distributor PDA (via associated_token::Create)
//!   3. Accumulator PDA         — seeds: ["accumulator", ot_mint]
//!   4. Accumulator USDC ATA    — owner = accumulator PDA (via associated_token::Create)

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::YdError;
use crate::events::DistributorCreated;
use crate::state::{Accumulator, DistributionConfig, MerkleDistributor};

#[derive(Accounts)]
pub struct CreateDistributor<'info> {
    #[account(mut, signer)]
    pub authority: &'info AccountView,

    #[account(
        has_one = authority, account_type = "DistributionConfig",
        seeds = [b"dist_config"], bump
    )]
    pub config: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    #[account(
        init, payer = authority, space = MerkleDistributor::SPACE,
        seeds = [b"merkle_dist", ot_mint.address().as_ref()], bump
    )]
    pub distributor: &'info AccountView,

    #[account(
        init, payer = authority, space = Accumulator::SPACE,
        seeds = [b"accumulator", ot_mint.address().as_ref()], bump
    )]
    pub accumulator: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub usdc_mint: &'info AccountView,

    // Reward vault (RWT ATA owned by distributor PDA) — created via CPI.
    #[account(mut)]
    pub reward_vault: &'info AccountView,

    // Accumulator USDC ATA (owned by accumulator PDA) — created via CPI.
    #[account(mut)]
    pub accumulator_usdc_ata: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,

    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,

    #[account(constraint = ata_program.address() == &Address::new_from_array(ASSOCIATED_TOKEN_PROGRAM))]
    pub ata_program: &'info AccountView,
}

pub fn handler(ctx: Context<CreateDistributor>, vesting_period_secs: i64) -> Result<()> {
    // --- Validate config (immutable read) ---
    let config = DistributionConfig::load(ctx.accounts.config, ctx.program_id)?;
    if !config.is_active {
        return Err(ProgramError::from(YdError::SystemPaused));
    }

    // --- Validate params ---
    if vesting_period_secs <= 0 {
        return Err(ProgramError::from(YdError::InvalidVestingPeriod));
    }
    if ctx.accounts.ot_mint.address().as_ref() == [0u8; 32].as_ref() {
        return Err(ProgramError::from(YdError::ZeroDestination));
    }

    // RWT/USDC mint pinning
    if ctx.accounts.rwt_mint.address().as_ref() != RWT_MINT.as_ref() {
        return Err(ProgramError::from(YdError::InvalidTokenAccount));
    }
    if ctx.accounts.usdc_mint.address().as_ref() != USDC_MINT.as_ref() {
        return Err(ProgramError::from(YdError::InvalidTokenAccount));
    }

    let ot_mint_bytes = {
        let mut a = [0u8; 32];
        a.copy_from_slice(ctx.accounts.ot_mint.address().as_ref());
        a
    };

    // --- Derive canonical bumps ---
    let (_, distributor_bump) = arlex_lang::find_program_address(
        &[b"merkle_dist", ctx.accounts.ot_mint.address().as_ref()],
        ctx.program_id,
    );
    let (_, accumulator_bump) = arlex_lang::find_program_address(
        &[b"accumulator", ctx.accounts.ot_mint.address().as_ref()],
        ctx.program_id,
    );

    let now = Clock::get()?.unix_timestamp;

    // --- Initialize MerkleDistributor ---
    // Scope the init so the distributor guard drops before the ATA Create CPI
    // below, which passes the distributor account as the ATA wallet. reward_vault
    // is written in a re-load_mut AFTER the CPIs (Pattern D).
    {
        let mut dist = MerkleDistributor::init(ctx.accounts.distributor, ctx.program_id)?;
        dist.ot_mint = ot_mint_bytes;
        // reward_vault / accumulator addresses copied after ATA creation
        dist.reward_vault = [0u8; 32];
        dist.accumulator.copy_from_slice(ctx.accounts.accumulator.address().as_ref());
        dist.merkle_root = [0u8; 32];
        dist.max_total_claim = 0;
        dist.total_claimed = 0;
        dist.total_funded = 0;
        dist.locked_vested = 0;
        dist.last_fund_ts = now;
        dist.vesting_period_secs = vesting_period_secs;
        dist.epoch = 0;
        dist.is_active = true;
        dist.bump = distributor_bump;
    } // distributor guard dropped — released before the reward-vault ATA Create.

    // --- Initialize Accumulator ---
    // Scope the init so the accumulator guard drops before its ATA Create CPI
    // below (which passes the accumulator account as the ATA wallet).
    {
        let mut acc = Accumulator::init(ctx.accounts.accumulator, ctx.program_id)?;
        acc.ot_mint = ot_mint_bytes;
        acc.bump = accumulator_bump;
    } // accumulator guard dropped — released before the accumulator ATA Create.

    // --- Create Reward Vault RWT ATA (wallet = distributor PDA) ---
    arlex_lang::associated_token::instructions::Create {
        funding_account: ctx.accounts.authority,
        account: ctx.accounts.reward_vault,
        wallet: ctx.accounts.distributor,
        mint: ctx.accounts.rwt_mint,
        system_program: ctx.accounts.system_program,
        token_program: ctx.accounts.token_program,
    }
    .invoke()?;

    // --- Create Accumulator USDC ATA (wallet = accumulator PDA) ---
    arlex_lang::associated_token::instructions::Create {
        funding_account: ctx.accounts.authority,
        account: ctx.accounts.accumulator_usdc_ata,
        wallet: ctx.accounts.accumulator,
        mint: ctx.accounts.usdc_mint,
        system_program: ctx.accounts.system_program,
        token_program: ctx.accounts.token_program,
    }
    .invoke()?;

    // Persist reward_vault address into distributor state — Pattern D: re-borrow
    // the distributor mut now that the ATA CPIs have returned.
    {
        let mut dist = MerkleDistributor::load_mut(ctx.accounts.distributor, ctx.program_id)?;
        dist.reward_vault
            .copy_from_slice(ctx.accounts.reward_vault.address().as_ref());
    }

    // --- Emit ---
    let reward_vault_bytes = {
        let mut a = [0u8; 32];
        a.copy_from_slice(ctx.accounts.reward_vault.address().as_ref());
        a
    };
    let accumulator_bytes = {
        let mut a = [0u8; 32];
        a.copy_from_slice(ctx.accounts.accumulator.address().as_ref());
        a
    };
    emit!(DistributorCreated {
        ot_mint: ot_mint_bytes,
        reward_vault: reward_vault_bytes,
        accumulator: accumulator_bytes,
        vesting_period_secs,
        timestamp: now,
    });

    arlex_lang::log("YD distributor created");
    Ok(())
}
