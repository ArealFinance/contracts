//! vault_swap — CPI to DEX::swap with RWT Vault PDA as signer.
//!
//! Manager-only instruction. Vault PDA signs the DEX swap via invoke_signed.
//! CPI account order MUST match DEX::swap exactly (verified against
//! contracts/native-dex/src/instructions/swap.rs).

use arlex_lang::prelude::*;
use pinocchio::instruction::{InstructionView, InstructionAccount};
use pinocchio::cpi::{invoke_signed, Seed, Signer};
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::RwtError;
use crate::events::VaultSwapExecuted;
use crate::state::RwtVault;
use crate::validation::read_token_account_mint;

const ZERO_PUBKEY: [u8; 32] = [0u8; 32];

#[derive(Accounts)]
pub struct VaultSwap<'info> {
    #[account(signer)]
    pub manager: &'info AccountView,

    #[account(mut, seeds = [b"rwt_vault"], bump)]
    pub rwt_vault: &'info AccountView,

    // Vault's ATA for input token (owner = rwt_vault PDA)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_token_in: &'info AccountView,

    // Vault's ATA for output token (owner = rwt_vault PDA)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_token_out: &'info AccountView,

    // DEX CPI accounts (passed through — validated by DEX program during CPI)
    pub dex_config: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub pool_vault_in: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub pool_vault_out: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub areal_fee_account: &'info AccountView,

    // remaining_accounts[0]: ot_treasury_fee_account (optional, if OT pair pool)

    pub dex_program: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

/// Read SPL Token Account `amount` field (bytes 64..72, little-endian u64).
fn read_token_account_amount(account: &AccountView) -> core::result::Result<u64, ProgramError> {
    let data = unsafe {
        core::slice::from_raw_parts(account.data_ptr(), account.data_len())
    };
    if data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&data[64..72]);
    Ok(u64::from_le_bytes(buf))
}

/// Read SPL Token Account `owner` field (bytes 32..64).
fn read_token_account_owner(account: &AccountView) -> core::result::Result<[u8; 32], ProgramError> {
    let data = unsafe {
        core::slice::from_raw_parts(account.data_ptr(), account.data_len())
    };
    if data.len() < 64 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut owner = [0u8; 32];
    owner.copy_from_slice(&data[32..64]);
    Ok(owner)
}

