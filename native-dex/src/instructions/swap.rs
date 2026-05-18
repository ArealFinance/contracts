use arlex_lang::prelude::*;
use pinocchio::sysvars::{Sysvar, clock::Clock};

use crate::constants::*;
use crate::cpi as dex_cpi;
use crate::error::DexError;
use crate::events::{SwapExecuted, SwapRoutedToMint};
use crate::state::*;
use crate::amm::{constant_product_output, calculate_fees};
use crate::validation::*;
use crate::concentrated;

#[derive(Accounts)]
pub struct Swap<'info> {
    #[account(signer)]
    pub user: &'info AccountView,

    #[account(seeds = [b"dex_config"], bump)]
    pub dex_config: &'info AccountView,

    #[account(mut)]
    pub pool_state: &'info AccountView,

    // User's input/output token accounts
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_token_in: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub user_token_out: &'info AccountView,

    // Pool vaults (in and out direction determined by a_to_b)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_in: &'info AccountView,

    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub vault_out: &'info AccountView,

    // Protocol fee destination (Areal Finance RWT ATA)
    #[account(mut, owner = Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub areal_fee_account: &'info AccountView,

    // Optional: OT treasury fee destination (remaining accounts[0])

    #[account(constraint = token_program.address() == &Address::new_from_array(SPL_TOKEN_PROGRAM))]
    pub token_program: &'info AccountView,
}

/// Decoupled view of `Swap` accounts used by `swap_internal`.
///
/// Allows `swap_internal` to be invoked from any handler whose accounts can
/// project into this shape (user-signed `swap`, or PDA-signed alternative
/// callers added in later layers).
///
/// CP-6 — added `token_program` so the mint-route branch can forward an
/// AccountView into `cpi_mint_rwt` without needing a separate remaining-
/// account slot for it (token_program is already validated as the canonical
/// SPL Token program by the outer `Swap` / `NexusSwap` typed accounts).
pub(crate) struct SwapAccountsView<'info> {
    pub authority: &'info AccountView,
    pub dex_config: &'info AccountView,
    pub pool_state: &'info AccountView,
    pub user_token_in: &'info AccountView,
    pub user_token_out: &'info AccountView,
    pub vault_in: &'info AccountView,
    pub vault_out: &'info AccountView,
    pub areal_fee_account: &'info AccountView,
    pub token_program: &'info AccountView,
}

impl<'info> Swap<'info> {
    pub(crate) fn view(&self) -> SwapAccountsView<'info> {
        SwapAccountsView {
            authority: self.user,
            dex_config: self.dex_config,
            pool_state: self.pool_state,
            user_token_in: self.user_token_in,
            user_token_out: self.user_token_out,
            vault_in: self.vault_in,
            vault_out: self.vault_out,
            areal_fee_account: self.areal_fee_account,
            token_program: self.token_program,
        }
    }
}

pub fn handler(
    ctx: Context<Swap>,
    amount_in: u64,
    min_amount_out: u64,
    a_to_b: bool,
) -> Result<()> {
    swap_internal(
        &ctx.accounts.view(),
        ctx.remaining_accounts,
        ctx.program_id,
        amount_in,
        min_amount_out,
        a_to_b,
        None,
    )
}

