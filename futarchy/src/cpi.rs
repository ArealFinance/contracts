//! CPI builders for Ownership Token instructions.
//!
//! Account order MUST match the OT #[derive(Accounts)] struct field order.
//! Verified against contracts/ownership-token/src/instructions/*.rs

extern crate alloc;

use alloc::vec::Vec;
use arlex_lang::prelude::*;
use pinocchio::instruction::{InstructionView, InstructionAccount};
use pinocchio::cpi::{invoke_signed, Seed, Signer};

use crate::constants::*;
use crate::state::FutarchyConfig;

/// CPI → OT::mint_ot(amount)
///
/// OT MintOt account order:
/// 0. authority (signer)              — Futarchy config PDA
/// 1. ot_governance
/// 2. ot_config (mut)
/// 3. ot_mint (mut)
/// 4. recipient_token_account (mut)
/// 5. recipient
/// 6. payer (signer, mut)             — executor
/// 7. token_program
/// 8. system_program
/// 9. ata_program
pub fn cpi_mint_ot<'a>(
    config: &FutarchyConfig,
    config_account: &'a AccountView,
    ot_governance: &'a AccountView,
    ot_config: &'a AccountView,
    ot_mint: &'a AccountView,
    recipient_token_account: &'a AccountView,
    recipient: &'a AccountView,
    payer: &'a AccountView,
    token_program: &'a AccountView,
    system_program: &'a AccountView,
    ata_program: &'a AccountView,
    ot_program: &'a AccountView,
    amount: u64,
) -> ProgramResult {
    let mut data = [0u8; 16];
    data[0..8].copy_from_slice(&DISC_MINT_OT);
    data[8..16].copy_from_slice(&amount.to_le_bytes());

    let accounts = [
        InstructionAccount::new(config_account.address(), false, true),   // authority (signer via PDA)
        InstructionAccount::new(ot_governance.address(), false, false),
        InstructionAccount::new(ot_config.address(), true, false),
        InstructionAccount::new(ot_mint.address(), true, false),
        InstructionAccount::new(recipient_token_account.address(), true, false),
        InstructionAccount::new(recipient.address(), false, false),
        InstructionAccount::new(payer.address(), true, true),             // payer also signer
        InstructionAccount::new(token_program.address(), false, false),
        InstructionAccount::new(system_program.address(), false, false),
        InstructionAccount::new(ata_program.address(), false, false),
    ];

    let instruction = InstructionView {
        program_id: ot_program.address(),
        data: &data,
        accounts: &accounts,
    };

    let bump = [config.bump];
    let seeds = [
        Seed::from(b"futarchy_config" as &[u8]),
        Seed::from(config.ot_mint.as_ref()),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    // 11 account_views: 10 CPI accounts + ot_program
    invoke_signed::<11>(
        &instruction,
        &[
            config_account, ot_governance, ot_config, ot_mint,
            recipient_token_account, recipient, payer,
            token_program, system_program, ata_program, ot_program,
        ],
        &[signer],
    )
}

/// CPI → OT::spend_treasury(amount)
///
/// OT SpendTreasury account order:
/// 0. authority (signer)
/// 1. ot_mint
/// 2. ot_governance
/// 3. ot_treasury
/// 4. treasury_token_account (mut)
/// 5. destination_token_account (mut)
/// 6. token_mint
/// 7. token_program
pub fn cpi_spend_treasury<'a>(
    config: &FutarchyConfig,
    config_account: &'a AccountView,
    ot_mint: &'a AccountView,
    ot_governance: &'a AccountView,
    ot_treasury: &'a AccountView,
    treasury_token_account: &'a AccountView,
    destination_token_account: &'a AccountView,
    token_mint: &'a AccountView,
    token_program: &'a AccountView,
    ot_program: &'a AccountView,
    amount: u64,
) -> ProgramResult {
    let mut data = [0u8; 16];
    data[0..8].copy_from_slice(&DISC_SPEND_TREASURY);
    data[8..16].copy_from_slice(&amount.to_le_bytes());

    let accounts = [
        InstructionAccount::new(config_account.address(), false, true),
        InstructionAccount::new(ot_mint.address(), false, false),
        InstructionAccount::new(ot_governance.address(), false, false),
        InstructionAccount::new(ot_treasury.address(), false, false),
        InstructionAccount::new(treasury_token_account.address(), true, false),
        InstructionAccount::new(destination_token_account.address(), true, false),
        InstructionAccount::new(token_mint.address(), false, false),
        InstructionAccount::new(token_program.address(), false, false),
    ];

    let instruction = InstructionView {
        program_id: ot_program.address(),
        data: &data,
        accounts: &accounts,
    };

    let bump = [config.bump];
    let seeds = [
        Seed::from(b"futarchy_config" as &[u8]),
        Seed::from(config.ot_mint.as_ref()),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    invoke_signed::<9>(
        &instruction,
        &[
            config_account, ot_mint, ot_governance, ot_treasury,
            treasury_token_account, destination_token_account, token_mint,
            token_program, ot_program,
        ],
        &[signer],
    )
}

/// CPI → OT::batch_update_destinations(destinations)
///
/// OT BatchUpdateDestinations account order:
/// 0. authority (signer)
/// 1. ot_mint
/// 2. ot_governance
/// 3. revenue_config (mut)
pub fn cpi_batch_update_destinations<'a>(
    config: &FutarchyConfig,
    config_account: &'a AccountView,
    ot_mint: &'a AccountView,
    ot_governance: &'a AccountView,
    revenue_config: &'a AccountView,
    ot_program: &'a AccountView,
    destinations_data: &[u8],
) -> ProgramResult {
    let mut data = Vec::with_capacity(8 + destinations_data.len());
    data.extend_from_slice(&DISC_BATCH_UPDATE_DESTINATIONS);
    data.extend_from_slice(destinations_data);

    let accounts = [
        InstructionAccount::new(config_account.address(), false, true),
        InstructionAccount::new(ot_mint.address(), false, false),
        InstructionAccount::new(ot_governance.address(), false, false),
        InstructionAccount::new(revenue_config.address(), true, false),
    ];

    let instruction = InstructionView {
        program_id: ot_program.address(),
        data: &data,
        accounts: &accounts,
    };

    let bump = [config.bump];
    let seeds = [
        Seed::from(b"futarchy_config" as &[u8]),
        Seed::from(config.ot_mint.as_ref()),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    invoke_signed::<5>(
        &instruction,
        &[config_account, ot_mint, ot_governance, revenue_config, ot_program],
        &[signer],
    )
}

/// CPI → OT::accept_authority_transfer
///
/// OT AcceptAuthorityTransfer account order:
/// 0. new_authority (signer)
/// 1. ot_mint
/// 2. ot_governance (mut)
pub fn cpi_accept_authority_transfer<'a>(
    config: &FutarchyConfig,
    config_account: &'a AccountView,
    ot_mint: &'a AccountView,
    ot_governance: &'a AccountView,
    ot_program: &'a AccountView,
) -> ProgramResult {
    let accounts = [
        InstructionAccount::new(config_account.address(), false, true),
        InstructionAccount::new(ot_mint.address(), false, false),
        InstructionAccount::new(ot_governance.address(), true, false),
    ];

    let instruction = InstructionView {
        program_id: ot_program.address(),
        data: &DISC_ACCEPT_AUTHORITY_TRANSFER,
        accounts: &accounts,
    };

    let bump = [config.bump];
    let seeds = [
        Seed::from(b"futarchy_config" as &[u8]),
        Seed::from(config.ot_mint.as_ref()),
        Seed::from(bump.as_ref()),
    ];
    let signer = Signer::from(&seeds);

    invoke_signed::<4>(
        &instruction,
        &[config_account, ot_mint, ot_governance, ot_program],
        &[signer],
    )
}
