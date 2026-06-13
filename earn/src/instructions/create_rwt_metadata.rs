//! create_rwt_metadata — CPI to Metaplex Token Metadata `CreateMetadataAccountV3`.
//!
//! Creates a Metaplex Token Metadata account for the earn-RWT mint so that
//! Phantom, Solscan, and Meteora display a proper name + symbol + icon instead
//! of a nameless "SPL Token".
//!
//! The earn-RWT mint authority is the `EarnConfig` PDA, which must sign the
//! Metaplex CPI as `mint_authority`. This instruction wraps that signing with
//! the same `[EARN_CONFIG_SEED, bump]` signer seeds used by `mint_rwt` and
//! `seed_genesis`.
//!
//! Three deliberate deviations from the rwt-engine / ownership-token templates
//! (the on-the-wire Metaplex byte layout is reused verbatim; the binding,
//! seeds, signer and gating are earn-specific):
//!   1. Gating = the pinned `BOOTSTRAP_AUTHORITY` (same gate as `initialize` /
//!      `seed_genesis`), NOT `config.authority`. The founder publishes metadata
//!      once during bootstrap, then hands authority to the multisig.
//!   2. The metadata `update_authority` = `config.authority` (the multisig
//!      vault), passed as a SEPARATE non-signer account distinct from the
//!      `mint_authority` signer (the EarnConfig PDA). CreateMetadataAccountV3
//!      allows update_authority ≠ mint_authority, and update_authority does NOT
//!      need to sign on create when is_mutable = true — so the multisig vault
//!      becomes the on-chain update authority without signing here.
//!   3. NOT devnet-gated — compiles and runs in BOTH the default (mainnet) and
//!      `--features devnet` builds.
//!
//! Config is read via `EarnConfig::load` (read-only). This instruction does NOT
//! mutate config (only reads rwt_mint, authority, bump), so there is no
//! `load_mut`, no engaged borrow guard, and no drop-before-CPI dance — the
//! EarnConfig PDA may be passed as the CPI signer directly (avoids the
//! arlex v0.1.2 CPI-borrow blocker, which only affects `load_mut`).
//!
//! Metaplex CreateMetadataAccountV3 instruction layout (tag = 33):
//!   tag (u8) = 33
//!   data: DataV2 {
//!     name: String,           // borsh: u32 LE length + UTF-8 bytes
//!     symbol: String,
//!     uri: String,
//!     seller_fee_basis_points: u16,     // 0
//!     creators: Option<Vec<Creator>>,   // None = 0
//!     collection: Option<Collection>,   // None = 0
//!     uses: Option<Uses>,               // None = 0
//!   }
//!   is_mutable: bool,                              // true
//!   collection_details: Option<CollectionDetails>, // None = 0
//!
//! Account order (Metaplex CreateMetadataAccountV3):
//!   0. metadata_account (mut, will be created)
//!   1. mint
//!   2. mint_authority (signer) = EarnConfig PDA
//!   3. payer (mut, signer) = bootstrap_authority
//!   4. update_authority = config.authority (multisig vault, non-signer)
//!   5. system_program
//!   6. rent sysvar

use arlex_lang::prelude::*;
use pinocchio::cpi::{invoke_signed, Seed, Signer};
use pinocchio::instruction::{InstructionAccount, InstructionView};

use crate::constants::{BOOTSTRAP_AUTHORITY, EARN_CONFIG_SEED, SPL_TOKEN_PROGRAM, SYSTEM_PROGRAM};
use crate::error::EarnError;
use crate::state::EarnConfig;

// Metaplex Token Metadata program ID: metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s
const MPL_TOKEN_METADATA_PROGRAM_ID: [u8; 32] = [
    0x0b, 0x70, 0x65, 0xb1, 0xe3, 0xd1, 0x7c, 0x45,
    0x38, 0x9d, 0x52, 0x7f, 0x6b, 0x04, 0xc3, 0xcd,
    0x58, 0xb8, 0x6c, 0x73, 0x1a, 0xa0, 0xfd, 0xb5,
    0x49, 0xb6, 0xd1, 0xbc, 0x03, 0xf8, 0x29, 0x46,
];

// Rent sysvar: SysvarRent111111111111111111111111111111111
const RENT_SYSVAR_ID: [u8; 32] = [
    0x06, 0xa7, 0xd5, 0x17, 0x19, 0x2c, 0x5c, 0x51,
    0x21, 0x8c, 0xc9, 0x4c, 0x3d, 0x4a, 0xf1, 0x7f,
    0x58, 0xda, 0xee, 0x08, 0x9b, 0xa1, 0xfd, 0x44,
    0xe3, 0xdb, 0xd9, 0x8a, 0x00, 0x00, 0x00, 0x00,
];