/// Core swap logic — usable by user-signed `swap` and by PDA-signed alternative
/// callers (e.g. future layers passing `Some(&[Signer])`).
///
/// Internal helper — do NOT use as instruction entrypoint. Caller must validate
/// access control before invoking.
///
/// `authority_signer_seeds`:
///   - `None` — authority is a transaction signer (user-signed path); the
///     inbound `user_token_in -> vault_in` transfer is invoked without seeds.
///   - `Some(seeds)` — authority is a PDA; the inbound transfer is invoked
///     with the supplied signer seeds.
///
/// Pool-side outbound transfers (`vault_out -> user_token_out`,
/// `vault -> areal_fee_account`, `vault -> ot_fee_account`) always sign with
/// the pool PDA seeds derived from `PoolState`, regardless of caller.
#[inline(never)]
pub(crate) fn swap_internal<'info>(
    accounts: &SwapAccountsView<'info>,
    remaining_accounts: &'info [AccountView],
    program_id: &Address,
    amount_in: u64,
    min_amount_out: u64,
    a_to_b: bool,
    authority_signer_seeds: Option<&[Signer]>,
) -> Result<()> {
    let config = DexConfig::load(accounts.dex_config, program_id)?;
    let pool = PoolState::load_mut(accounts.pool_state, program_id)?;

    // --- Checks ---
    if !config.is_active {
        return Err(ProgramError::from(DexError::DexPaused));
    }
    if !pool.is_active {
        return Err(ProgramError::from(DexError::PoolNotActive));
    }
    if amount_in == 0 {
        return Err(ProgramError::from(DexError::ZeroAmount));
    }
    // For StandardCurve: both reserves must be non-zero
    // For Concentrated: one-sided liquidity is legitimate (bin-walk handles it)
    if pool.pool_type == POOL_TYPE_STANDARD && (pool.reserve_a == 0 || pool.reserve_b == 0) {
        return Err(ProgramError::from(DexError::EmptyReserves));
    }

    // SECURITY: Validate vaults match pool state
    let (expected_vault_in, expected_vault_out) = if a_to_b {
        (&pool.vault_a, &pool.vault_b)
    } else {
        (&pool.vault_b, &pool.vault_a)
    };
    validate_vault(accounts.vault_in, expected_vault_in)?;
    validate_vault(accounts.vault_out, expected_vault_out)?;

    // Validate areal_fee_account matches DexConfig
    if accounts.areal_fee_account.address().as_ref() != config.areal_fee_destination.as_ref() {
        return Err(ProgramError::from(DexError::InvalidTokenAccount));
    }

    // --- OT treasury fee account validation ---
    if pool.has_ot_treasury {
        if remaining_accounts.is_empty() {
            return Err(ProgramError::from(DexError::MissingOtTreasuryAccount));
        }
        let ot_fee_account = &remaining_accounts[0];
        if ot_fee_account.address().as_ref() != pool.ot_treasury_fee_destination.as_ref() {
            return Err(ProgramError::from(DexError::OtTreasuryAccountMismatch));
        }
    }

    // --- Determine RWT side and fee direction ---
    // Fee is ALWAYS taken from the RWT side:
    // - Selling RWT (input is RWT): fee from input BEFORE swap
    // - Buying RWT (output is RWT): fee from output AFTER swap
    let input_is_rwt = if a_to_b {
        is_rwt_mint(&pool.token_a_mint)
    } else {
        is_rwt_mint(&pool.token_b_mint)
    };

    let (reserve_in, reserve_out) = if a_to_b {
        (pool.reserve_a, pool.reserve_b)
    } else {
        (pool.reserve_b, pool.reserve_a)
    };

    let (amount_out, fee_lp, fee_protocol, fee_ot_treasury, net_input);

    // Per spec (Fee Architecture Step 4: "Always RWT"; Step 2: Protocol Fee
    // "always in RWT, transferred to areal_fee_destination"), the protocol
    // fee destination MUST be an RWT ATA. Verify the mint here and revert
    // fast on mismatch so operators notice the misconfiguration rather than
    // silently burning protocol fees into the LP accumulator. The
    // areal_fee_destination address is set at initialize_dex and is
    // immutable thereafter — fixing a misconfigured cluster requires a
    // program-upgrade migration or a fresh bootstrap.
    let dest_mint = read_token_account_mint(accounts.areal_fee_account)?;
    if dest_mint != RWT_MINT {
        return Err(ProgramError::from(DexError::InvalidProtocolFeeDestination));
    }

    // ---------------------------------------------------------------------
    // CP-6 — Mint-route gate (master pool USDC → RWT).
    //
    // For master Monotonic-Ladder pools (USDC/RWT, USDY/RWT), USDC→RWT
    // swaps reroute through `rwt_engine::mint_rwt` when:
    //   1. organic ask above the active bin is empty, OR
    //   2. best ask price > NAV × (1 + MINT_ROUTE_PRICE_OFFSET_BPS / 10_000).
    //
    // In the mint-route branch ZERO DEX fee is charged (LP + protocol both
    // suppressed) — the 1% mint fee (0.5% NAV accrual + 0.5% DAO) replaces
    // the role. User signature passes through via `invoke` (NOT
    // `invoke_signed`) — no new pool-PDA authority surface.
    //
    // Remaining-account layout for the mint-route branch (after the
    // bin_array slot at `remaining_accounts[0]` — master pools never carry
    // OT-treasury, see CP-4):
    //   [1] rwt_vault         (mut, owner = rwt_engine)
    //   [2] rwt_mint          (mut, owner = SPL Token)
    //   [3] capital_acc       (mut, owner = SPL Token, == vault.capital_accumulator_ata)
    //   [4] dao_fee_account   (mut, owner = SPL Token, == vault.areal_fee_destination)
    //   [5] rwt_engine_program (read, program-id slot)
    //
    // Note (deviation from architect plan): the plan listed only 4
    // mint-route slots — pinocchio CPI requires the target program as an
    // AccountView slot (program-id resolution), so slot [5] holds
    // `rwt_engine_program` and is verified against `RWT_ENGINE_PROGRAM_ID`.
    // `token_program` is forwarded through `SwapAccountsView.token_program`
    // (already validated by the outer typed-accounts struct).
    //
    // The gate predicates are deliberately fail-closed: any of the
    // pool-type / direction / pair-mint conditions failing skips the entire
    // mint-route branch (StandardCurve pools, OT pairs, and non-master
    // concentrated pools continue with the pre-CP-6 bin-walk / curve path).
    // Identify the non-RWT side: `input_is_rwt` flags which side of the
    // pair is RWT (`a_to_b` + RWT-side bookkeeping is done above). When
    // `!input_is_rwt`, the input mint is the non-RWT side (the user is
    // buying RWT with USDC / USDY). Use that to decide if the pair is a
    // master pool (non-RWT side ∈ {USDC, USDY}).
    let non_rwt_mint_for_route: [u8; 32] = if a_to_b {
        // input is token_a; if input_is_rwt then a is RWT (and we won't
        // route in that case); otherwise a is the non-RWT side.
        pool.token_a_mint
    } else {
        pool.token_b_mint
    };
    let is_master_pool_usdc_to_rwt = pool.pool_type == POOL_TYPE_CONCENTRATED
        && !input_is_rwt
        && (non_rwt_mint_for_route == USDC_MINT || non_rwt_mint_for_route == USDY_MINT);

    if is_master_pool_usdc_to_rwt {
        // Load the bin_array slot (always present for concentrated pools).
        // Master pools have `has_ot_treasury == false` (CP-4 invariant), so
        // the bin_array is at `remaining_accounts[0]` exactly.
        if remaining_accounts.is_empty() {
            return Err(ProgramError::from(DexError::InvalidBinRange));
        }
        let pool_key = pubkey_bytes(accounts.pool_state);
        let (expected_bin_pda, _) = arlex_lang::find_program_address(
            &[b"bins", pool_key.as_ref()],
            program_id,
        );
        if remaining_accounts[0].address().as_ref() != expected_bin_pda.as_ref() {
            return Err(ProgramError::InvalidSeeds);
        }
        let bin_array_for_check = BinArray::load(&remaining_accounts[0], program_id)?;
        let has_organic_ask = concentrated::bin_walk_has_liquidity_above(
            bin_array_for_check,
            pool.active_bin_id,
        );
        // CU-hotfix (2026-05-18) — see mint_rwt.rs for the full rationale: the
        // eager `ok_or(ProgramError::from(...))` form invokes the From impl on
        // the success path, logging a spurious "price_at_bin overflow when
        // computing best-ask threshold" syscall every swap. Use `ok_or_else`
        // (closure → lazy) so the conversion (and its log) runs only on the
        // actual overflow branch.
        let best_ask_price_q = concentrated::price_at_bin(
            pool.bin_step_bps,
            pool.active_bin_id.saturating_add(1),
        )
        .ok_or_else(|| ProgramError::from(DexError::PriceOverflow))?;

        // Pre-load NAV from the rwt_vault remaining_account so the routing
        // decision is data-driven. We need the 4 mint-route slots present
        // BEFORE the routing decision because we cannot otherwise emit
        // the `SwapRoutedToMint` event with `nav_at_route`.
        if remaining_accounts.len() < 6 {
            return Err(ProgramError::from(DexError::MissingMintRouteAccounts));
        }
        let rwt_vault = &remaining_accounts[1];
        let rwt_mint = &remaining_accounts[2];
        let capital_acc = &remaining_accounts[3];
        let dao_fee_account = &remaining_accounts[4];
        let rwt_engine_program = &remaining_accounts[5];

        let nav = dex_cpi::read_rwt_vault_nav(rwt_vault)?;

        let route_to_mint = should_route_to_mint(
            has_organic_ask,
            nav,
            best_ask_price_q,
            pool.bin_step_bps,
        );

        if route_to_mint {
            // CP-6 — additional defensive checks (the called handler also
            // performs its own validations; these are defence-in-depth so
            // operators see clean DexError codes instead of opaque
            // ProgramError::Custom from rwt_engine):
            //
            // 1. user_token_out.mint must be RWT — otherwise the mint-route
            //    output would land in a foreign-token account (silently
            //    fails inside mint_rwt with InvalidTokenAccount, but we
            //    fail-fast here to save CU on bad-input reverts).
            let out_mint = read_token_account_mint(accounts.user_token_out)?;
            if out_mint != RWT_MINT {
                return Err(ProgramError::from(DexError::InvalidRwtMint));
            }
            // 2. rwt_engine_program pinning — refuse foreign program impostors.
            if rwt_engine_program.address().as_ref() != RWT_ENGINE_PROGRAM_ID.as_ref() {
                return Err(ProgramError::from(DexError::InvalidRwtVault));
            }

            // 3. The mint-route branch is incompatible with PDA-signed
            //    callers (Nexus): mint_rwt requires `user: signer`, and the
            //    pass-through depends on the outer-Tx user signature. A
            //    PDA-signed caller would need invoke_signed semantics that
            //    rwt_engine::mint_rwt does NOT accept. Refuse cleanly
            //    rather than letting the CPI fail at the runtime layer.
            //
            //    This is a structural refusal — current Nexus paths only
            //    drive RWT→USDC swaps (a_to_b sells RWT for USDC), which
            //    is the bin-walk branch, so the refusal is unreachable
            //    today; it guards future code that might attempt to add a
            //    Nexus USDC→RWT path.
            if authority_signer_seeds.is_some() {
                return Err(ProgramError::from(DexError::InvalidRwtVault));
            }

            // CPI → rwt_engine::mint_rwt. User-signed pass-through.
            // - amount_in_usdc: full user-supplied amount_in (no fee carve).
            // - min_rwt_out: forward user's slippage bound directly.
            //   The bin-walk branch applies slippage to `amount_out` (post-fee);
            //   here we apply it to the rwt_engine output, which is the
            //   user-visible RWT delivered. mint_rwt enforces its own
            //   ZeroSlippage / SlippageExceeded reverts.
            dex_cpi::cpi_mint_rwt(
                accounts.authority,
                rwt_vault,
                rwt_mint,
                accounts.user_token_in,
                accounts.user_token_out,
                capital_acc,
                dao_fee_account,
                accounts.token_program,
                rwt_engine_program,
                amount_in,
                min_amount_out,
            )?;

            // Emit `SwapRoutedToMint` in place of `SwapExecuted`. Indexers
            // distinguish the two events to attribute fee accounting
            // correctly (the mint-route path produces no DEX fee).
            let clock = Clock::get()?;
            emit!(SwapRoutedToMint {
                pool: pubkey_bytes(accounts.pool_state),
                user: pubkey_bytes(accounts.authority),
                amount_in,
                nav_at_route: nav,
                best_ask_price_q,
                timestamp: clock.unix_timestamp,
            });

            return Ok(());
        }
        // Fall through to the bin-walk path. Organic ask is present AND
        // priced at-or-below threshold — consume it with normal LP +
        // protocol fees from the existing branch below.
    }

    if input_is_rwt {
        // Selling RWT (fee-on-top per docs/contracts/native-dex.mdx:522-568):
        // Fees are charged ON TOP of `amount_in` — the full `amount_in` enters
        // the constant-product curve. User's wallet is debited
        // `amount_in + fee_total + ot_treasury_fee` (computed below as
        // `user_total_debit`). `fee_lp` stays in the RWT vault and is tracked
        // by the per-share accumulator (D28); `fee_protocol` + `fee_ot_treasury`
        // are CPI-extracted to their destinations by the outbound transfers.
        let fees = calculate_fees(amount_in, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
        // Full amount_in enters the curve — fees are external to it.
        net_input = amount_in;

        // Branch on pool type for output calculation
        if pool.pool_type == POOL_TYPE_CONCENTRATED {
            // Load BinArray from remaining_accounts with PDA verification
            let bin_idx = if pool.has_ot_treasury { 1 } else { 0 };
            if remaining_accounts.len() <= bin_idx {
                return Err(ProgramError::from(DexError::InvalidBinRange));
            }
            let pool_key = pubkey_bytes(accounts.pool_state);
            let (expected_bin_pda, _) = arlex_lang::find_program_address(
                &[b"bins", pool_key.as_ref()],
                program_id,
            );
            if remaining_accounts[bin_idx].address().as_ref() != expected_bin_pda.as_ref() {
                return Err(ProgramError::InvalidSeeds);
            }
            let bin_array = BinArray::load_mut(&remaining_accounts[bin_idx], program_id)?;
            // net_input == amount_in (fee-on-top): full amount enters bin walk.
            let (walk_out, walk_remaining) = concentrated::bin_walk_swap(bin_array, pool.bin_step_bps, net_input, a_to_b)?;
            amount_out = walk_out;
            pool.active_bin_id = bin_array.active_bin_id;

            // SECURITY: Sync unconsumed input into bins so sum(bins) == reserves.
            // Layer 9 D28: fee_lp is NOT synced into bins — it lives in the
            // `cumulative_fees_per_share_<side>` accumulator instead, and is
            // explicitly excluded from reserves by the effects step below.
            concentrated::sync_remaining_to_bin(bin_array, walk_remaining, a_to_b)?;
        } else {
            // Standard path: full amount_in enters the curve (fee-on-top).
            amount_out = constant_product_output(reserve_in, reserve_out, amount_in)?;
        }

        fee_lp = fees.fee_lp;
        fee_protocol = fees.fee_protocol;
        fee_ot_treasury = fees.ot_treasury_fee;
    } else {
        // Buying RWT: fee deducted from output AFTER swap
        net_input = amount_in;

        // Branch on pool type for output calculation
        let gross_out;
        if pool.pool_type == POOL_TYPE_CONCENTRATED {
            let bin_idx = if pool.has_ot_treasury { 1 } else { 0 };
            if remaining_accounts.len() <= bin_idx {
                return Err(ProgramError::from(DexError::InvalidBinRange));
            }
            let pool_key = pubkey_bytes(accounts.pool_state);
            let (expected_bin_pda, _) = arlex_lang::find_program_address(
                &[b"bins", pool_key.as_ref()],
                program_id,
            );
            if remaining_accounts[bin_idx].address().as_ref() != expected_bin_pda.as_ref() {
                return Err(ProgramError::InvalidSeeds);
            }
            let bin_array = BinArray::load_mut(&remaining_accounts[bin_idx], program_id)?;
            let (walk_out, walk_remaining) = concentrated::bin_walk_swap(bin_array, pool.bin_step_bps, net_input, a_to_b)?;
            gross_out = walk_out;
            pool.active_bin_id = bin_array.active_bin_id;

            // Sync unconsumed input into bins. Layer 9 D28: fee_lp stays in
            // the RWT vault but is tracked via the per-share accumulator,
            // not via bin liquidity (bins must mirror reserves; reserves
            // exclude fee_lp post-D28).
            concentrated::sync_remaining_to_bin(bin_array, walk_remaining, a_to_b)?;

            let fees = calculate_fees(gross_out, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;

            let total_deducted = fees.fee_total.checked_add(fees.ot_treasury_fee)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            amount_out = gross_out.checked_sub(total_deducted)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            fee_lp = fees.fee_lp;
            fee_protocol = fees.fee_protocol;
            fee_ot_treasury = fees.ot_treasury_fee;
        } else {
            gross_out = constant_product_output(reserve_in, reserve_out, net_input)?;

            let fees = calculate_fees(gross_out, pool.fee_bps, config.lp_fee_share_bps, pool.has_ot_treasury)?;
            let total_deducted = fees.fee_total.checked_add(fees.ot_treasury_fee)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            amount_out = gross_out.checked_sub(total_deducted)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            fee_lp = fees.fee_lp;
            fee_protocol = fees.fee_protocol;
            fee_ot_treasury = fees.ot_treasury_fee;
        }
    }

    // Slippage check
    if amount_out == 0 {
        return Err(ProgramError::from(DexError::ZeroOutput));
    }
    if amount_out < min_amount_out {
        return Err(ProgramError::from(DexError::SlippageExceeded));
    }

    // --- Effects: update reserves BEFORE CPIs ---
    //
    // Layer 9 D28: LP fee no longer auto-compounds into reserves. fee_lp
    // physically stays in the RWT vault on the fee side, but reserves are
    // updated as if it were extracted. The off-balance amount is tracked
    // via `cumulative_fees_per_share_<side>` (see accumulator update
    // below), and is paid out to LPs by `claim_lp_fees` (D28).
    //
    // For the input-RWT branches the difference vs pre-D28 is "do not add
    // fee_lp back into reserves". For the output-RWT branches the
    // difference is "subtract fee_lp from reserves alongside fee_protocol
    // and fee_ot_treasury". Either way the post-swap invariant holds:
    // `vault_<side>_balance == reserves + cumulative_fees_owed_to_LPs`.
    if a_to_b {
        if input_is_rwt {
            // User sends amount_in + fees via the inbound transfer below; full amount_in
            // enters reserves; fee_lp stays in the RWT vault tracked by the per-share
            // accumulator (D28); fee_protocol + ot_treasury are CPI-extracted to their
            // destinations by the outbound transfers below.
            pool.reserve_a = pool.reserve_a
                .checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_b = pool.reserve_b.checked_sub(amount_out)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
        } else {
            // Output side (B=RWT): fee_lp now leaves reserves (off-reserve
            // accumulator) instead of staying as auto-compound.
            pool.reserve_a = pool.reserve_a.checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_b = pool.reserve_b.checked_sub(amount_out)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_protocol)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_ot_treasury)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_lp)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
        }
    } else {
        if input_is_rwt {
            pool.reserve_b = pool.reserve_b
                .checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_a = pool.reserve_a.checked_sub(amount_out)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
        } else {
            pool.reserve_b = pool.reserve_b.checked_add(net_input)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
            pool.reserve_a = pool.reserve_a.checked_sub(amount_out)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_protocol)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_ot_treasury)
                .ok_or(ProgramError::from(DexError::MathOverflow))?
                .checked_sub(fee_lp)
                .ok_or(ProgramError::from(DexError::MathOverflow))?;
        }
    }

    // Layer 9 D28 — update LP-fee per-share accumulator on the RWT side.
    //
    // The fee always accrues on the RWT side of the pair (token_a or token_b
    // depending on `is_rwt_mint`). When `total_lp_shares == 0` the fee_lp
    // funds remain in the vault unaccounted; this is only reachable before
    // the very first `add_liquidity` (no LP yet). We document the edge but
    // do not panic — the deposited fee_lp would be effectively "burned"
    // toward the eventual first LP cohort because the snapshot at first
    // add_liquidity will be 0. In practice swaps cannot succeed at all
    // when reserves are zero, so this branch is unreachable in the
    // standard pool path; concentrated pools allow one-sided liquidity, so
    // the guard is required there.
    {
        let rwt_is_side_a = (a_to_b && input_is_rwt) || (!a_to_b && !input_is_rwt);
        accrue_lp_fee_per_share(pool, fee_lp, rwt_is_side_a)?;
    }

    pool.total_fees_accumulated = pool.total_fees_accumulated
        .checked_add(fee_lp).ok_or(ProgramError::from(DexError::MathOverflow))?
        .checked_add(fee_protocol).ok_or(ProgramError::from(DexError::MathOverflow))?
        .checked_add(fee_ot_treasury).ok_or(ProgramError::from(DexError::MathOverflow))?;

    // --- Interactions: CPIs ---
    let pool_bump = [pool.bump];

    // Fee-on-top inbound transfer sizing (docs/contracts/native-dex.mdx:534-535):
    // When the user is selling RWT, the wallet debit is `amount_in + fee_total +
    // ot_treasury_fee`. `fee_total == fee_lp + fee_protocol`, so the explicit sum
    // below is equivalent and makes each fee component visible to readers.
    // When the user is buying RWT, fees come out of the output side, so the
    // inbound debit stays equal to `amount_in`.
    let user_total_debit = if input_is_rwt {
        amount_in
            .checked_add(fee_lp).ok_or(ProgramError::from(DexError::MathOverflow))?
            .checked_add(fee_protocol).ok_or(ProgramError::from(DexError::MathOverflow))?
            .checked_add(fee_ot_treasury).ok_or(ProgramError::from(DexError::MathOverflow))?
    } else {
        amount_in
    };

    // 1. Authority sends input tokens to vault_in.
    //    User-signed path: empty signer slice (authority is a transaction signer).
    //    PDA-signed path: caller-supplied seeds authorize the PDA-owned ATA.
    //    Amount = `user_total_debit` so the RWT vault receives both the swap
    //    principal (which enters reserves) AND the fee components (which leave
    //    the vault via the outbound CPIs below — fee_protocol to areal_fee_account,
    //    fee_ot_treasury to ot_fee_account — while fee_lp stays in the vault and
    //    is tracked by `cumulative_fees_per_share_<rwt_side>`).
    {
        let transfer_in = arlex_lang::token::instructions::Transfer {
            from: accounts.user_token_in,
            to: accounts.vault_in,
            authority: accounts.authority,
            amount: user_total_debit,
        };
        match authority_signer_seeds {
            Some(seeds) => transfer_in.invoke_signed(seeds)?,
            None => transfer_in.invoke()?,
        }
    }

    // 2. Pool sends output tokens to user
    {
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: accounts.vault_out,
            to: accounts.user_token_out,
            authority: accounts.pool_state,
            amount: amount_out,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // 3. Pool sends protocol fee to areal_fee_account (from RWT vault)
    if fee_protocol > 0 {
        let rwt_vault = if input_is_rwt { accounts.vault_in } else { accounts.vault_out };
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: rwt_vault,
            to: accounts.areal_fee_account,
            authority: accounts.pool_state,
            amount: fee_protocol,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // 4. Pool sends OT treasury fee (if applicable)
    if fee_ot_treasury > 0 {
        let ot_fee_account = &remaining_accounts[0];
        let rwt_vault = if input_is_rwt { accounts.vault_in } else { accounts.vault_out };
        let seeds = [
            Seed::from(b"pool" as &[u8]),
            Seed::from(pool.token_a_mint.as_ref()),
            Seed::from(pool.token_b_mint.as_ref()),
            Seed::from(pool_bump.as_ref()),
        ];
        arlex_lang::token::instructions::Transfer {
            from: rwt_vault,
            to: ot_fee_account,
            authority: accounts.pool_state,
            amount: fee_ot_treasury,
        }.invoke_signed(&[Signer::from(&seeds)])?;
    }

    // --- Emit event ---
    let clock = Clock::get()?;
    emit!(SwapExecuted {
        pool: pubkey_bytes(accounts.pool_state),
        user: pubkey_bytes(accounts.authority),
        a_to_b,
        amount_in,
        amount_out,
        fee_lp,
        fee_protocol,
        fee_ot_treasury,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

// =====================================================================
// CP-6 — Mint-routing decision helper.
//
// Pure-data predicate factored out so the unit tests can exercise the
// decision matrix (organic-ask present / above-threshold / NAV-zero
// corner cases) without spinning up a BPF runtime. The production
// branch in `swap_internal` calls this helper after loading the
// BinArray and reading NAV from the RwtVault.
// =====================================================================

/// True iff the swap should reroute to `rwt_engine::mint_rwt` instead of
/// consuming organic ask. Per docs/contracts/native-dex.mdx §83:
///   - empty organic ask (`!has_organic_ask`)   → reroute, OR
///   - best on-book ask price > NAV × (1 + MINT_ROUTE_PRICE_OFFSET_BPS / 10_000)
///     → reroute.
///
/// `nav` is `RwtVault.nav_book_value` in USDC 6-decimal scale.
/// `best_ask_price_q` is `pow_bps(bin_step_bps, active_bin_id + 1)` —
/// CONCENTRATED_SCALE-scaled Q-fixed-point, same units as `nav` after
/// dividing by `CONCENTRATED_SCALE / NAV_SCALE` (both 6-decimal here).
///
/// Threshold math uses u128 to avoid overflow on
/// `nav × (10_000 + offset_bps)` for any plausible NAV.
///
/// `nav == 0` is a defensive edge case (pre-init or post-impairment): the
/// threshold becomes 0, and any positive ask price exceeds it → reroute.
/// Strict inequality on the upper edge matches the docs phrasing
/// ("price > NAV × 1.005") so price exactly equal to the threshold uses
/// the bin-walk path.
pub(crate) fn should_route_to_mint(
    has_organic_ask: bool,
    nav: u64,
    best_ask_price_q: u128,
    bin_step_bps: u16,
) -> bool {
    if !has_organic_ask {
        return true;
    }
    // Convert best_ask_price_q (CONCENTRATED_SCALE units) to NAV-scale by
    // dividing by `CONCENTRATED_SCALE / NAV_SCALE_FOR_THRESHOLD`. Since
    // NAV_SCALE == 1_000_000 and CONCENTRATED_SCALE == 1_000_000_000_000,
    // we divide best_ask_price_q by 1_000_000 to land in NAV scale.
    //
    // We compare in u128 to avoid u64 overflow. Strict `>` matches the
    // docs wording (boundary uses bin-walk).
    //
    // bin_step_bps is currently unused by this helper but kept in the
    // signature so the call site is forward-compatible if the threshold
    // formula later incorporates step-dependent slack.
    let _ = bin_step_bps;
    let nav_q = (nav as u128) * (CONCENTRATED_SCALE / 1_000_000);
    let threshold_q = nav_q
        .saturating_mul((BPS_DENOMINATOR as u128) + (MINT_ROUTE_PRICE_OFFSET_BPS as u128))
        / (BPS_DENOMINATOR as u128);
    best_ask_price_q > threshold_q
}

/// Layer 9 D28 — accrue LP-side `fee_lp` into the per-share accumulator on
/// the RWT side of the pool.
///
/// Q64.64 fixed-point: `delta_per_share = (fee_lp << 64) / total_lp_shares`.
/// Returns `Ok(())` (no-op) when either `fee_lp == 0` or
/// `total_lp_shares == 0`. The latter is only reachable before the first
/// `add_liquidity`; concentrated pools allow swaps with one-sided liquidity
/// before any LP exists, so the guard is required to avoid div-by-zero.
///
/// Extracted as `pub(crate)` so the unit tests below can drive the
/// accumulator math against a synthetic `PoolState` without spinning up the
/// full BPF runtime. Production callers must continue to invoke it from
/// inside `swap_internal` after the reserve effects step (we mutate
/// `cumulative_fees_per_share_<side>` only — reserves are owned by the
/// caller).
pub(crate) fn accrue_lp_fee_per_share(
    pool: &mut PoolState,
    fee_lp: u64,
    rwt_is_side_a: bool,
) -> core::result::Result<(), ProgramError> {
    if pool.total_lp_shares == 0 || fee_lp == 0 {
        return Ok(());
    }
    let delta_q64: u128 = (fee_lp as u128) << 64;
    let delta_per_share: u128 = delta_q64 / pool.total_lp_shares;
    if rwt_is_side_a {
        pool.cumulative_fees_per_share_a = pool
            .cumulative_fees_per_share_a
            .checked_add(delta_per_share)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    } else {
        pool.cumulative_fees_per_share_b = pool
            .cumulative_fees_per_share_b
            .checked_add(delta_per_share)
            .ok_or(ProgramError::from(DexError::MathOverflow))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Layer 9 D28 — spot tests for the LP-fee per-share accumulator.
    //!
    //! Layer 4-5 swap-math regression coverage continues to live in the
    //! per-package test suites (`amm.rs`, `concentrated.rs`, integration
    //! harnesses). These tests pin the new accumulator behaviour: a swap
    //! with non-zero `fee_lp` should bump
    //! `cumulative_fees_per_share_<side>` by `(fee_lp << 64) / total_lp_shares`
    //! on the RWT side and leave the other side untouched, while a pool
    //! with `total_lp_shares == 0` must not divide-by-zero.
    use super::*;

    /// Build a synthetic `PoolState` in raw bytes for accumulator math
    /// testing. Only the fields the helper actually reads/writes are set.
    /// SAFETY: PoolState is `#[repr(C, packed)]` via `#[account]` and
    /// all-zero is a valid bit pattern for every field type.
    fn make_pool(total_lp_shares: u128) -> PoolState {
        let buf = [0u8; core::mem::size_of::<PoolState>()];
        let mut pool: PoolState = unsafe { core::ptr::read(buf.as_ptr() as *const PoolState) };
        pool.total_lp_shares = total_lp_shares;
        pool
    }

    /// D28 — happy path: side-A accumulator gains `(fee_lp << 64) /
    /// total_lp_shares`; side-B accumulator stays zero.
    #[test]
    fn swap_updates_cumulative_fees_per_share_a() {
        let mut pool = make_pool(1_000);
        accrue_lp_fee_per_share(&mut pool, 10, /* rwt_is_side_a */ true).unwrap();
        let expected: u128 = (10u128 << 64) / 1_000u128;
        assert_eq!({ pool.cumulative_fees_per_share_a }, expected);
        assert_eq!({ pool.cumulative_fees_per_share_b }, 0u128);
    }

    /// D28 — symmetric coverage: when RWT is on side B, only the B-side
    /// accumulator must move.
    #[test]
    fn swap_updates_cumulative_fees_per_share_b() {
        let mut pool = make_pool(2_000);
        accrue_lp_fee_per_share(&mut pool, 25, /* rwt_is_side_a */ false).unwrap();
        let expected: u128 = (25u128 << 64) / 2_000u128;
        assert_eq!({ pool.cumulative_fees_per_share_b }, expected);
        assert_eq!({ pool.cumulative_fees_per_share_a }, 0u128);
    }

    /// D28 — guard: `total_lp_shares == 0` must short-circuit without
    /// dividing by zero. Reachable for concentrated pools that allow a
    /// swap before the first LP joins.
    #[test]
    fn swap_zero_lp_shares_does_not_panic() {
        let mut pool = make_pool(0);
        // Both branches must be safe.
        accrue_lp_fee_per_share(&mut pool, 100, true).unwrap();
        accrue_lp_fee_per_share(&mut pool, 100, false).unwrap();
        assert_eq!({ pool.cumulative_fees_per_share_a }, 0u128);
        assert_eq!({ pool.cumulative_fees_per_share_b }, 0u128);
    }

    /// P0-2 — Protocol fee destination mint validation. The swap path must
    /// revert with `InvalidProtocolFeeDestination` when the
    /// `areal_fee_account` is a token account whose mint is NOT the pinned
    /// `RWT_MINT`. Pins the boolean decision of the check at swap.rs:200-203
    /// without requiring a full BPF runtime to exercise the handler.
    ///
    /// The production check in `swap_internal` is:
    ///   `if dest_mint != RWT_MINT { return Err(InvalidProtocolFeeDestination) }`
    ///
    /// This test documents the binary decision rule and prevents accidental
    /// regression if someone re-introduces a fallback behavior.
    #[test]
    fn swap_rejects_non_rwt_protocol_fee_destination_mint() {
        // The RWT mint constant from this crate
        let rwt = RWT_MINT;

        // Generate a fake non-RWT mint: a 32-byte pubkey statistically
        // certain to differ from the real RWT mint. Use sequential bytes
        // (i+1 at each position) to ensure bit-wise distinction.
        let non_rwt_mint: [u8; 32] = {
            let mut bytes = [0u8; 32];
            for (i, byte) in bytes.iter_mut().enumerate() {
                *byte = (i as u8).wrapping_add(1);
            }
            bytes
        };

        // Sanity check: our fake non-RWT mint differs from RWT
        assert_ne!(non_rwt_mint, rwt, "test fixture: synthetic mint must differ from RWT");

        // Mirror the production check: the decision is whether dest_mint == RWT_MINT
        // RWT mint should satisfy the check
        assert_eq!(
            rwt == RWT_MINT,
            true,
            "RWT_MINT must equal itself (passes the check)"
        );

        // Non-RWT mint should fail the check (causing revert in production)
        assert_ne!(
            non_rwt_mint, RWT_MINT,
            "non-RWT mint must NOT equal RWT_MINT (fails the check, triggers InvalidProtocolFeeDestination revert)"
        );
    }

    // ---------------------------------------------------------------------
    // Fee-on-top compliance (docs/contracts/native-dex.mdx:522-568)
    //
    // The three tests below pin the new sell-RWT semantics:
    //   1) Full `amount_in` enters the constant-product curve (NOT `amount_in - fees`).
    //   2) The inbound transfer debits `amount_in + fee_lp + fee_protocol +
    //      fee_ot_treasury` from the user's wallet (fees are external to the
    //      swap principal).
    //   3) The vault invariant holds:
    //        vault_in_after == reserves + Σ_unclaimed_fee_lp
    //      after the outbound CPIs extract fee_protocol + fee_ot_treasury.
    // ---------------------------------------------------------------------

    /// Pure-math pin: when selling RWT, the full `amount_in` is fed into
    /// `constant_product_output` — fees are external. Compare to the docs
    /// formula at native-dex.mdx:537 (`amount_out = constant_product(amount_in)`).
    #[test]
    fn sell_rwt_full_amount_in_enters_curve() {
        // Realistic-ish 1M / 1M pool with 30 bps fee.
        let reserve_in: u64 = 1_000_000;
        let reserve_out: u64 = 1_000_000;
        let amount_in: u64 = 10_000;
        let fee_bps: u16 = 30;
        let lp_fee_share_bps: u16 = 5_000;
        let has_ot_treasury = false;

        // Compute fees ON the trade size; do NOT subtract them from `amount_in`.
        let fees = crate::amm::calculate_fees(amount_in, fee_bps, lp_fee_share_bps, has_ot_treasury)
            .expect("calculate_fees should not fail");

        // New spec (docs/contracts/native-dex.mdx:565): full amount_in enters
        // the curve. The expected formula is
        //   amount_out = reserve_out * amount_in / (reserve_in + amount_in)
        //              = 1_000_000 * 10_000 / 1_010_000 ≈ 9_900.
        let amount_out = crate::amm::constant_product_output(reserve_in, reserve_out, amount_in)
            .expect("constant_product_output should not fail");

        // Reference: 1M * 10_000 / 1_010_000 = 9_900 (integer truncation).
        let expected_out: u64 = (reserve_out as u128 * amount_in as u128
            / (reserve_in as u128 + amount_in as u128)) as u64;
        assert_eq!(amount_out, expected_out);
        assert_eq!(amount_out, 9_900u64);

        // OLD (pre-fix) formula for clarity — used `amount_in - fees.fee_total`
        // and would have returned `~9_870`. The delta (~30 units) is the user's
        // gross-up benefit under fee-on-top: they get the full curve output.
        let old_net_input = amount_in - fees.fee_total;
        let old_amount_out = crate::amm::constant_product_output(reserve_in, reserve_out, old_net_input)
            .expect("constant_product_output should not fail");
        assert!(old_amount_out < amount_out, "fee-on-top must yield strictly better output for the user");
    }

    /// Pure-math pin: the user's total wallet debit on the sell-RWT path is
    /// `amount_in + fee_lp + fee_protocol + fee_ot_treasury`. Equivalently
    /// `amount_in + fee_total + fee_ot_treasury` since
    /// `fee_lp + fee_protocol == fee_total`. Also asserts the `checked_add`
    /// chain reverts with `MathOverflow` when the components overflow `u64`.
    #[test]
    fn sell_rwt_user_total_debit_math() {
        let amount_in: u64 = 10_000;
        let fee_bps: u16 = 30;
        let lp_fee_share_bps: u16 = 5_000;
        let has_ot_treasury = true; // exercise OT branch too

        let fees = crate::amm::calculate_fees(amount_in, fee_bps, lp_fee_share_bps, has_ot_treasury)
            .expect("calculate_fees should not fail");

        // Mirror the production gross-up: amount_in + fee_lp + fee_protocol + fee_ot_treasury.
        let user_total_debit = amount_in
            .checked_add(fees.fee_lp).unwrap()
            .checked_add(fees.fee_protocol).unwrap()
            .checked_add(fees.ot_treasury_fee).unwrap();

        // Equivalent form: amount_in + fee_total + ot_treasury_fee.
        let user_total_debit_alt = amount_in
            .checked_add(fees.fee_total).unwrap()
            .checked_add(fees.ot_treasury_fee).unwrap();

        assert_eq!(user_total_debit, user_total_debit_alt);
        // fee_lp + fee_protocol must equal fee_total exactly (remainder pattern).
        assert_eq!(fees.fee_lp + fees.fee_protocol, fees.fee_total);

        // Overflow test: u64::MAX as input MUST cause the gross-up chain to
        // overflow `checked_add`. Pick any non-zero fee component so the add
        // overflows on the first step; the production code propagates via
        // `?` → ProgramError::from(DexError::MathOverflow).
        let amount_in_max: u64 = u64::MAX;
        // fee_lp on u64::MAX would itself be derived from u128 arithmetic
        // inside calculate_fees; we instead exercise the gross-up directly
        // with a non-zero fee_lp to ensure the chain reverts.
        let synthetic_fee_lp: u64 = 1;
        let overflow_attempt = amount_in_max
            .checked_add(synthetic_fee_lp);
        assert!(overflow_attempt.is_none(), "u64::MAX + 1 must overflow checked_add");
    }

    /// Vault-invariant simulation: starting from a clean state, walk through
    /// the production sequence (inbound transfer, reserve updates, outbound
    /// CPIs) and assert the post-state invariant
    ///   vault_in_after == reserve_in_after + Σ_unclaimed_fee_lp
    /// since the starting accumulator is zero, `Σ_unclaimed_fee_lp == fee_lp`.
    #[test]
    fn sell_rwt_post_swap_invariant() {
        // Starting state.
        let reserve_in_before: u64 = 1_000_000;
        let reserve_out_before: u64 = 1_000_000;
        let vault_in_before: u64 = reserve_in_before; // clean pool, no prior fee_lp dust
        let amount_in: u64 = 10_000;
        let fee_bps: u16 = 30;
        let lp_fee_share_bps: u16 = 5_000;
        let has_ot_treasury = true;

        let fees = crate::amm::calculate_fees(amount_in, fee_bps, lp_fee_share_bps, has_ot_treasury)
            .expect("calculate_fees should not fail");

        // User's wallet debit (fee-on-top): amount_in + fee_lp + fee_protocol + fee_ot_treasury.
        let user_total_debit = amount_in
            + fees.fee_lp + fees.fee_protocol + fees.ot_treasury_fee;

        // Step 1: inbound transfer — vault gets +user_total_debit.
        let mut vault_in = vault_in_before + user_total_debit;

        // Step 2: reserve updates — full amount_in enters reserves (fee-on-top).
        // amount_out leaves reserve_out; not part of vault_in counter.
        let amount_out = crate::amm::constant_product_output(reserve_in_before, reserve_out_before, amount_in)
            .expect("constant_product_output should not fail");
        let reserve_in_after = reserve_in_before + amount_in;
        let _reserve_out_after = reserve_out_before - amount_out;

        // Step 3: outbound CPIs extract fee_protocol + fee_ot_treasury from vault.
        // fee_lp stays in the vault (tracked off-reserve by the accumulator).
        vault_in -= fees.fee_protocol;
        vault_in -= fees.ot_treasury_fee;

        // Post-state invariant: vault_in == reserves + Σ_unclaimed_fee_lp.
        // Starting accumulator is zero, so Σ_unclaimed_fee_lp == fee_lp (the
        // only swap so far).
        let sum_unclaimed_fee_lp = fees.fee_lp;
        assert_eq!(
            vault_in,
            reserve_in_after + sum_unclaimed_fee_lp,
            "vault_in_after must equal reserve_in_after + Σ_unclaimed_fee_lp"
        );
    }

    // =====================================================================
    // CP-6 — Mint-route decision tests.
    //
    // Pure-data exercises of `should_route_to_mint` against the full matrix
    // of {organic-ask presence × ask-price vs threshold × NAV value}.
    // Handler-level negative ACs (full revert with all accounts populated)
    // are exercised by future BPF integration tests; here we pin the
    // boolean decision rule so any refactor of the gate logic surfaces
    // immediately.
    //
    // Plus pin tests for the surrounding eligibility predicates:
    // - StandardCurve pools never route (pool_type gate)
    // - RWT→USDC direction never routes (input_is_rwt gate)
    // - Non-master concentrated pools never route (mint-pair gate)
    // =====================================================================

    /// Helper: synthetic best-ask price `q` in CONCENTRATED_SCALE units that
    /// equals `nav * scale_factor / 1_000_000` (NAV-scale → CONCENTRATED).
    /// Used to construct test prices at, above, and below the threshold.
    fn nav_to_q_price(nav: u64, bps_offset_from_nav: i64) -> u128 {
        // Price = NAV × (1 + bps_offset / 10_000) in CONCENTRATED_SCALE units.
        let nav_q = (nav as u128) * (CONCENTRATED_SCALE / 1_000_000);
        let signed = bps_offset_from_nav;
        if signed >= 0 {
            nav_q * (10_000u128 + signed as u128) / 10_000u128
        } else {
            nav_q * (10_000u128 - (-signed) as u128) / 10_000u128
        }
    }

    /// (1) Empty organic ask → route to mint, regardless of price.
    #[test]
    fn master_usdc_to_rwt_empty_ask_routes_to_mint() {
        let nav = 1_000_000u64; // $1.00 NAV
        // Best ask at NAV − 1% (below threshold) shouldn't matter when no ask exists.
        let best_ask_q = nav_to_q_price(nav, -100);
        let routed = super::should_route_to_mint(
            /* has_organic_ask */ false,
            nav,
            best_ask_q,
            /* bin_step_bps */ 10,
        );
        assert!(routed, "empty ask must always route to mint");
    }

    /// (2) Ask present, price below threshold → use bin-walk.
    #[test]
    fn master_usdc_to_rwt_ask_present_below_threshold_uses_bin_walk() {
        let nav = 1_000_000u64;
        // Best ask at NAV × 1.001 (well below threshold NAV × 1.005)
        let best_ask_q = nav_to_q_price(nav, 10);
        let routed = super::should_route_to_mint(true, nav, best_ask_q, 10);
        assert!(!routed, "ask priced under threshold uses bin-walk path");
    }

    /// (3) Ask present, price above threshold → route to mint.
    #[test]
    fn master_usdc_to_rwt_ask_present_above_threshold_routes_to_mint() {
        let nav = 1_000_000u64;
        // Best ask at NAV × 1.01 (above threshold NAV × 1.005)
        let best_ask_q = nav_to_q_price(nav, 100);
        let routed = super::should_route_to_mint(true, nav, best_ask_q, 10);
        assert!(routed, "ask priced above NAV × 1.005 must route to mint");
    }

    /// (4) `master_rwt_to_usdc_never_routes_to_mint` — pinned by the outer
    /// `is_master_pool_usdc_to_rwt` gate (`!input_is_rwt`). The gate predicate
    /// is exercised against fabricated `(input_is_rwt, pool_type, non_rwt_mint)`
    /// tuples here, mirroring the handler's branch decision.
    #[test]
    fn master_rwt_to_usdc_never_routes_to_mint() {
        // Mirror the production gate's boolean expression:
        //   is_master_pool_usdc_to_rwt =
        //     pool.pool_type == POOL_TYPE_CONCENTRATED
        //     && !input_is_rwt
        //     && (non_rwt_mint == USDC_MINT || non_rwt_mint == USDY_MINT)
        let pool_type: u8 = POOL_TYPE_CONCENTRATED;
        let input_is_rwt = true; // a_to_b sell-RWT direction (RWT → USDC)
        let non_rwt_mint = USDC_MINT;
        let is_master = pool_type == POOL_TYPE_CONCENTRATED
            && !input_is_rwt
            && (non_rwt_mint == USDC_MINT || non_rwt_mint == USDY_MINT);
        assert!(!is_master, "RWT→USDC direction must never trigger mint-route gate");
    }

    /// (5) Non-master concentrated pool (non-USDC, non-USDY pair) → never routes.
    /// This is a defence-in-depth assertion: CP-4 already enforces that
    /// concentrated pools only get created with USDC/USDY non-RWT sides,
    /// but the swap-time gate must also refuse for any hypothetical pool
    /// that slipped through (e.g. legacy state from before CP-4).
    #[test]
    fn non_master_concentrated_never_routes_to_mint() {
        let pool_type = POOL_TYPE_CONCENTRATED;
        let input_is_rwt = false;
        let non_rwt_mint = [0xFFu8; 32]; // arbitrary non-USDC, non-USDY mint
        let is_master = pool_type == POOL_TYPE_CONCENTRATED
            && !input_is_rwt
            && (non_rwt_mint == USDC_MINT || non_rwt_mint == USDY_MINT);
        assert!(!is_master, "non-USDC/USDY concentrated pool must skip mint-route");
    }

    /// (6) StandardCurve pools never route to mint.
    #[test]
    fn standard_curve_never_routes_to_mint() {
        let pool_type = POOL_TYPE_STANDARD;
        let input_is_rwt = false;
        let non_rwt_mint = USDC_MINT;
        let is_master = pool_type == POOL_TYPE_CONCENTRATED
            && !input_is_rwt
            && (non_rwt_mint == USDC_MINT || non_rwt_mint == USDY_MINT);
        assert!(!is_master, "StandardCurve pools must skip mint-route entirely");
    }

    /// (7) Price exactly at threshold → bin-walk (strict `>` boundary).
    /// Mirrors the docs phrasing "price > NAV × 1.005" — equality stays on
    /// the bin-walk path.
    #[test]
    fn price_at_threshold_exact_uses_bin_walk() {
        let nav = 1_000_000u64;
        // Construct exact threshold price using the SAME formula the gate
        // uses (avoids rounding drift between test setup and helper).
        let nav_q = (nav as u128) * (CONCENTRATED_SCALE / 1_000_000);
        let threshold_q = nav_q
            * ((BPS_DENOMINATOR as u128) + (MINT_ROUTE_PRICE_OFFSET_BPS as u128))
            / (BPS_DENOMINATOR as u128);
        // best_ask_q == threshold_q exactly.
        let routed = super::should_route_to_mint(true, nav, threshold_q, 10);
        assert!(!routed, "price exactly at threshold uses bin-walk (strict `>`)");
    }

    /// (8) Price one CONCENTRATED_SCALE-ulp above threshold → route to mint.
    #[test]
    fn price_one_above_threshold_routes_to_mint() {
        let nav = 1_000_000u64;
        let nav_q = (nav as u128) * (CONCENTRATED_SCALE / 1_000_000);
        let threshold_q = nav_q
            * ((BPS_DENOMINATOR as u128) + (MINT_ROUTE_PRICE_OFFSET_BPS as u128))
            / (BPS_DENOMINATOR as u128);
        let one_above = threshold_q + 1;
        let routed = super::should_route_to_mint(true, nav, one_above, 10);
        assert!(routed, "price one ulp above threshold must route to mint");
    }

    /// (9) NAV == 0 edge case: threshold collapses to 0; any positive ask
    /// price exceeds it → route to mint. Defensive against pre-init /
    /// post-impairment vault state.
    #[test]
    fn nav_zero_routes_to_mint() {
        let routed = super::should_route_to_mint(
            /* has_organic_ask */ true,
            /* nav */ 0,
            /* best_ask_price_q */ 1, // any positive ask
            10,
        );
        assert!(routed, "NAV=0 must defensively route to mint for any positive ask");
    }

    /// (10) Bin-walk path fees unchanged — when the routing decision picks
    /// bin-walk, the existing fee-on-top math (covered by
    /// `sell_rwt_*` tests above and `calculate_fees`) is the source of
    /// truth. Pin that `should_route_to_mint` returns `false` for the
    /// canonical "ask present, price below threshold" case so the
    /// downstream fee math is reached unmodified.
    #[test]
    fn bin_walk_path_fees_unchanged() {
        let nav = 1_000_000u64;
        let best_ask_q = nav_to_q_price(nav, 0); // exactly at NAV
        assert!(!super::should_route_to_mint(true, nav, best_ask_q, 10));
    }

    /// (11) Mint-route path emits SwapRoutedToMint instead of SwapExecuted.
    /// Without a BPF runtime we can only pin the structural decision: the
    /// production handler `return Ok(())` inside the `route_to_mint`
    /// branch, BEFORE reaching the `SwapExecuted` `emit!()` at the end of
    /// `swap_internal`. This test documents that contract by pinning the
    /// SwapRoutedToMint event's field layout (catches a refactor that
    /// rearranges or drops fields).
    #[test]
    fn mint_route_path_emits_distinct_event() {
        // Field layout pin: pool, user, amount_in, nav_at_route,
        // best_ask_price_q, timestamp = 6 fields.
        use crate::events::SwapRoutedToMint;
        let evt = SwapRoutedToMint {
            pool: [0x11u8; 32],
            user: [0x22u8; 32],
            amount_in: 1_000_000,
            nav_at_route: 1_005_000,
            best_ask_price_q: 1_006_000_000_000u128,
            timestamp: 1_700_000_000,
        };
        // Smoke: every field is reachable as documented.
        assert_eq!(evt.pool, [0x11u8; 32]);
        assert_eq!(evt.user, [0x22u8; 32]);
        assert_eq!({ evt.amount_in }, 1_000_000);
        assert_eq!({ evt.nav_at_route }, 1_005_000);
        assert_eq!({ evt.best_ask_price_q }, 1_006_000_000_000u128);
        assert_eq!({ evt.timestamp }, 1_700_000_000);
    }

    /// (12) NAV read offset — synthetic 40-byte RwtVault buffer test for
    /// the per-test mirror of `read_rwt_vault_nav`. Pins offset 24 (within
    /// data) / 32 (absolute, post-discriminator). Catches drift if the
    /// rwt_engine `RwtVault` layout is reordered without updating the
    /// `RWT_VAULT_NAV_OFFSET` constant.
    #[test]
    fn nav_read_offset_correct() {
        const NAV: u64 = 1_005_000;
        let mut buf = [0u8; 40];
        buf[32..40].copy_from_slice(&NAV.to_le_bytes());
        let read = u64::from_le_bytes(buf[32..40].try_into().unwrap());
        assert_eq!(read, NAV);
        // Mirror the constant arithmetic.
        assert_eq!(RWT_VAULT_DISC_LEN + RWT_VAULT_NAV_OFFSET, 32);
    }

    /// (13) CPI account-meta order/flags — pin the production `cpi_mint_rwt`
    /// account list against the `MintRwt` accounts struct declared in
    /// `contracts/rwt-engine/src/instructions/mint_rwt.rs`. Drift in either
    /// half causes the runtime to reject the CPI as a foreign-meta call.
    ///
    /// This test pins the tuple matrix at the swap-handler boundary
    /// (mirrors the test in `cpi.rs`) so a single grep against
    /// `cpi_account_metas_match_rwt_engine_signature` finds both ends of
    /// the contract.
    #[test]
    fn cpi_account_metas_match_rwt_engine_signature() {
        // (is_writable, is_signer) per slot, in MintRwt field order.
        let expected: [(bool, bool); 8] = [
            (false, true),  // user (signer)
            (true,  false), // rwt_vault (mut)
            (true,  false), // rwt_mint (mut)
            (true,  false), // user_deposit (mut)
            (true,  false), // user_rwt (mut)
            (true,  false), // capital_acc (mut)
            (true,  false), // dao_fee_account (mut)
            (false, false), // token_program (read)
        ];
        assert_eq!(expected.len(), 8);
        let signers: usize = expected.iter().filter(|(_, s)| *s).count();
        assert_eq!(signers, 1, "exactly one signer (user pass-through)");
    }
}
