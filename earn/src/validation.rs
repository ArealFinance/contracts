use arlex_lang::prelude::*;

fn account_data(account: &AccountView) -> &[u8] {
    unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) }
}

fn parse_coption_pubkey(
    data: &[u8],
    tag_offset: usize,
    key_offset: usize,
) -> core::result::Result<Option<[u8; 32]>, ProgramError> {
    if data.len() < key_offset + 32 {
        return Err(ProgramError::InvalidAccountData);
    }

    let tag = u32::from_le_bytes(
        data[tag_offset..tag_offset + 4]
            .try_into()
            .map_err(|_| ProgramError::InvalidAccountData)?,
    );

    match tag {
        0 => Ok(None),
        1 => {
            let mut key = [0u8; 32];
            key.copy_from_slice(&data[key_offset..key_offset + 32]);
            Ok(Some(key))
        }
        _ => Err(ProgramError::InvalidAccountData),
    }
}

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
pub fn read_token_account_mint(
    account: &AccountView,
) -> core::result::Result<[u8; 32], ProgramError> {
    let data = account_data(account);
    if data.len() < 32 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut mint = [0u8; 32];
    mint.copy_from_slice(&data[0..32]);
    Ok(mint)
}

/// Read owner field from an SPL Token Account.
/// SPL Token Account layout: [mint: 32][owner: 32][amount: u64][...]
pub fn read_token_account_owner(
    account: &AccountView,
) -> core::result::Result<[u8; 32], ProgramError> {
    let data = account_data(account);
    if data.len() < 64 {
        return Err(ProgramError::InvalidAccountData);
    }
    let mut owner = [0u8; 32];
    owner.copy_from_slice(&data[32..64]);
    Ok(owner)
}

/// Read amount field from an SPL Token Account.
/// SPL Token Account layout: [mint: 32][owner: 32][amount: u64][...]
pub fn read_token_account_amount(account: &AccountView) -> core::result::Result<u64, ProgramError> {
    let data = account_data(account);
    if data.len() < 72 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

/// Read mint authority from an SPL Mint account.
/// SPL Mint layout:
/// [mint_authority COption @0..36][supply @36..44][decimals @44]
/// [is_initialized @45][freeze_authority COption @46..82]
pub fn read_mint_authority(
    account: &AccountView,
) -> core::result::Result<Option<[u8; 32]>, ProgramError> {
    let data = account_data(account);
    parse_coption_pubkey(data, 0, 4)
}

/// Read `supply` (u64) from an SPL Mint account.
/// SPL Mint layout: [mint_authority COption: 36][supply: u64 @36..44][decimals: u8 @44][...]
/// Mirrors `ownership-token/src/instructions/initialize_ot.rs` supply read.
pub fn read_mint_supply(mint: &AccountView) -> core::result::Result<u64, ProgramError> {
    let data = account_data(mint);
    if data.len() < 44 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(u64::from_le_bytes(data[36..44].try_into().unwrap()))
}

/// Read decimals from an SPL Mint account.
pub fn read_mint_decimals(mint: &AccountView) -> core::result::Result<u8, ProgramError> {
    let data = account_data(mint);
    if data.len() < 45 {
        return Err(ProgramError::InvalidAccountData);
    }
    Ok(data[44])
}

/// Read freeze authority from an SPL Mint account.
pub fn read_mint_freeze_authority(
    account: &AccountView,
) -> core::result::Result<Option<[u8; 32]>, ProgramError> {
    let data = account_data(account);
    parse_coption_pubkey(data, 46, 50)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_coption_pubkey_accepts_some_and_none() {
        let key = [7u8; 32];
        let mut some = [0u8; 36];
        some[0..4].copy_from_slice(&1u32.to_le_bytes());
        some[4..36].copy_from_slice(&key);
        assert_eq!(parse_coption_pubkey(&some, 0, 4).unwrap(), Some(key));

        let none = [0u8; 36];
        assert_eq!(parse_coption_pubkey(&none, 0, 4).unwrap(), None);
    }

    #[test]
    fn parse_coption_pubkey_rejects_invalid_tag() {
        let mut data = [0u8; 36];
        data[0..4].copy_from_slice(&2u32.to_le_bytes());
        assert!(parse_coption_pubkey(&data, 0, 4).is_err());
    }
}
