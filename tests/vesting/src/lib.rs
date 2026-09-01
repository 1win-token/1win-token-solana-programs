//! Shared infrastructure for the SVM tests of the Vesting program.
//!
//! Contents:
//! - [`TestEnv`] - LiteSVM with `vesting.so` loaded, the ProgramData fixture
//!   (Arch §9.1), actors for the three roles of Arch §8 (owner, admin, operator)
//!   and the $1WIN mint (8 decimals) - `env.rs`;
//! - clock helpers (`warp_to`, `now`, `expire_blockhash`) - `env.rs`;
//! - the artifact freshness gate: `assert_artifact_fresh` (for `vesting.so`
//!   itself) and `assert_fresh_vs_sources` (for any derived artifact, the IDL for
//!   instance) - `env.rs`; both are also called from the e2e crate;
//! - token helpers (`create_ata`, `mint_to`, `token_balance`, the forges) -
//!   `token.rs`;
//! - PDA helpers, instruction builders, state readers and forges - `ix.rs`;
//! - scenario helpers (`init_default`, `create_activity`, `fund_vault`,
//!   `at_period`, `warp_to_day`, `claim_as`, `claim_at_day`) - `scenario.rs`;
//! - a re-export of the `vesting-expected` crate - the single source of reference
//!   constants (`MONTH`, `DAY_SECS` and the §10.5 schedules live in
//!   `vesting_expected::vesting` and are NOT duplicated under separate names
//!   here).

// DUP: to be merged into svm-core (follow-up). The crate mirrors the structure of
// sale-test-common: a shared crate would right now disturb the 211 green Sale
// tests.

mod env;
mod ix;
mod scenario;
mod token;

pub use env::{
    assert_artifact_fresh, assert_fresh_vs_sources, Actors, TestEnv, ACTOR_LAMPORTS,
    SLOTS_PER_SECOND, VESTING_SO_PATH,
};
pub use ix::{
    accept_owner_ix, accept_owner_ix_with_config, assert_funded_ix, assert_funded_ix_with_slots,
    cancel_owner_proposal_ix, cancel_owner_proposal_ix_with_config, cancel_vesting_ix,
    cancel_vesting_ix_with_slots, claim_ix, claim_ix_with_slots, close_vesting_ix,
    close_vesting_ix_with_slots, config_pda, create_vesting_ix, create_vesting_ix_with_slots,
    event_authority_pda, funding_tx_ixs, initialize_ix, initialize_ix_with_slots, mint_to_vault_ix,
    non_canonical_pda, propose_owner_ix, propose_owner_ix_with_config, recipient_ata,
    revoke_mint_authority_ix, set_admin_ix, set_admin_ix_with_config, set_operator_ix,
    set_operator_ix_with_config, set_recipient_ix, set_recipient_ix_with_slots, set_tge_ix,
    set_tge_ix_with_config, set_withdrawal_authority_ix, set_withdrawal_authority_ix_with_slots,
    vault_authority_pda, vault_pda, vesting_pda, without_signer, ActivitySpec, InitParams,
};
pub use scenario::{at_period, DEFAULT_TGE_OFFSET};
pub use token::TOKEN_2022_ID;
/// The literal reference values (the `vesting-expected` crate) - the only source of
/// expectation constants; re-exporting keeps that single source.
pub use vesting_expected;

use anchor_lang::error::ERROR_CODE_OFFSET;
use litesvm::types::TransactionResult;
use solana_instruction_error::InstructionError;
use solana_transaction_error::TransactionError;
use vesting::errors::VestingError;

/// Decimals of the $1WIN mint (Arch §10.1: 1 $1WIN = 10⁸ token units). A literal in
/// the test: taking the expectation from the program's own constant
/// (`TOKEN_DECIMALS`) would mean comparing it against itself.
pub const WIN_DECIMALS: u8 = 8;

/// The program's on-chain error code: `#[error_code]` numbers the variants from
/// [`ERROR_CODE_OFFSET`] (6000) in declaration order.
///
/// The only way to name an expected error in the Vesting tests. The `6xxx`
/// literals appear in exactly one place - the `error_codes_are_frozen` pinning test
/// (`tests/smoke.rs`), where the literal IS the expectation: this helper derives the
/// code from the enum and would not notice a shift of the codes.
///
/// The helper lives in the crate rather than being copied into every test file (as
/// it is for Sale): the variant order is frozen, and a single copy of the
/// "variant → code" mapping keeps the wording from drifting apart.
pub fn err(variant: VestingError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

/// Extracts `Custom(code)` from a failed transaction: `Some(code)` if the
/// transaction failed with `InstructionError::Custom` (Anchor errors, including the
/// dispatcher codes and the program's `#[error_code]`), otherwise `None`.
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

/// Decodes the FIRST payload of the Anchor event `E` from the inner instructions of
/// a successful transaction. `#[event_cpi]` emits an event as a self-CPI to the
/// event authority; `InnerInstruction.instruction.data` holds
/// `EVENT_IX_TAG_LE (8) || E::DISCRIMINATOR (8) || Borsh(E)` (under LiteSVM this is
/// NOT the `Program data:` line but the inner instruction's data). The helper scans
/// the inner instructions, checks the event discriminator at offset 8 and
/// Borsh-deserializes the body from offset 16.
///
/// It exists so that event fields that are otherwise hard to observe can be asserted
/// (`label` in `VestingCreated` is not stored on-chain at all, Arch §5.2) - an
/// event's payload is part of an instruction's observable effect too
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

/// The CPI calls of a successful transaction: `(program_id, data)` for each inner
/// instruction, in execution order.
///
/// Needed where the observable effect is the ABSENCE of a step rather than a state
/// change: skipping the transfer for an empty vault (`if amount > 0` in
/// `drain_and_close`) is indistinguishable from a zero transfer by balances alone -
/// SPL Token accepts a `Transfer` of zero units. The CPI list shows the fact itself.
///
/// `ixs` and `payer` are the same ones that went into [`TestEnv::send_ixs`]: an
/// inner instruction's `program_id_index` indexes the message key array, which is
/// derived deterministically from the instructions and the fee payer and does not
/// depend on the blockhash.
///
/// Panics on a failed transaction and on an index outside the key array: both are
/// scenario bugs, and returning an empty list silently is not allowed (fail-closed).
pub fn inner_instructions(
    result: &TransactionResult,
    ixs: &[solana_instruction::Instruction],
    payer: &solana_pubkey::Pubkey,
) -> Vec<(solana_pubkey::Pubkey, Vec<u8>)> {
    let meta = result
        .as_ref()
        .expect("inner instructions are read from a SUCCESSFUL transaction");
    let keys = solana_message::Message::new(ixs, Some(payer)).account_keys;
    meta.inner_instructions
        .iter()
        .flatten()
        .map(|inner| {
            let idx = usize::from(inner.instruction.program_id_index);
            let program_id = *keys.get(idx).unwrap_or_else(|| {
                panic!(
                    "program_id_index {idx} is outside the message keys - `ixs`/`payer` are wrong"
                )
            });
            (program_id, inner.instruction.data.clone())
        })
        .collect()
}
