use arlex_lang::prelude::*;

/// Read mint field (first 32 bytes) from an SPL Token Account.
/// SPL Token Account layout: [mint: 32][owner: 32][amount: u64][...]
///
/// L-5: `unsafe` is the standard Pinocchio zero-copy pattern. `AccountView`
/// exposes `data_ptr()` and `data_len()` straight from the Solana BPF loader;
/// constructing a slice from them is sound as long as `data_len()` bounds are
/// respected. The explicit length check below prevents OOB reads.
///
/// N-6 audit note: this helper is duplicated across the contract crates
/// (identical implementation). Consolidation into an arlex shared module is
/// deferred to the next arlex minor release.
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

/// Read `supply` (u64) from an SPL Mint account.
/// SPL Mint layout: [mint_authority COption: 36][supply: u64 @36..44][decimals: u8 @44][...]
/// Mirrors `ownership-token/src/instructions/initialize_ot.rs` supply read.
pub fn read_mint_supply(mint: &AccountView) -> core::result::Result<u64, ProgramError> {
    let data = unsafe {
        core::slice::from_raw_parts(mint.data_ptr(), mint.data_len())
    };
    if data.len() < 82 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(u64::from_le_bytes(data[36..44].try_into().unwrap()))
}
