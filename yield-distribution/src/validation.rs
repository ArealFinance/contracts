//! SPL Token Account zero-copy readers.
//!
//! L-5: `unsafe` blocks wrap raw-pointer slice construction (standard Pinocchio
//! pattern); every read is bounded by an explicit length check.
//!
//! N-6 audit note: these helpers are duplicated in `rwt-engine` and `native-dex`
//! with identical semantics. Consolidation into shared arlex helpers is
//! deferred to the next arlex minor release.

use arlex_lang::prelude::*;

/// Read the `mint` field (first 32 bytes) from an SPL Token Account.
/// SPL Token Account layout: [mint: 32][owner: 32][amount: u64][...].
pub fn read_token_account_mint(
    account: &AccountView,
) -> core::result::Result<[u8; 32], ProgramError> {
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 32 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut mint = [0u8; 32];
    mint.copy_from_slice(&data[0..32]);
    Ok(mint)
}

/// Read the `owner` field (bytes 32..64) from an SPL Token Account.
pub fn read_token_account_owner(
    account: &AccountView,
) -> core::result::Result<[u8; 32], ProgramError> {
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 64 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut owner = [0u8; 32];
    owner.copy_from_slice(&data[32..64]);
    Ok(owner)
}

/// Read the `amount` field (bytes 64..72, little-endian u64) from an SPL Token Account.
pub fn read_token_account_amount(account: &AccountView) -> core::result::Result<u64, ProgramError> {
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&data[64..72]);
    Ok(u64::from_le_bytes(buf))
}
