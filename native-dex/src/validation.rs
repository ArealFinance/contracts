use arlex_lang::prelude::*;

/// Read mint field (first 32 bytes) from an SPL Token Account.
/// SPL Token Account layout: [mint: 32][owner: 32][amount: u64][...]
pub fn read_token_account_mint(account: &AccountView) -> core::result::Result<[u8; 32], ProgramError> {
    let data = unsafe {
        core::slice::from_raw_parts(account.data_ptr(), account.data_len())
    };
    if data.len() < 32 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut mint = [0u8; 32];
    mint.copy_from_slice(&data[0..32]);
    Ok(mint)
}

/// Read owner field (bytes 32..64) from an SPL Token Account.
pub fn read_token_account_owner(account: &AccountView) -> core::result::Result<[u8; 32], ProgramError> {
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

/// Validate vault account matches the stored vault address in pool_state.
pub fn validate_vault(vault: &AccountView, expected: &[u8; 32]) -> core::result::Result<(), ProgramError> {
    if vault.address().as_ref() != expected.as_ref() {
        return Err(ProgramError::from(crate::error::DexError::InvalidVault));
    }
    Ok(())
}

/// Extract pubkey bytes from an AccountView into a [u8; 32].
pub fn pubkey_bytes(account: &AccountView) -> [u8; 32] {
    let mut arr = [0u8; 32];
    arr.copy_from_slice(account.address().as_ref());
    arr
}

/// Check if an address is the RWT mint.
pub fn is_rwt_mint(addr: &[u8; 32]) -> bool {
    *addr == crate::constants::RWT_MINT
}

/// Determine which side of a pool pair is RWT. Returns true if token_a is RWT.
pub fn token_a_is_rwt(token_a_mint: &[u8; 32], token_b_mint: &[u8; 32]) -> core::result::Result<bool, ProgramError> {
    if is_rwt_mint(token_a_mint) {
        Ok(true)
    } else if is_rwt_mint(token_b_mint) {
        Ok(false)
    } else {
        Err(ProgramError::from(crate::error::DexError::MissingRwtMint))
    }
}