pub fn handler(
    ctx: Context<VaultSwap>,
    amount_in: u64,
    min_amount_out: u64,
    a_to_b: bool,
) -> Result<()> {
    let vault = RwtVault::load(ctx.accounts.rwt_vault, ctx.program_id)?;

    // --- Validation ---

    // Manager must be set (not zeroed)
    if vault.manager == ZERO_PUBKEY {
        return Err(ProgramError::from(RwtError::ManagerDisabled));
    }

    // Signer must be the manager
    if ctx.accounts.manager.address().as_ref() != vault.manager.as_ref() {
        return Err(ProgramError::from(RwtError::UnauthorizedManager));
    }

    // Amount checks
    if amount_in == 0 {
        return Err(ProgramError::from(RwtError::ZeroAmount));
    }
    if min_amount_out == 0 {
        return Err(ProgramError::from(RwtError::ZeroSlippage));
    }

    // DEX program ID validation
    if ctx.accounts.dex_program.address().as_ref() != DEX_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidDexProgram));
    }

    // Vault ATA ownership: both must be owned by the vault PDA
    let vault_pda_key = ctx.accounts.rwt_vault.address();
    let owner_in = read_token_account_owner(ctx.accounts.vault_token_in)?;
    if owner_in.as_ref() != vault_pda_key.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidVaultTokenOwner));
    }
    let owner_out = read_token_account_owner(ctx.accounts.vault_token_out)?;
    if owner_out.as_ref() != vault_pda_key.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidVaultTokenOwner));
    }

    // Same-ATA check
    if ctx.accounts.vault_token_in.address().as_ref() == ctx.accounts.vault_token_out.address().as_ref() {
        return Err(ProgramError::from(RwtError::SameTokenAccount));
    }

    // --- Snapshot balance before ---
    let balance_out_before = read_token_account_amount(ctx.accounts.vault_token_out)?;

    // --- Build CPI instruction data ---
    // swap args: discriminator(8) + amount_in(u64) + min_amount_out(u64) + a_to_b(bool)
    let mut instruction_data = [0u8; 25]; // 8 + 8 + 8 + 1
    instruction_data[0..8].copy_from_slice(&DISC_DEX_SWAP);
    instruction_data[8..16].copy_from_slice(&amount_in.to_le_bytes());
    instruction_data[16..24].copy_from_slice(&min_amount_out.to_le_bytes());
    instruction_data[24] = if a_to_b { 1 } else { 0 };

    // --- Build CPI account list ---
    // DEX::swap account order (verified against contracts/native-dex/src/instructions/swap.rs):
    // 0. user (signer)           — rwt_vault PDA
    // 1. dex_config
    // 2. pool_state (mut)
    // 3. user_token_in (mut)     — vault's ATA
    // 4. user_token_out (mut)    — vault's ATA
    // 5. vault_in (mut)          — pool vault
    // 6. vault_out (mut)         — pool vault
    // 7. areal_fee_account (mut)
    // 8. token_program

    let has_remaining = !ctx.remaining_accounts.is_empty();

    // Build accounts array — 9 named accounts + optional OT treasury
    let mut accounts_buf = [
        InstructionAccount::new(ctx.accounts.rwt_vault.address(), false, true),       // 0: user (signer via PDA)
        InstructionAccount::new(ctx.accounts.dex_config.address(), false, false),      // 1: dex_config
        InstructionAccount::new(ctx.accounts.pool_state.address(), true, false),       // 2: pool_state (mut)
        InstructionAccount::new(ctx.accounts.vault_token_in.address(), true, false),   // 3: user_token_in (mut)
        InstructionAccount::new(ctx.accounts.vault_token_out.address(), true, false),  // 4: user_token_out (mut)
        InstructionAccount::new(ctx.accounts.pool_vault_in.address(), true, false),    // 5: vault_in (mut)
        InstructionAccount::new(ctx.accounts.pool_vault_out.address(), true, false),   // 6: vault_out (mut)
        InstructionAccount::new(ctx.accounts.areal_fee_account.address(), true, false),// 7: areal_fee_account (mut)
        InstructionAccount::new(ctx.accounts.token_program.address(), false, false),   // 8: token_program
        // Slot 9 used if OT treasury present — filled below
        InstructionAccount::new(ctx.accounts.token_program.address(), false, false),   // placeholder
    ];

    let cpi_account_count: usize;
    if has_remaining {
        accounts_buf[9] = InstructionAccount::new(ctx.remaining_accounts[0].address(), true, false);
        cpi_account_count = 10;
    } else {
        cpi_account_count = 9;
    }

    let instruction = InstructionView {
        program_id: ctx.accounts.dex_program.address(),
        data: &instruction_data,
        accounts: &accounts_buf[..cpi_account_count],
    };

    // --- PDA signer ---
    let bump = [vault.bump];
    let seeds = [
        Seed::from(b"rwt_vault" as &[u8]),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    // --- Invoke CPI ---
    // Account views passed to invoke_signed: CPI accounts + dex_program
    if has_remaining {
        invoke_signed::<12>(
            &instruction,
            &[
                ctx.accounts.rwt_vault,       // 0: user (vault PDA)
                ctx.accounts.dex_config,      // 1: dex_config
                ctx.accounts.pool_state,      // 2: pool_state
                ctx.accounts.vault_token_in,  // 3: user_token_in
                ctx.accounts.vault_token_out, // 4: user_token_out
                ctx.accounts.pool_vault_in,   // 5: vault_in
                ctx.accounts.pool_vault_out,  // 6: vault_out
                ctx.accounts.areal_fee_account, // 7: areal_fee_account
                ctx.accounts.token_program,   // 8: token_program
                &ctx.remaining_accounts[0],   // 9: ot_treasury_fee_account
                ctx.accounts.dex_program,     // program
                ctx.accounts.manager,         // pass manager for CPI account resolution
            ],
            &[signer],
        )?;
    } else {
        invoke_signed::<11>(
            &instruction,
            &[
                ctx.accounts.rwt_vault,
                ctx.accounts.dex_config,
                ctx.accounts.pool_state,
                ctx.accounts.vault_token_in,
                ctx.accounts.vault_token_out,
                ctx.accounts.pool_vault_in,
                ctx.accounts.pool_vault_out,
                ctx.accounts.areal_fee_account,
                ctx.accounts.token_program,
                ctx.accounts.dex_program,
                ctx.accounts.manager,
            ],
            &[signer],
        )?;
    }

    // --- Snapshot balance after ---
    let balance_out_after = read_token_account_amount(ctx.accounts.vault_token_out)?;
    let actual_out = balance_out_after.checked_sub(balance_out_before)
        .unwrap_or(0);

    // --- Read mint fields for event ---
    let token_in_mint = read_token_account_mint(ctx.accounts.vault_token_in)?;
    let token_out_mint = read_token_account_mint(ctx.accounts.vault_token_out)?;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(VaultSwapExecuted {
        token_in_mint,
        token_out_mint,
        amount_in,
        amount_out: actual_out,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}