#[derive(Accounts)]
pub struct CreateRwtMetadata<'info> {
    /// Founder bootstrap key — must equal the pinned BOOTSTRAP_AUTHORITY (same
    /// gate as `initialize` / `seed_genesis`). Also the metadata rent payer.
    #[account(mut, signer)]
    pub bootstrap_authority: &'info AccountView,

    /// EarnConfig PDA (mint authority of the earn-RWT mint; signs the Metaplex
    /// CPI). Read-only: only rwt_mint / authority / bump are read.
    #[account(seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,

    /// Earn-RWT mint (must == config.rwt_mint). mint_authority == EarnConfig PDA.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    /// Metadata PDA — derived as [b"metadata", mpl_program_id, rwt_mint] under
    /// the Token Metadata program. Created by the CPI.
    #[account(mut)]
    pub metadata_account: &'info AccountView,

    /// Metadata `update_authority` — must equal `config.authority` (the multisig
    /// vault). Passed as a SEPARATE account from the EarnConfig PDA mint_authority
    /// signer. Non-signer / non-writable on create.
    pub update_authority: &'info AccountView,

    /// Metaplex Token Metadata program (validated against the pinned ID).
    pub mpl_token_metadata_program: &'info AccountView,

    /// System program (account creation).
    #[account(constraint = system_program.address() == &Address::new_from_array(SYSTEM_PROGRAM))]
    pub system_program: &'info AccountView,

    /// Rent sysvar.
    pub rent: &'info AccountView,
}

