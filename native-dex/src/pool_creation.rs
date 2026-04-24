//! Shared helpers for `create_pool` and `create_concentrated_pool`.
//!
//! L-7 audit fix: the two create-pool instructions had ~77% duplicated code
//! (creator whitelist check, mint ordering / RWT presence, pool PDA allocation,
//! vault pair initialisation, OT treasury detection). All of that lives here.
//! The instruction handlers in `instructions/create_pool.rs` and
//! `instructions/create_concentrated_pool.rs` call these helpers for the
//! shared flow and only keep their specific additions
//! (bin_step validation, BinArray init, bin_step_bps / active_bin_id on state).

use arlex_lang::prelude::*;
use pinocchio::sysvars::rent::Rent;
use pinocchio::sysvars::Sysvar;

use crate::constants::*;
use crate::error::DexError;
use crate::state::{PoolCreators, PoolState};
use crate::validation::{is_rwt_mint, pubkey_bytes, token_a_is_rwt};

/// SPL Token Account size.
const SPL_TOKEN_ACCOUNT_SIZE: u64 = 165;

/// Validate creator is on the whitelist. Returns creator pubkey bytes.
pub fn require_whitelisted_creator(
    pool_creators: &AccountView,
    creator: &AccountView,
    program_id: &Address,
) -> core::result::Result<[u8; 32], ProgramError> {
    let creators = PoolCreators::load(pool_creators, program_id)?;
    let creator_key = pubkey_bytes(creator);
    for i in 0..creators.active_count as usize {
        if creators.creators[i] == creator_key {
            return Ok(creator_key);
        }
    }
    Err(ProgramError::from(DexError::CreatorNotWhitelisted))
}

/// Validate mint pair: identical check, canonical ordering, and RWT presence.
pub fn require_valid_mint_pair(
    token_a_mint: &AccountView,
    token_b_mint: &AccountView,
) -> core::result::Result<([u8; 32], [u8; 32]), ProgramError> {
    let mint_a = pubkey_bytes(token_a_mint);
    let mint_b = pubkey_bytes(token_b_mint);

    if mint_a == mint_b {
        return Err(ProgramError::from(DexError::IdenticalMints));
    }
    if mint_a >= mint_b {
        return Err(ProgramError::from(DexError::InvalidMintOrder));
    }
    let _a_is_rwt = token_a_is_rwt(&mint_a, &mint_b)?;
    Ok((mint_a, mint_b))
}

/// Derive the `["pool", mint_a, mint_b]` PDA and create the account via
/// System Program CPI. Returns (pool_pda, bump).
pub fn create_pool_account(
    creator: &AccountView,
    pool_state: &AccountView,
    mint_a: &[u8; 32],
    mint_b: &[u8; 32],
    program_id: &Address,
    rent: &Rent,
) -> core::result::Result<(Address, u8), ProgramError> {
    let (pool_pda, pool_bump) = arlex_lang::find_program_address(
        &[b"pool", mint_a.as_ref(), mint_b.as_ref()],
        program_id,
    );

    if pool_state.address().as_ref() != pool_pda.as_ref() {
        return Err(ProgramError::InvalidSeeds);
    }

    let lamports = rent.minimum_balance(PoolState::SPACE);
    arlex_lang::system::instructions::CreateAccount {
        from: creator,
        to: pool_state,
        lamports,
        space: PoolState::SPACE as u64,
        owner: program_id,
    }
    .invoke_signed(&[Signer::from(&[
        Seed::from(b"pool" as &[u8]),
        Seed::from(mint_a.as_ref()),
        Seed::from(mint_b.as_ref()),
        Seed::from(&[pool_bump]),
    ])])?;

    Ok((pool_pda, pool_bump))
}

/// Initialise both vault_a and vault_b as SPL Token Accounts owned by the pool PDA.
pub fn init_vault_pair(
    creator: &AccountView,
    vault_a: &AccountView,
    vault_b: &AccountView,
    token_a_mint: &AccountView,
    token_b_mint: &AccountView,
    pool_pda: &Address,
    rent: &Rent,
) -> ProgramResult {
    let vault_rent = rent.minimum_balance(SPL_TOKEN_ACCOUNT_SIZE as usize);

    // Vault A
    arlex_lang::system::instructions::CreateAccount {
        from: creator,
        to: vault_a,
        lamports: vault_rent,
        space: SPL_TOKEN_ACCOUNT_SIZE,
        owner: &Address::new_from_array(SPL_TOKEN_PROGRAM),
    }
    .invoke()?;
    arlex_lang::token::instructions::InitializeAccount3 {
        account: vault_a,
        mint: token_a_mint,
        owner: pool_pda,
    }
    .invoke()?;

    // Vault B
    arlex_lang::system::instructions::CreateAccount {
        from: creator,
        to: vault_b,
        lamports: vault_rent,
        space: SPL_TOKEN_ACCOUNT_SIZE,
        owner: &Address::new_from_array(SPL_TOKEN_PROGRAM),
    }
    .invoke()?;
    arlex_lang::token::instructions::InitializeAccount3 {
        account: vault_b,
        mint: token_b_mint,
        owner: pool_pda,
    }
    .invoke()?;

    Ok(())
}

/// Detect and validate the optional OT treasury pair in remaining_accounts[0..2].
/// Returns (ot_treasury_fee_destination, has_ot_treasury).
pub fn detect_ot_treasury(
    remaining_accounts: &[AccountView],
    mint_a: &[u8; 32],
    mint_b: &[u8; 32],
) -> core::result::Result<([u8; 32], bool), ProgramError> {
    if remaining_accounts.len() < 2 {
        return Ok(([0u8; 32], false));
    }

    let ot_treasury = &remaining_accounts[0];
    let ot_treasury_rwt_ata = &remaining_accounts[1];
    let ot_mint = if is_rwt_mint(mint_a) { mint_b } else { mint_a };

    let (expected_treasury_pda, _) = arlex_lang::find_program_address(
        &[b"ot_treasury", ot_mint.as_ref()],
        &Address::new_from_array(OT_PROGRAM_ID),
    );
    if ot_treasury.address().as_ref() != expected_treasury_pda.as_ref() {
        return Err(ProgramError::from(DexError::InvalidOtTreasuryDestination));
    }

    // See lib.rs "Unsafe (L-5 audit note)" — Pinocchio zero-copy pattern.
    if unsafe { ot_treasury.owner() }.as_ref() != OT_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(DexError::InvalidOtTreasuryDestination));
    }

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

    let mut dest = [0u8; 32];
    dest.copy_from_slice(ot_treasury_rwt_ata.address().as_ref());
    Ok((dest, true))
}
