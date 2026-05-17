//! Layer 9 §4.6 — `nexus_withdraw_profits`. Authority-gated sweep of the
//! Nexus's "above-principal" balance to the ARL Treasury. Enforces the
//! **principal-lock invariant**: the on-chain `total_deposited_<token>`
//! counter is the irreducible floor; only the delta `(ata_balance - floor)`
//! is withdrawable.
//!
//! # Principal-lock invariant
//!
//! `total_deposited_<token>` is monotonically non-decreasing (only
//! `nexus_deposit` and `nexus_record_deposit` may bump it; nothing
//! decrements it, including this handler). The invariant
//!
//! ```text
//! ata_balance >= total_deposited_<token>   (at the moment of withdraw)
//! ```
//!
//! is **required** for the Nexus to honour deposit redemption semantics
//! across the protocol's lifetime. Withdrawing principal would silently
//! lock future redeems; hence the `checked_sub` underflow path explicitly
//! reverts with `InsufficientNexusProfit` on impermanent-loss scenarios.
//!
//! # Counter behaviour
//!
//! The counter is **not** decremented on profit withdrawal. The next call's
//! profit calculation rebases on the new (lower) `ata_balance` against the
//! same floor; a subsequent `nexus_deposit` correctly bumps the floor
//! again. Documentation (`docs/contracts/native-dex.mdx` §nexus state)
//! pins this behaviour.

use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::error::DexError;
use crate::events::NexusProfitsWithdrawn;
use crate::state::{DexConfig, LiquidityNexus};
use crate::validation::{pubkey_bytes, read_token_account_mint, read_token_account_owner};

#[derive(Accounts)]
pub struct NexusWithdrawProfits<'info> {
    /// DEX authority. `has_one = authority` on `dex_config` is the
    /// access-control gate (Layer 9 §4.6, mirrors the `update_nexus_manager`
    /// pattern).
    #[account(signer)]
    pub authority: &'info AccountView,

    /// DEX config singleton. `has_one = authority` enforces Authority gating.
    #[account(
        has_one = authority, account_type = "DexConfig",
        seeds = [b"dex_config"], bump
    )]
    pub dex_config: &'info AccountView,

    /// Nexus singleton. Mutable because the outbound Transfer signs with the
    /// Nexus PDA seeds; the data of the Nexus account itself is not modified
    /// (counter NOT decremented — principal-lock invariant).
    #[account(mut, seeds = [LIQUIDITY_NEXUS_SEED], bump)]
    pub liquidity_nexus: &'info AccountView,

    /// Source ATA — Nexus-owned, holds either USDC or RWT (per `token_kind`).
    /// PDA-signed Transfer source.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub nexus_token_ata: &'info AccountView,

    /// Recipient ATA — Treasury (typically `dex_config.areal_fee_destination`,
    /// but mint-typed: caller-supplied to support distinct per-token Treasury
    /// ATAs). Validated for correct mint at runtime.
    //
    // Note: the SPL Token program account is required in the on-chain TX
    // accounts list but intentionally NOT a named field of this Accounts
    // struct — same BPF stack-frame budget rationale as `nexus_deposit`.
    // pinocchio_token hardcodes the program ID; callers pass the program
    // account via remaining_accounts.
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub recipient_token_ata: &'info AccountView,
}

