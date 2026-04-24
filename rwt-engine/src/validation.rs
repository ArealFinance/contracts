use arlex_lang::prelude::*;

/// Read mint field (first 32 bytes) from an SPL Token Account.
/// SPL Token Account layout: [mint: 32][owner: 32][amount: u64][...]
///
/// L-5: `unsafe` is the standard Pinocchio zero-copy pattern. `AccountView`
/// exposes `data_ptr()` and `data_len()` straight from the Solana BPF loader;
/// constructing a slice from them is sound as long as `data_len()` bounds are
/// respected. The explicit length check below prevents OOB reads.
///
/// N-6 audit note: this helper is duplicated in `native-dex` and
/// `yield-distribution` (identical implementation). Consolidation into an
/// arlex shared module is deferred to the next arlex minor release — for the
/// hackathon cut we keep it local per contract.
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
