//! create_ot_metadata — CPI to Metaplex Token Metadata `CreateMetadataAccountV3`.
//!
//! Creates a Metaplex Token Metadata account for an OT mint so that
//! Phantom and other wallets display proper name + symbol + icon
//! instead of "Unknown Token / null".
//!
//! The OT mint authority is the `ot_config` PDA, which must sign the
//! Metaplex CPI. This instruction wraps that signing.
//!
//! Devnet-only: gated by `#[cfg(feature = "devnet")]` on the registry
//! side. The handler additionally requires the OtGovernance `authority`
//! to sign, so only the deployer / configured authority can call this.
//!
//! Metaplex CreateMetadataAccountV3 layout: see
//! contracts/rwt-engine/src/instructions/create_rwt_metadata.rs for the
//! shared documentation of the on-the-wire format.

use arlex_lang::prelude::*;
use pinocchio::instruction::{InstructionView, InstructionAccount};
use pinocchio::cpi::{invoke_signed, Seed, Signer};

use crate::error::OtError;
use crate::state::{OtConfig, OtGovernance};

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
pub struct CreateOtMetadata<'info> {
    /// Governance authority must sign (gates this instruction).
    #[account(signer)]
    pub authority: &'info AccountView,

    /// OtGovernance PDA — validates `authority` via has_one.
    #[account(
        has_one = authority, account_type = "OtGovernance",
        seeds = [b"ot_governance", ot_mint.address().as_ref()], bump
    )]
    pub ot_governance: &'info AccountView,

    /// OtConfig PDA — signs Metaplex CPI as mint_authority.
    #[account(
        seeds = [b"ot_config", ot_mint.address().as_ref()], bump
    )]
    pub ot_config: &'info AccountView,

    /// OT mint (mint_authority == ot_config).
    pub ot_mint: &'info AccountView,

    /// Metadata PDA — derived as [b"metadata", mpl_program_id, ot_mint].
    #[account(mut)]
    pub metadata_account: &'info AccountView,

    /// Payer of the metadata account rent.
    #[account(mut, signer)]
    pub payer: &'info AccountView,

    /// Metaplex Token Metadata program (validated against pinned ID).
    pub mpl_token_metadata_program: &'info AccountView,

    /// System program.
    pub system_program: &'info AccountView,

    /// Rent sysvar.
    pub rent: &'info AccountView,
}

pub fn handler(
    ctx: Context<CreateOtMetadata>,
    name: [u8; 32],
    name_len: u8,
    symbol: [u8; 10],
    symbol_len: u8,
    uri: [u8; 200],
    uri_len: u8,
) -> Result<()> {
    // --- Validation ---

    // Governance is_active check
    let governance = OtGovernance::load(ctx.accounts.ot_governance, ctx.program_id)?;
    if !governance.is_active {
        return Err(ProgramError::from(OtError::GovernanceInactive));
    }

    // Load ot_config to verify ot_mint pin + grab bump for PDA signer
    let ot_config = OtConfig::load(ctx.accounts.ot_config, ctx.program_id)?;
    if ctx.accounts.ot_mint.address().as_ref() != ot_config.ot_mint.as_ref() {
        return Err(ProgramError::from(OtError::TokenMintMismatch));
    }

    // Validate Metaplex program ID
    if ctx.accounts.mpl_token_metadata_program.address().as_ref() != MPL_TOKEN_METADATA_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(OtError::Unauthorized));
    }

    // Validate system program
    if ctx.accounts.system_program.address().as_ref() != SYSTEM_PROGRAM_ID.as_ref() {
        return Err(ProgramError::from(OtError::Unauthorized));
    }

    // Validate rent sysvar
    if ctx.accounts.rent.address().as_ref() != RENT_SYSVAR_ID.as_ref() {
        return Err(ProgramError::from(OtError::Unauthorized));
    }

    // Length sanity (Metaplex hard caps: 32 / 10 / 200)
    if name_len as usize > 32 || symbol_len as usize > 10 || uri_len as usize > 200 {
        return Err(ProgramError::from(OtError::InvalidName));
    }

    // --- Build CPI instruction data ---
    // Worst case: 1 + (4+32) + (4+10) + (4+200) + 2 + 3 + 1 + 1 = 262 bytes.
    let n = name_len as usize;
    let s = symbol_len as usize;
    let u = uri_len as usize;
    let total_len = 1 + (4 + n) + (4 + s) + (4 + u) + 2 + 1 + 1 + 1 + 1 + 1;

    let mut data = [0u8; 262];
    if total_len > data.len() {
        return Err(ProgramError::from(OtError::MathOverflow));
    }
    let mut off = 0usize;

    data[off] = 33; off += 1; // tag = CreateMetadataAccountV3

    data[off..off + 4].copy_from_slice(&(n as u32).to_le_bytes()); off += 4;
    data[off..off + n].copy_from_slice(&name[..n]); off += n;

    data[off..off + 4].copy_from_slice(&(s as u32).to_le_bytes()); off += 4;
    data[off..off + s].copy_from_slice(&symbol[..s]); off += s;

    data[off..off + 4].copy_from_slice(&(u as u32).to_le_bytes()); off += 4;
    data[off..off + u].copy_from_slice(&uri[..u]); off += u;

    // seller_fee_basis_points = 0
    data[off] = 0; data[off + 1] = 0; off += 2;
    // creators / collection / uses = None
    data[off] = 0; off += 1;
    data[off] = 0; off += 1;
    data[off] = 0; off += 1;
    // is_mutable = true
    data[off] = 1; off += 1;
    // collection_details = None
    data[off] = 0; off += 1;

    debug_assert!(off == total_len);

    // --- Build CPI account list ---
    let cpi_accounts = [
        InstructionAccount::new(ctx.accounts.metadata_account.address(), true, false),  // 0 metadata (mut)
        InstructionAccount::new(ctx.accounts.ot_mint.address(), false, false),          // 1 mint
        InstructionAccount::new(ctx.accounts.ot_config.address(), false, true),         // 2 mint_authority signer (PDA)
        InstructionAccount::new(ctx.accounts.payer.address(), true, true),              // 3 payer signer
        InstructionAccount::new(ctx.accounts.ot_config.address(), false, false),        // 4 update_authority = ot_config PDA
        InstructionAccount::new(ctx.accounts.system_program.address(), false, false),   // 5
        InstructionAccount::new(ctx.accounts.rent.address(), false, false),             // 6
    ];

    let instruction = InstructionView {
        program_id: ctx.accounts.mpl_token_metadata_program.address(),
        data: &data[..total_len],
        accounts: &cpi_accounts,
    };

    // --- PDA signer for ot_config: seeds = [b"ot_config", ot_mint, bump] ---
    let bump = [ot_config.bump];
    let seeds = [
        Seed::from(b"ot_config" as &[u8]),
        Seed::from(ot_config.ot_mint.as_ref()),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    invoke_signed::<8>(
        &instruction,
        &[
            ctx.accounts.metadata_account,
            ctx.accounts.ot_mint,
            ctx.accounts.ot_config,
            ctx.accounts.payer,
            ctx.accounts.ot_config, // update_authority
            ctx.accounts.system_program,
            ctx.accounts.rent,
            ctx.accounts.mpl_token_metadata_program,
        ],
        &[signer],
    )?;

    arlex_lang::log("OT metadata created");
    Ok(())
}
