//! Zero-copy SPL Token reads (mint supply, token-account mint/owner/amount).
//!
//! L-5 audit note: `unsafe { from_raw_parts(...) }` is the standard Pinocchio
//! zero-copy pattern (`AccountView` exposes `data_ptr()` / `data_len()` from
//! the BPF loader). Every read is bounded by an explicit length check before
//! any indexing. Same helpers as `rwt-engine::validation`, kept local.

use arlex_lang::prelude::*;

/// SPL Mint layout: `[mint_authority(36 COption)][supply: u64 @36..44][...]`.
/// Read the `supply` field.
pub fn read_mint_supply(account: &AccountView) -> core::result::Result<u64, ProgramError> {
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 44 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(u64::from_le_bytes(data[36..44].try_into().unwrap()))
}

/// SPL Token Account layout: `[mint:32][owner:32][amount:u64 @64..72][...]`.
/// Read the `amount` field.
pub fn read_token_account_amount(account: &AccountView) -> core::result::Result<u64, ProgramError> {
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

/// Read `mint` (bytes 0..32) of an SPL Token Account.
pub fn read_token_account_mint(account: &AccountView) -> core::result::Result<[u8; 32], ProgramError> {
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 32 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut mint = [0u8; 32];
    mint.copy_from_slice(&data[0..32]);
    Ok(mint)
}

/// Read `owner` (bytes 32..64) of an SPL Token Account.
pub fn read_token_account_owner(account: &AccountView) -> core::result::Result<[u8; 32], ProgramError> {
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 64 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut owner = [0u8; 32];
    owner.copy_from_slice(&data[32..64]);
    Ok(owner)
}
