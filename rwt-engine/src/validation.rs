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