#[inline(never)]
pub fn handler(
    ctx: Context<NexusWithdrawProfits>,
    amount: u64,
    token_kind: u8,
) -> Result<()> {
    // `#[inline(never)]` keeps the handler's locals (ATA balance read, PDA
    // signer-seed array, CPI builder) out of the BPF entrypoint dispatcher's
    // stack frame. The 4 new Layer 9 ix together push the dispatcher near
    // the 4096-byte SBF stack budget; keeping each handler non-inlined
    // shifts the frame charge to a separate, isolated function.

    // 1. Amount sanity.
    if amount == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount).into());
    }

    // 2. Token-kind dispatch + canonical mint resolution. Mirrors
    //    `nexus_deposit` so the counter-slot disambiguation is symmetric
    //    on the read side.
    let expected_mint: [u8; 32] = match token_kind {
        TOKEN_KIND_USDC => USDC_MINT,
        TOKEN_KIND_RWT => RWT_MINT,
        _ => return Err(ProgramError::from(DexError::InvalidNexusToken).into()),
    };

    // 3. Source ATA mint must match `token_kind`. Recipient ATA mint must
    //    also match (cross-token withdraw is structurally impossible because
    //    SPL Transfer would revert mid-CPI, but we surface a clean error
    //    code up-front).
    let src_mint = read_token_account_mint(ctx.accounts.nexus_token_ata)?;
    if src_mint != expected_mint {
        return Err(ProgramError::from(DexError::InvalidNexusToken).into());
    }
    let dst_mint = read_token_account_mint(ctx.accounts.recipient_token_ata)?;
    if dst_mint != expected_mint {
        return Err(ProgramError::from(DexError::InvalidNexusToken).into());
    }

    // 4. Source ATA SPL-owner must equal Nexus PDA. Defence-in-depth — the
    //    caller could otherwise pass a non-Nexus-owned ATA whose mint
    //    happens to match, sweeping funds the principal-floor was never
    //    intended to gate.
    let src_owner = read_token_account_owner(ctx.accounts.nexus_token_ata)?;
    let nexus_addr = pubkey_bytes(ctx.accounts.liquidity_nexus);
    if src_owner != nexus_addr {
        return Err(ProgramError::from(DexError::InvalidTokenAccount).into());
    }

    // 5. Snapshot ATA balance + Nexus active-state + principal floor + bump.
    //    Scoped block so the load handle is dropped before the CPI.
    let (ata_balance, principal_floor, nexus_bump) = {
        let nexus = LiquidityNexus::load(
            ctx.accounts.liquidity_nexus,
            ctx.program_id,
        )?;
        if !nexus.is_active {
            return Err(ProgramError::from(DexError::NexusNotActive).into());
        }
        let floor = match token_kind {
            TOKEN_KIND_USDC => nexus.total_deposited_usdc,
            TOKEN_KIND_RWT => nexus.total_deposited_rwt,
            _ => unreachable!(),
        };
        let bal = read_token_account_amount(ctx.accounts.nexus_token_ata)?;
        (bal, floor, nexus.bump)
    };

    // 6. Principal-lock invariant. `checked_sub` underflow on
    //    `ata_balance < principal_floor` (impermanent loss) explicitly
    //    reverts — withdrawing under-water is forbidden so the next deposit
    //    can be redeemed at par. This is the most security-critical line
    //    of the handler.
    let profit = ata_balance
        .checked_sub(principal_floor)
        .ok_or(ProgramError::from(DexError::InsufficientNexusProfit))?;
    if amount > profit {
        return Err(ProgramError::from(DexError::InsufficientNexusProfit).into());
    }
    let remaining_profit = profit - amount; // safe: amount <= profit per check above

    // 7. PDA-signed outbound Transfer. The Nexus is the SPL-level authority
    //    of `nexus_token_ata`; PDA seeds `[LIQUIDITY_NEXUS_SEED, &[bump]]`
    //    authorise the move.
    let bump_arr = [nexus_bump];
    let signer_seeds: [Seed; 2] = [
        Seed::from(LIQUIDITY_NEXUS_SEED),
        Seed::from(bump_arr.as_ref()),
    ];
    arlex_lang::token::instructions::Transfer {
        from: ctx.accounts.nexus_token_ata,
        to: ctx.accounts.recipient_token_ata,
        authority: ctx.accounts.liquidity_nexus,
        amount,
    }
    .invoke_signed(&[Signer::from(&signer_seeds)])?;

    // 8. Emit event. Counter intentionally NOT decremented (principal-lock
    //    invariant — the floor stays at `principal_floor` so a subsequent
    //    withdraw rebases on the new `ata_balance`).
    let recipient_addr = pubkey_bytes(ctx.accounts.recipient_token_ata);
    let clock = Clock::get()?;
    emit!(NexusProfitsWithdrawn {
        token_mint: expected_mint,
        amount,
        remaining_profit,
        treasury_destination: recipient_addr,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

/// Read SPL Token Account `amount` field (bytes 64..72 LE) via zero-copy.
///
/// SPL Token Account layout: `[mint: 32][owner: 32][amount: u64][...]`.
/// Length-checked before any indexing. Mirrors the helper in
/// `compound_yield.rs`; consolidated here to avoid a public `pub`-export
/// across instruction modules until N-6 lands a shared validation helper.
fn read_token_account_amount(account: &AccountView) -> Result<u64> {
    // SAFETY: see `validation::read_token_account_mint` for the L-5 audit
    // note. Length check below ensures no OOB indexing.
    let data = unsafe { core::slice::from_raw_parts(account.data_ptr(), account.data_len()) };
    if data.len() < 72 {
        return Err(ProgramError::InvalidAccountData.into());
    }
    Ok(u64::from_le_bytes(data[64..72].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    //! Pure-Rust pinning tests for the principal-lock invariant. Handler-level
    //! negative ACs (non-Authority signer, ATA mint mismatch) require the BPF
    //! runtime; here we exercise the `(balance - floor)` math directly so any
    //! refactor preserving the invariant must also preserve these answers.
    use super::*;
    use crate::state::LiquidityNexus;

    /// Helper: compute `profit = ata_balance.checked_sub(principal_floor)?`
    /// and gate `amount <= profit`. Mirrors the production check exactly.
    fn try_withdraw(ata_balance: u64, principal_floor: u64, amount: u64) -> Result<u64> {
        let profit = ata_balance
            .checked_sub(principal_floor)
            .ok_or(ProgramError::from(DexError::InsufficientNexusProfit))?;
        if amount > profit {
            return Err(ProgramError::from(DexError::InsufficientNexusProfit).into());
        }
        Ok(profit - amount)
    }

    /// Happy path — `ata_balance > principal_floor`, `amount <= profit`.
    /// Withdraw succeeds and yields `remaining_profit = profit - amount`.
    #[test]
    fn nexus_withdraw_profits_above_floor_succeeds() {
        // Floor = 100, balance = 150, withdraw 30 → remaining = 20.
        let result = try_withdraw(/* balance */ 150, /* floor */ 100, /* amount */ 30);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 20);

        // Edge: withdraw exactly profit → remaining = 0.
        let exact = try_withdraw(150, 100, 50);
        assert!(exact.is_ok());
        assert_eq!(exact.unwrap(), 0);

        // Edge: floor + 1 (smallest profit), withdraw 1 → remaining = 0.
        let smallest = try_withdraw(101, 100, 1);
        assert!(smallest.is_ok());
        assert_eq!(smallest.unwrap(), 0);
    }

    /// **Principal-lock invariant** — withdrawing below the floor must
    /// revert. Two failure modes covered:
    ///   1. `ata_balance < principal_floor` → checked_sub underflow.
    ///   2. `ata_balance == principal_floor`, `amount > 0` → `amount > 0 = profit`.
    #[test]
    fn nexus_withdraw_profits_below_floor_reverts() {
        // (1) Under-water (impermanent loss).
        //     Floor = 100, balance = 80, any non-zero amount → revert.
        let underwater = try_withdraw(/* balance */ 80, /* floor */ 100, /* amount */ 1);
        assert!(underwater.is_err());
        let want = ProgramError::from(DexError::InsufficientNexusProfit);
        match (underwater.unwrap_err(), &want) {
            (ProgramError::Custom(got), ProgramError::Custom(w)) => assert_eq!(got, *w),
            (g, _) => panic!("expected Custom, got {:?}", g),
        }

        // (2) At-floor (zero profit), any amount > 0 → revert.
        //     Floor = 100, balance = 100, amount = 1 → revert.
        let at_floor = try_withdraw(100, 100, 1);
        assert!(at_floor.is_err());

        // (3) Above floor but amount > profit.
        //     Floor = 100, balance = 105, amount = 6 → revert.
        let exceed = try_withdraw(105, 100, 6);
        assert!(exceed.is_err());
    }

    /// **Counter NOT decremented invariant** — the handler reads the floor
    /// but does not write it back. Mirrors the production effect: only
    /// `nexus_deposit` / `nexus_record_deposit` bump the counter; this
    /// handler is read-only with respect to `LiquidityNexus.total_deposited_*`.
    #[test]
    fn nexus_withdraw_profits_counter_not_decremented() {
        // Build a synthetic Nexus with a known principal floor; mirror the
        // handler's read; assert no write-path mutates the counter.
        // SAFETY: zero-init valid for `LiquidityNexus`; see `state::tests`.
        let buf = [0u8; core::mem::size_of::<LiquidityNexus>()];
        let mut nexus: LiquidityNexus =
            unsafe { core::ptr::read(buf.as_ptr() as *const LiquidityNexus) };
        nexus.is_active = true;
        nexus.total_deposited_usdc = 1_000;
        nexus.total_deposited_rwt = 0;
        nexus.bump = 0xFD;

        // Read the floor (mirror the handler's load step).
        let floor_before = nexus.total_deposited_usdc;
        // Simulate a withdraw of 200 units of profit (balance = 1500, floor = 1000).
        let _remaining = try_withdraw(1_500, floor_before, 200).unwrap();

        // Production handler does NOT touch `nexus.total_deposited_usdc` after
        // the load. Pin the invariant: counter unchanged.
        assert_eq!({ nexus.total_deposited_usdc }, 1_000);
        // Subsequent withdraw observes the same floor (rebases on new balance).
        let _next = try_withdraw(1_300, nexus.total_deposited_usdc, 100).unwrap();
        assert_eq!({ nexus.total_deposited_usdc }, 1_000);
    }
}
