//! Shared infrastructure for the SVM tests of the Sale and Burn programs.
//!
//! Contents:
//! - [`TestEnv`] - LiteSVM with `sale.so` and `burn.so` loaded, the ProgramData
//!   fixtures (Arch §9.1), the actors and the mints (USDC dec 6, 1WIN dec 8);
//! - clock helpers (`warp_to`, `now`, `expire_blockhash`) - `env.rs`;
//! - token helpers (`create_ata`, `mint_to`, `token_balance`) - `token.rs`;
//! - typed instruction builders, PDA helpers and the `SaleState` reader -
//!   `ix.rs`; the Burn harness (builders, PDAs, readers) lives in its own crate
//!   `tests/burn` (`burn-tests`);
//! - a re-export of the `expected` crate - the single source of reference
//!   constants.

mod env;
mod ix;
mod token;

pub use env::{
    assert_artifact_fresh, assert_fresh_vs_sources, Actors, TestEnv, ACTOR_LAMPORTS,
    SALE_CRATE_DIR, SALE_SO_PATH, SLOTS_PER_SECOND,
};
/// The literal reference values of E1-E3 / §10.6 (tests/expected) - the only
/// source of expectation constants; re-exporting keeps that single source.
pub use expected;
pub use ix::{
    accept_owner_ix, activate_claim_ix, aggregate_pda, assert_funded_ix,
    assert_funded_ix_with_slots, blacklist_add_ix, blacklist_add_ix_with_slot, blacklist_pda,
    blacklist_remove_ix, blacklist_remove_ix_with_slot, cancel_owner_proposal_ix, claim_ix,
    claim_ix_full, close_aggregates_ix, close_deposits_ix, deposit_pda, deposit_vault_pda,
    event_authority_pda, graylist_create_ix, graylist_create_ix_with_slot, graylist_pda,
    graylist_remove_ix, graylist_remove_ix_with_slot, graylist_update_ix,
    graylist_update_ix_with_slot, init_aggregate_ix, init_aggregate_ix_with_slots, initialize_ix,
    owner_token_ata, pause_ix, proceeds_vault_pda, propose_owner_ix, purchase_ix,
    purchase_ix_with_slots, refund_account_pda, refund_ix, refund_ix_with_slots,
    refund_ix_with_vault, refund_vault_pda, sale_state_pda, set_admin_ix, set_claim_schedule_ix,
    set_emission_ix, set_operator_ix, set_price_ix, set_sale_schedule_ix, set_tx_limits_ix,
    sweep_deposit_vault_ix, sweep_refund_vault_ix, sweep_token_vault_ix, token_vault_pda,
    unpause_ix, vault_authority_pda, wallet_token_ata, wallet_usdc_ata, withdraw_proceeds_ix,
    InitParams, CLAIM_DURATION_90D,
};

use litesvm::types::TransactionResult;
use solana_instruction_error::InstructionError;
use solana_transaction_error::TransactionError;

/// Decimals of the USDC mint (TRD: USDC = 6 decimals).
pub const USDC_DECIMALS: u8 = 6;
/// Decimals of the $1WIN mint (TRD: $1WIN = 8 decimals).
pub const WIN_DECIMALS: u8 = 8;

/// Extracts `Custom(code)` from a failed transaction: `Some(code)` if the
/// transaction failed with `InstructionError::Custom` (Anchor errors, including
/// the dispatcher codes and the program's `#[error_code]`), otherwise `None`.
///
/// Tests assert a SPECIFIC code (tests-bind-to-real-code), never `is_err()`.
pub fn custom_error_code(result: &TransactionResult) -> Option<u32> {
    match result {
        Err(failed) => match failed.err {
            TransactionError::InstructionError(_, InstructionError::Custom(code)) => Some(code),
            _ => None,
        },
        Ok(_) => None,
    }
}

/// Decodes the FIRST payload of the Anchor event `E` from the inner instructions
/// of a successful transaction. `#[event_cpi]` emits an event as a self-CPI to the
/// event authority; `InnerInstruction.instruction.data` holds
/// `EVENT_IX_TAG_LE (8) || E::DISCRIMINATOR (8) || Borsh(E)` (under LiteSVM this
/// is NOT the `Program data:` line but the inner instruction's data). The helper
/// scans the inner instructions, checks the event discriminator at offset 8 and
/// Borsh-deserializes the body from offset 16.
///
/// It exists so that event fields that are otherwise hard to observe can be
/// asserted (`AccountsClosed.count/rent_returned`, `Swept.vault`) - an event's
/// payload is part of an instruction's observable effect too
/// (tests-bind-to-real-code).
pub fn decode_event<E: anchor_lang::AnchorDeserialize + anchor_lang::Discriminator>(
    result: &TransactionResult,
) -> Option<E> {
    const TAG_LEN: usize = 8;
    const DISC_LEN: usize = 8;
    let meta = result.as_ref().ok()?;
    for inner in &meta.inner_instructions {
        for ix in inner {
            let data = &ix.instruction.data;
            let body_start = TAG_LEN + DISC_LEN;
            if data.len() < body_start || data[TAG_LEN..body_start] != *E::DISCRIMINATOR {
                continue;
            }
            if let Ok(ev) = E::try_from_slice(&data[body_start..]) {
                return Some(ev);
            }
        }
    }
    None
}