pub fn handler(
    ctx: Context<CreateRwtMetadata>,
    name: [u8; 32],
    name_len: u8,
    symbol: [u8; 10],
    symbol_len: u8,
    uri: [u8; 200],
    uri_len: u8,
) -> Result<()> {
    // --- Validation ---

    // Read-only config load (owner + size + discriminator checks; non-engaging).
    // No mutation here, so no load_mut / borrow guard / drop-before-CPI needed —
    // the EarnConfig PDA can be passed as the CPI signer directly.
    let config = EarnConfig::load(ctx.accounts.earn_config, ctx.program_id)?;

    // Bootstrap gate (NOT config.authority): the founder publishes metadata once
    // before handing off — same pinned key as `initialize` / `seed_genesis`.
    if ctx.accounts.bootstrap_authority.address().as_ref() != BOOTSTRAP_AUTHORITY.as_ref() {
        return Err(ProgramError::from(EarnError::UnauthorizedBootstrap));
    }

    // earn-RWT mint must match the config-pinned mint.
    if ctx.accounts.rwt_mint.address().as_ref() != config.rwt_mint.as_ref() {
        return Err(ProgramError::from(EarnError::InvalidRwtMint));
    }

    // update_authority must equal config.authority (the multisig vault). This
    // becomes the metadata's on-chain update authority — distinct from the
    // mint_authority signer (the EarnConfig PDA).
    if ctx.accounts.update_authority.address().as_ref() != config.authority.as_ref() {
        return Err(ProgramError::from(EarnError::Unauthorized));
    }

    // Validate Metaplex program ID (prevents a fake-program CPI).
    if ctx.accounts.mpl_token_metadata_program.address().as_ref()
        != MPL_TOKEN_METADATA_PROGRAM_ID.as_ref()
    {
        return Err(ProgramError::from(EarnError::InvalidTokenAccount));
    }

    // Validate rent sysvar.
    if ctx.accounts.rent.address().as_ref() != RENT_SYSVAR_ID.as_ref() {
        return Err(ProgramError::from(EarnError::InvalidTokenAccount));
    }

    // Length sanity (Metaplex hard caps: 32 / 10 / 200).
    if name_len as usize > 32 || symbol_len as usize > 10 || uri_len as usize > 200 {
        return Err(ProgramError::from(EarnError::ZeroAmount));
    }

    // --- Build CPI instruction data ---
    // Layout: tag(1) + name(4+N) + symbol(4+S) + uri(4+U) + sellerFeeBps(2)
    //         + creators(1=None) + collection(1=None) + uses(1=None)
    //         + is_mutable(1) + collectionDetails(1=None)
    //
    // Worst case: 1 + (4+32) + (4+10) + (4+200) + 2 + 3 + 1 + 1 = 262 bytes.
    let n = name_len as usize;
    let s = symbol_len as usize;
    let u = uri_len as usize;
    let total_len = 1 + (4 + n) + (4 + s) + (4 + u) + 2 + 1 + 1 + 1 + 1 + 1;

    let mut data = [0u8; 262];
    if total_len > data.len() {
        return Err(ProgramError::from(EarnError::MathOverflow));
    }
    let mut off = 0usize;

    // tag = CreateMetadataAccountV3
    data[off] = 33;
    off += 1;

    // name string (u32 LE length + UTF-8 bytes)
    data[off..off + 4].copy_from_slice(&(n as u32).to_le_bytes());
    off += 4;
    data[off..off + n].copy_from_slice(&name[..n]);
    off += n;

    // symbol string
    data[off..off + 4].copy_from_slice(&(s as u32).to_le_bytes());
    off += 4;
    data[off..off + s].copy_from_slice(&symbol[..s]);
    off += s;

    // uri string
    data[off..off + 4].copy_from_slice(&(u as u32).to_le_bytes());
    off += 4;
    data[off..off + u].copy_from_slice(&uri[..u]);
    off += u;

    // seller_fee_basis_points = 0
    data[off] = 0;
    data[off + 1] = 0;
    off += 2;

    // creators: Option<Vec<Creator>> = None
    data[off] = 0;
    off += 1;
    // collection: Option<Collection> = None
    data[off] = 0;
    off += 1;
    // uses: Option<Uses> = None
    data[off] = 0;
    off += 1;

    // is_mutable = true
    data[off] = 1;
    off += 1;
    // collection_details: Option<CollectionDetails> = None
    data[off] = 0;
    off += 1;

    debug_assert!(off == total_len);

    // --- Build CPI account list (Metaplex CreateMetadataAccountV3 order) ---
    // 0. metadata_account (mut)
    // 1. mint
    // 2. mint_authority (signer) = EarnConfig PDA
    // 3. payer (mut, signer) = bootstrap_authority
    // 4. update_authority = config.authority (multisig vault, non-signer)
    // 5. system_program
    // 6. rent sysvar
    let cpi_accounts = [
        InstructionAccount::new(ctx.accounts.metadata_account.address(), true, false), // 0
        InstructionAccount::new(ctx.accounts.rwt_mint.address(), false, false),        // 1
        InstructionAccount::new(ctx.accounts.earn_config.address(), false, true),      // 2 mint_authority signer (PDA)
        InstructionAccount::new(ctx.accounts.bootstrap_authority.address(), true, true), // 3 payer signer
        InstructionAccount::new(ctx.accounts.update_authority.address(), false, false), // 4 update_authority = config.authority (vault)
        InstructionAccount::new(ctx.accounts.system_program.address(), false, false),  // 5
        InstructionAccount::new(ctx.accounts.rent.address(), false, false),            // 6
    ];

    let instruction = InstructionView {
        program_id: ctx.accounts.mpl_token_metadata_program.address(),
        data: &data[..total_len],
        accounts: &cpi_accounts,
    };

    // --- PDA signer for EarnConfig: seeds = [EARN_CONFIG_SEED, [bump]] ---
    // Same signer-seeds pattern as `seed_genesis` / `mint_rwt`. The read-only
    // load above does not engage the borrow flag, so the checked invoke accepts
    // the EarnConfig account as the mint_authority signer.
    let bump = [config.bump];
    let seeds = [Seed::from(EARN_CONFIG_SEED), Seed::from(bump.as_ref())];
    let signer = Signer::from(&seeds);

    // The account_views slice is matched positionally (by address) against the
    // instruction's account metas, so update_authority must be its own view in
    // slot 4 — NOT the EarnConfig PDA. The mpl program account trails as the
    // CPI program. 8 views total → invoke_signed::<8> (cf. OT / rwt-engine).
    invoke_signed::<8>(
        &instruction,
        &[
            ctx.accounts.metadata_account,
            ctx.accounts.rwt_mint,
            ctx.accounts.earn_config,
            ctx.accounts.bootstrap_authority,
            ctx.accounts.update_authority, // update_authority = config.authority (vault)
            ctx.accounts.system_program,
            ctx.accounts.rent,
            ctx.accounts.mpl_token_metadata_program,
        ],
        &[signer],
    )?;

    arlex_lang::log("earn-RWT metadata created");
    Ok(())
}
