//! `seed_genesis` — one-time founder genesis mint of earn-RWT against an
//! off-chain real-world asset that was ALREADY acquired (e.g. a $25k car).
//!
//! Unlike `mint_rwt`, this is NOT a USDC deposit: there is no token inflow into
//! the basket. The founder's RWA is the backing; the contract records its value
//! directly into `total_invested_capital` and mints the matching RWT supply.
//!
//! Why NAV is forced to $1.00:
//!   At genesis `supply == 0`, so Book NAV uses the INITIAL_NAV guard ($1.00).
//!   We set `total_invested_capital = amount` and mint exactly `amount` RWT.
//!   Because earn-RWT (6dp) and USDC (6dp) share the same scale, capital and
//!   supply grow in lockstep 1:1 → post-genesis NAV == INITIAL_NAV ($1.00).
//!   Example: a $25,000 asset → amount = 25_000_000_000 → 25,000 RWT @ $1.00.
//!
//! Guards (this is a singleton, irreversible bootstrap):
//!   - bootstrap-gated: caller must be the pinned BOOTSTRAP_AUTHORITY, NOT
//!     `config.authority` — same gate as `initialize`. The founder seeds the
//!     genesis once before handing the authority to a multisig.
//!   - one-time: on-chain RWT `supply` MUST be 0 (no prior mint), AND
//!     `total_invested_capital` MUST be 0 (belt-and-suspenders against any
//!     pre-seeded capital). A non-zero supply means genesis already happened.
//!   - amount > 0.
//!
//! Order: CHECKS → EFFECTS → INTERACTIONS. The `total_invested_capital` write
//! is applied BEFORE the MintTo CPI, and the EarnConfig borrow guard is DROPPED
//! before the CPI — the earn_config PDA is the MintTo `mint_authority` signer,
//! and the checked invoke rejects a live `AccountRefMut` on it (exactly like
//! `mint_rwt`).

use arlex_lang::prelude::*;
use pinocchio::sysvars::{clock::Clock, Sysvar};

use crate::constants::*;
use crate::error::EarnError;
use crate::events::GenesisSeeded;
use crate::state::EarnConfig;
use crate::validation::{read_mint_supply, read_token_account_mint};

#[derive(Accounts)]
pub struct SeedGenesis<'info> {
    /// Founder bootstrap key — must equal the pinned BOOTSTRAP_AUTHORITY.
    #[account(signer)]
    pub bootstrap_authority: &'info AccountView,

    /// EarnConfig PDA (mutated: capital counter set to `amount`).
    #[account(mut, seeds = [EARN_CONFIG_SEED], bump)]
    pub earn_config: &'info AccountView,

    /// Earn-RWT mint (mutated by the MintTo CPI). Supply read for the
    /// one-time genesis guard.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub rwt_mint: &'info AccountView,

    /// Treasury earn-RWT ATA receiving the genesis mint.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub recipient_rwt: &'info AccountView,

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

/// Pure-validation core of the genesis gates, extracted for unit testing
/// (mirrors `initialize::validate_bootstrap_inputs`). All four invariants must
/// hold for genesis to proceed:
///   - `signer` is the pinned BOOTSTRAP_AUTHORITY,
///   - `amount` > 0,
///   - on-chain RWT `supply` == 0,
///   - `capital` (total_invested_capital) == 0.
fn validate_seed_genesis_inputs(
    signer: &[u8],
    amount: u64,
    supply: u64,
    capital: u128,
) -> Result<()> {
    // bootstrap-gated (NOT config.authority): the founder seeds genesis once.
    if signer != BOOTSTRAP_AUTHORITY.as_ref() {
        return Err(ProgramError::from(EarnError::UnauthorizedBootstrap));
    }
    if amount == 0 {
        return Err(ProgramError::from(EarnError::ZeroAmount));
    }
    // one-time genesis guard: on-chain supply must be zero.
    if supply != 0 {
        return Err(ProgramError::from(EarnError::GenesisAlreadyComplete));
    }
    // belt-and-suspenders: capital must also be zero at genesis.
    if capital != 0 {
        return Err(ProgramError::from(EarnError::GenesisAlreadyComplete));
    }

    Ok(())
}

