use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::OtError;
use crate::events::RevenueDistributed;
use crate::state::*;

#[derive(Accounts)]
pub struct DistributeRevenue<'info> {
    #[account(signer)]
    pub crank: &'info AccountView,

    // OT mint — for PDA seed derivation, validated as SPL Mint
    #[account(owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub ot_mint: &'info AccountView,

    // RevenueAccount PDA — validated via seeds
    #[account(
        mut,
        seeds = [b"revenue", ot_mint.address().as_ref()], bump
    )]
    pub revenue_account: &'info AccountView,

    // Revenue USDC ATA — owned by RevenueAccount PDA
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub revenue_token_account: &'info AccountView,

    // RevenueConfig PDA — validated via seeds
    #[account(seeds = [b"revenue_config", ot_mint.address().as_ref()], bump)]
    pub revenue_config: &'info AccountView,

    // Areal fee USDC account — manually validated against revenue_config.areal_fee_destination
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub areal_fee_account: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

pub fn handler(ctx: Context<DistributeRevenue>) -> Result<()> {
    let revenue = RevenueAccount::load_mut(ctx.accounts.revenue_account, ctx.program_id)?;

    // SECURITY (M-1/M-2): validate passed ATA matches the canonical one stored at init.
    // Prevents distribute from an unexpected token account that happens to be owned by the
    // revenue PDA (e.g. for a different mint).
    if ctx.accounts.revenue_token_account.address().as_ref()
        != revenue.revenue_token_account.as_ref()
    {
        return Err(ProgramError::from(OtError::InvalidTokenAccountOwner));
    }

    // Reentrancy guard — SPL Token CPIs don't call back, so the practical attack
    // surface is nil. If any Transfer below returns Err, Solana atomicity reverts
    // all state including `is_distributing`, so the flag does NOT get stuck.
    // Defense-in-depth only (H-1 audit finding).
    if revenue.is_distributing {
        return Err(ProgramError::from(OtError::DistributionInProgress));
    }
    revenue.is_distributing = true;

    // Read revenue ATA data: mint (0..32), owner (32..64), balance (64..72)
    // SPL Token Account layout: [mint: 32][owner: 32][amount: u64][...]
    let ata_data = unsafe {
        core::slice::from_raw_parts(
            ctx.accounts.revenue_token_account.data_ptr(),
            ctx.accounts.revenue_token_account.data_len(),
        )
    };
    if ata_data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    // Store mint for cross-validation with fee and destination accounts
    let revenue_ata_mint = &ata_data[0..32];
    // Token account owner must be the revenue_account PDA
    if &ata_data[32..64] != ctx.accounts.revenue_account.address().as_ref() {
        return Err(ProgramError::from(OtError::InvalidTokenAccountOwner));
    }
    let balance = u64::from_le_bytes(ata_data[64..72].try_into().unwrap());

    // Check minimum distribution amount
    if balance < revenue.min_distribution_amount {
        return Err(ProgramError::from(OtError::BelowMinDistribution));
    }

    // Check cooldown
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    if revenue.last_distribution_ts != 0
        && now.saturating_sub(revenue.last_distribution_ts) < DISTRIBUTION_COOLDOWN
    {
        return Err(ProgramError::from(OtError::DistributionCooldown));
    }

    // Load RevenueConfig (PDA already validated via seeds constraint)
    let config = RevenueConfig::load(ctx.accounts.revenue_config, ctx.program_id)?;

    let active_count = config.active_count as usize;
    if active_count == 0 {
        return Err(ProgramError::from(OtError::ActiveBpsSumMismatch));
    }

    // Verify active destinations BPS sum == 10,000
    let mut bps_sum: u64 = 0;
    for i in 0..active_count {
        bps_sum += config.destinations[i].allocation_bps as u64;
    }
    if bps_sum != BPS_DENOMINATOR {
        return Err(ProgramError::from(OtError::ActiveBpsSumMismatch));
    }

    // Validate areal_fee_account address matches config
    if ctx.accounts.areal_fee_account.address().as_ref() != config.areal_fee_destination.as_ref() {
        return Err(ProgramError::from(OtError::InvalidFeeAccount));
    }

    // Validate areal_fee_account mint matches revenue_token_account mint
    let fee_data = unsafe {
        core::slice::from_raw_parts(
            ctx.accounts.areal_fee_account.data_ptr(),
            ctx.accounts.areal_fee_account.data_len(),
        )
    };
    if fee_data.len() < 32 {
        return Err(ProgramError::InvalidAccountData);
    }
    if &fee_data[0..32] != revenue_ata_mint {
        return Err(ProgramError::from(OtError::TokenMintMismatch));
    }

    // Check remaining accounts
    if ctx.remaining_accounts.len() < active_count {
        return Err(ProgramError::from(OtError::InsufficientRemainingAccounts));
    }

    // Validate all destination accounts: address match, SPL ownership, mint match
    for i in 0..active_count {
        let dest = &config.destinations[i];
        let dest_account = &ctx.remaining_accounts[i];

        if dest_account.address().as_ref() != dest.address.as_ref() {
            return Err(ProgramError::from(OtError::DestinationAccountMismatch));
        }
        if !dest_account.owned_by(&Address::new_from_array(SPL_TOKEN_PROGRAM)) {
            return Err(ProgramError::from(OtError::InvalidTokenAccountOwner));
        }
        // Validate destination mint matches revenue ATA mint
        let dest_data = unsafe {
            core::slice::from_raw_parts(dest_account.data_ptr(), dest_account.data_len())
        };
        if dest_data.len() < 32 || &dest_data[0..32] != revenue_ata_mint {
            return Err(ProgramError::from(OtError::TokenMintMismatch));
        }
    }

    // Calculate protocol fee (ceiling division — in favor of protocol)
    let fee = arlex_lang::math::mul_div_u64_round_up(balance, AREAL_PROTOCOL_FEE_BPS, BPS_DENOMINATOR)
        .ok_or(ProgramError::from(OtError::MathOverflow))?;
    let post_fee_balance = balance.checked_sub(fee)
        .ok_or(ProgramError::from(OtError::MathOverflow))?;

    // --- EFFECTS: Update state BEFORE CPI (checks-effects-interactions) ---
    revenue.total_distributed = revenue.total_distributed
        .checked_add(balance)
        .ok_or(ProgramError::from(OtError::MathOverflow))?;
    revenue.distribution_count = revenue.distribution_count
        .checked_add(1)
        .ok_or(ProgramError::from(OtError::MathOverflow))?;
    revenue.last_distribution_ts = now;

    // PDA signer seeds for RevenueAccount
    let bump_bytes = [revenue.bump];

    // --- INTERACTIONS: Transfer protocol fee ---
    if fee > 0 {
        let seeds = [
            Seed::from(b"revenue" as &[u8]),
            Seed::from(revenue.ot_mint.as_ref()),
            Seed::from(bump_bytes.as_ref()),
        ];
        let signer = Signer::from(&seeds);
        arlex_lang::token::instructions::Transfer {
            from: ctx.accounts.revenue_token_account,
            to: ctx.accounts.areal_fee_account,
            authority: ctx.accounts.revenue_account,
            amount: fee,
        }.invoke_signed(&[signer])?;
    }

    // --- INTERACTIONS: Distribute remainder to destinations ---
    let mut remaining_amount = post_fee_balance;

    for i in 0..active_count {
        let dest = &config.destinations[i];
        let dest_account = &ctx.remaining_accounts[i];

        let share = if i == active_count - 1 {
            remaining_amount
        } else {
            let s = arlex_lang::math::mul_div_u64(post_fee_balance, dest.allocation_bps as u64, BPS_DENOMINATOR)
                .ok_or(ProgramError::from(OtError::MathOverflow))?;
            remaining_amount = remaining_amount.checked_sub(s)
                .ok_or(ProgramError::from(OtError::MathOverflow))?;
            s
        };

        if share > 0 {
            let seeds = [
                Seed::from(b"revenue" as &[u8]),
                Seed::from(revenue.ot_mint.as_ref()),
                Seed::from(bump_bytes.as_ref()),
            ];
            let signer = Signer::from(&seeds);
            arlex_lang::token::instructions::Transfer {
                from: ctx.accounts.revenue_token_account,
                to: dest_account,
                authority: ctx.accounts.revenue_account,
                amount: share,
            }.invoke_signed(&[signer])?;
        }
    }

    // Clear reentrancy guard
    revenue.is_distributing = false;

    // Emit event
    emit!(RevenueDistributed {
        ot_mint: revenue.ot_mint,
        total_amount: balance,
        protocol_fee: fee,
        distribution_count: revenue.distribution_count,
        num_destinations: active_count as u8,
        timestamp: now,
    });

    arlex_lang::log("Revenue distributed");
    Ok(())
}
