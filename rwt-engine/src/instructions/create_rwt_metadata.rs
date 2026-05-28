//! create_rwt_metadata — CPI to Metaplex Token Metadata `CreateMetadataAccountV3`.
//!
//! Creates a Metaplex Token Metadata account for the RWT mint so that
//! Phantom and other wallets display "Areal RWT" with a proper icon
//! instead of "Unknown Token / null".
//!
//! The RWT mint authority is the `rwt_vault` PDA, which must sign the
//! Metaplex CPI. This instruction wraps that signing.
//!
//! Devnet-only: gated by `#[cfg(feature = "devnet")]` on the registry
//! side. The handler additionally requires the configured `authority`
//! (from RwtVault state) to sign, so even on devnet only the deployer
//! / ConfigAuthority can call this.
//!
//! Metaplex CreateMetadataAccountV3 instruction layout (tag = 33):
//!   tag (u8) = 33
//!   data: DataV2 {
//!     name: String,           // borsh: u32 LE length + UTF-8 bytes
//!     symbol: String,
//!     uri: String,
//!     seller_fee_basis_points: u16,
//!     creators: Option<Vec<Creator>>,   // None = 0
//!     collection: Option<Collection>,   // None = 0
//!     uses: Option<Uses>,               // None = 0
//!   }
//!   is_mutable: bool,
//!   collection_details: Option<CollectionDetails>,  // None = 0
//!
//! Account order:
//!   0. metadata_account (mut, will be created)
//!   1. mint
//!   2. mint_authority (signer) = rwt_vault PDA
//!   3. payer (mut, signer)
//!   4. update_authority
//!   5. system_program
//!   6. rent sysvar

use arlex_lang::prelude::*;
use pinocchio::instruction::{InstructionView, InstructionAccount};
use pinocchio::cpi::{invoke_signed, Seed, Signer};

use crate::error::RwtError;
use crate::state::RwtVault;

// Metaplex Token Metadata program ID: metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s
const MPL_TOKEN_METADATA_PROGRAM_ID: [u8; 32] = [
    0x0b, 0x70, 0x65, 0xb1, 0xe3, 0xd1, 0x7c, 0x45,
    0x38, 0x9d, 0x52, 0x7f, 0x6b, 0x04, 0xc3, 0xcd,
    0x58, 0xb8, 0x6c, 0x73, 0x1a, 0xa0, 0xfd, 0xb5,
    0x49, 0xb6, 0xd1, 0xbc, 0x03, 0xf8, 0x29, 0x46,
];

const SYSTEM_PROGRAM_ID: [u8; 32] = [0u8; 32];

// Rent sysvar: SysvarRent111111111111111111111111111111111
const RENT_SYSVAR_ID: [u8; 32] = [
    0x06, 0xa7, 0xd5, 0x17, 0x19, 0x2c, 0x5c, 0x51,
    0x21, 0x8c, 0xc9, 0x4c, 0x3d, 0x4a, 0xf1, 0x7f,
    0x58, 0xda, 0xee, 0x08, 0x9b, 0xa1, 0xfd, 0x44,
    0xe3, 0xdb, 0xd9, 0x8a, 0x00, 0x00, 0x00, 0x00,
];

#[derive(Accounts)]
pub struct CreateRwtMetadata<'info> {
    /// Vault authority must sign (gates this instruction).
    #[account(signer)]
    pub authority: &'info AccountView,

    /// RWT vault PDA (signs Metaplex CPI as mint_authority).
    #[account(seeds = [b"rwt_vault"], bump)]
    pub rwt_vault: &'info AccountView,

    /// RWT mint (mint_authority == rwt_vault).
    pub rwt_mint: &'info AccountView,

    /// Metadata PDA — derived as [b"metadata", mpl_program_id, rwt_mint].
    #[account(mut)]
    pub metadata_account: &'info AccountView,

    /// Payer of the metadata account rent.
    #[account(mut, signer)]
    pub payer: &'info AccountView,

    /// Metaplex Token Metadata program (validated against pinned ID).
    pub mpl_token_metadata_program: &'info AccountView,

    /// System program (account creation).
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

    // Vault authority gate
    let vault = RwtVault::load(ctx.accounts.rwt_vault, ctx.program_id)?;
    if ctx.accounts.authority.address().as_ref() != vault.authority.as_ref() {
        return Err(ProgramError::from(RwtError::Unauthorized));
    }

    // RWT mint must match vault.rwt_mint
    if ctx.accounts.rwt_mint.address().as_ref() != vault.rwt_mint.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // Validate Metaplex program ID
    if ctx.accounts.mpl_token_metadata_program.address().as_ref() != MPL_TOKEN_METADATA_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidDexProgram));
    }

    // Validate system program
    if ctx.accounts.system_program.address().as_ref() != SYSTEM_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // Validate rent sysvar
    if ctx.accounts.rent.address().as_ref() != RENT_SYSVAR_ID.as_ref() {
        return Err(ProgramError::from(RwtError::InvalidTokenAccount));
    }

    // Length sanity (Metaplex hard caps: 32 / 10 / 200)
    if name_len as usize > 32 || symbol_len as usize > 10 || uri_len as usize > 200 {
        return Err(ProgramError::from(RwtError::ZeroAmount));
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
        return Err(ProgramError::from(RwtError::MathOverflow));
    }
    let mut off = 0usize;

    // tag
    data[off] = 33;
    off += 1;

    // name string
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
    // 2. mint_authority (signer) = rwt_vault PDA
    // 3. payer (mut, signer)
    // 4. update_authority — using rwt_vault PDA so future updates are gated by this program
    // 5. system_program
    // 6. rent sysvar
    let cpi_accounts = [
        InstructionAccount::new(ctx.accounts.metadata_account.address(), true, false),    // 0
        InstructionAccount::new(ctx.accounts.rwt_mint.address(), false, false),           // 1
        InstructionAccount::new(ctx.accounts.rwt_vault.address(), false, true),           // 2 mint_authority signer (PDA)
        InstructionAccount::new(ctx.accounts.payer.address(), true, true),                // 3 payer signer
        InstructionAccount::new(ctx.accounts.rwt_vault.address(), false, false),          // 4 update_authority = vault PDA
        InstructionAccount::new(ctx.accounts.system_program.address(), false, false),     // 5
        InstructionAccount::new(ctx.accounts.rent.address(), false, false),               // 6
    ];

    let instruction = InstructionView {
        program_id: ctx.accounts.mpl_token_metadata_program.address(),
        data: &data[..total_len],
        accounts: &cpi_accounts,
    };

    // --- PDA signer for rwt_vault ---
    let bump = [vault.bump];
    let seeds = [
        Seed::from(b"rwt_vault" as &[u8]),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    invoke_signed::<8>(
        &instruction,
        &[
            ctx.accounts.metadata_account,
            ctx.accounts.rwt_mint,
            ctx.accounts.rwt_vault,
            ctx.accounts.payer,
            ctx.accounts.rwt_vault, // update_authority (same view re-used)
            ctx.accounts.system_program,
            ctx.accounts.rent,
            ctx.accounts.mpl_token_metadata_program,
        ],
        &[signer],
    )?;

    arlex_lang::log("RWT metadata created");
    Ok(())
}