pub fn handler(ctx: Context<SeedGenesis>, amount: u64) -> Result<()> {
    // CHECKS + EFFECTS in a scope that drops the EarnConfig borrow guard before
    // the MintTo CPI below. The earn_config PDA is the MintTo mint_authority
    // signer, which the checked invoke rejects while the account is still
    // mutably borrowed. Copy out the bump and amount; the guard's Drop releases
    // the borrow flag at the end of this block.
    let (config_bump, mint_amount);
    {
        EarnConfig::assert_account_size(ctx.accounts.earn_config)?;
        let mut config = EarnConfig::load_mut(ctx.accounts.earn_config, ctx.program_id)?;

        // --- Checks ---
        // bootstrap + amount + one-time gates (supply==0, capital==0).
        let supply = read_mint_supply(ctx.accounts.rwt_mint)?;
        validate_seed_genesis_inputs(
            ctx.accounts.bootstrap_authority.address().as_ref(),
            amount,
            supply,
            config.total_invested_capital,
        )?;

        // rwt_mint must match the config-pinned earn-RWT mint.
        if ctx.accounts.rwt_mint.address().as_ref() != config.rwt_mint.as_ref() {
            return Err(ProgramError::from(EarnError::InvalidRwtMint));
        }
        // recipient must hold the earn-RWT mint.
        let recipient_mint = read_token_account_mint(ctx.accounts.recipient_rwt)?;
        if recipient_mint != config.rwt_mint {
            return Err(ProgramError::from(EarnError::InvalidTokenAccount));
        }

        // --- Effect: capital = amount ---
        // 1:1 with the minted RWT → NAV == INITIAL_NAV ($1.00), since earn-RWT
        // (6dp) and USDC (6dp) share the same scale. No USDC deposit.
        config.total_invested_capital = amount as u128;

        config_bump = config.bump;
        mint_amount = amount;
    } // EarnConfig guard dropped here — borrow flag released before the CPI.

    // --- Interaction: mint `amount` RWT to the treasury, signed by the config PDA ---
    // The EarnConfig borrow guard was dropped at the end of the block above, so
    // passing earn_config as the mint_authority signer here is accepted by the
    // checked invoke (no live AccountRefMut on this account).
    let bump = [config_bump];
    let seeds = [Seed::from(EARN_CONFIG_SEED), Seed::from(bump.as_ref())];
    let signer = Signer::from(&seeds);

    arlex_lang::token::instructions::MintTo {
        mint: ctx.accounts.rwt_mint,
        account: ctx.accounts.recipient_rwt,
        mint_authority: ctx.accounts.earn_config,
        amount: mint_amount,
    }
    .invoke_signed(&[signer])?;

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(GenesisSeeded {
        recipient: {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(ctx.accounts.recipient_rwt.address().as_ref());
            arr
        },
        amount: mint_amount,
        capital: mint_amount as u128,
        nav: INITIAL_NAV,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Genesis-gate validation + NAV-$1 proof. The pure `validate_seed_genesis_inputs`
    //! fn lets the bootstrap / amount / supply==0 / capital==0 gates be exercised
    //! without a BPF runtime (mirrors `initialize`'s test style).
    use super::*;
    use crate::nav::calculate_nav;

    fn custom_code(err: ProgramError) -> u32 {
        match err {
            ProgramError::Custom(code) => code,
            other => panic!("expected ProgramError::Custom, got {:?}", other),
        }
    }

    fn assert_err_code<T>(result: Result<T>, expected: EarnError) {
        let err = match result {
            Ok(_) => panic!("expected error"),
            Err(err) => err,
        };
        assert_eq!(custom_code(err), custom_code(ProgramError::from(expected)));
    }

    /// Valid genesis: bootstrap signer, amount > 0, supply == 0, capital == 0.
    #[test]
    fn valid_genesis_passes() {
        assert!(
            validate_seed_genesis_inputs(BOOTSTRAP_AUTHORITY.as_ref(), 25_000_000_000, 0, 0).is_ok()
        );
        // Smallest legal amount.
        assert!(validate_seed_genesis_inputs(BOOTSTRAP_AUTHORITY.as_ref(), 1, 0, 0).is_ok());
    }

    /// Non-bootstrap signer → UnauthorizedBootstrap.
    #[test]
    fn non_bootstrap_signer_rejected() {
        let wrong = [9u8; 32];
        assert_err_code(
            validate_seed_genesis_inputs(&wrong, 25_000_000_000, 0, 0),
            EarnError::UnauthorizedBootstrap,
        );
    }

    /// amount == 0 → ZeroAmount.
    #[test]
    fn zero_amount_rejected() {
        assert_err_code(
            validate_seed_genesis_inputs(BOOTSTRAP_AUTHORITY.as_ref(), 0, 0, 0),
            EarnError::ZeroAmount,
        );
    }

    /// supply != 0 → GenesisAlreadyComplete (one-time guard).
    #[test]
    fn nonzero_supply_rejected() {
        assert_err_code(
            validate_seed_genesis_inputs(BOOTSTRAP_AUTHORITY.as_ref(), 25_000_000_000, 1, 0),
            EarnError::GenesisAlreadyComplete,
        );
    }

    /// capital != 0 → GenesisAlreadyComplete (belt-and-suspenders guard).
    #[test]
    fn nonzero_capital_rejected() {
        assert_err_code(
            validate_seed_genesis_inputs(BOOTSTRAP_AUTHORITY.as_ref(), 25_000_000_000, 0, 1),
            EarnError::GenesisAlreadyComplete,
        );
    }

    /// NAV $1.00 proof: with capital == amount and supply == amount (the
    /// post-genesis state), Book NAV equals INITIAL_NAV for any amount.
    /// earn-RWT (6dp) and USDC (6dp) share scale → 1:1 → $1.00.
    #[test]
    fn nav_is_one_dollar_after_genesis() {
        for amount in [1u64, 1_000_000, 25_000_000_000, 1_000_000_000_000] {
            let nav = calculate_nav(amount as u128, amount).unwrap();
            assert_eq!(
                nav, INITIAL_NAV,
                "post-genesis NAV must be $1.00 for amount {amount}"
            );
        }
        // Explicit $25k car case: 25,000 RWT backing a $25,000 asset @ $1.00.
        assert_eq!(
            calculate_nav(25_000_000_000u128, 25_000_000_000).unwrap(),
            INITIAL_NAV,
            "$25k genesis → NAV $1.00"
        );
    }
}
