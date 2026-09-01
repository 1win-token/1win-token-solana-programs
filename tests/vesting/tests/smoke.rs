//! Infrastructure smoke tests for Vesting - four cases, each aimed at a
//! specific fixture risk rather than at "everything works in general". Plus a pinning
//! test for the on-chain error codes: it concerns the same contract with the outside
//! world as the rest of the infrastructure and exercises exactly the `err()` helper
//! all the V1-V3 tests will name those codes with.
//!
//! Why these four in particular:
//! 1. **A jump of 990 days.** The program's furthest scenario is day 991 of the
//!    Treasury bucket: 85,536,000 s from TGE, which at `SLOTS_PER_SECOND = 2` is
//!    171,072,000 slots in a single `set_sysvar(Clock)` - 11 times more than anything
//!    the Sale suite ever did. The jump must leave the environment usable - the proof
//!    being a transaction executed AFTER it with an observable effect (a system
//!    transfer; the program has no instructions yet in V0).
//! 2. **A non-existent discriminator → `InstructionFallbackNotFound`.** The only way
//!    available in V0 to prove that `vesting.so` really EXECUTES: code 101 is born
//!    inside the program's own dispatch rather than in the runner. Which means the
//!    hand-edited first 45 bytes of ProgramData did not break the program cache. The
//!    same call is repeated AFTER the 990-day jump - the jump moves the program
//!    cache's slot too.
//! 3. **The ProgramData fixture resolves.** `litesvm::add_program` writes
//!    `upgrade_authority_address: None`; the whole authorization of `initialize`
//!    rests on that field (Arch §9.1). If the fixture does not read back or has lost
//!    its ELF tail, every V1 negative would be falsely green.
//! 4. **A repeated identical transaction → `AlreadyProcessed`.** The trap:
//!    without `expire_blockhash` a repeated tx is filtered out by LiteSVM's history
//!    BEFORE execution, and a replay test would be checking the runner's
//!    deduplication rather than the program's guard. The test pins that semantics
//!    explicitly.
//!
//! Plus a `should_panic` test on rolling time back: a backwards `warp_to` is
//! forbidden by construction, and the prohibition itself is verified.
//!
//! ===== DELIBERATELY DEFERRED =====
//! Below is what V0 covers by compilation alone. The gap is deliberate: the program
//! has no instructions at all, so there is no observable on-chain effect to anchor an
//! assertion to (and tests-bind-to-real-code forbids substituting a check of the
//! helper itself).
//! - The mints (`win_mint` = 8 decimals, `foreign_mint` - a different address with
//!   the same decimals), the actors and their funding, `create_ata` / `mint_to` /
//!   `token_balance` / `token_account_mint_owner`, and the forges
//!   (`set_token_amount`, `forge_program_account`) - they are asserted in the V1
//!   tests (`initialize` pins `token_mint`) and V2 (`create_vesting` creates the
//!   vault).
//! - The PDA helpers (`config_pda`, `vesting_pda`, `vault_pda`,
//!   `vault_authority_pda`, `event_authority_pda`, `recipient_ata`) and the readers
//!   (`vesting_config`, `vesting`, `*_exists`) - there is nothing to check their
//!   addresses against yet: the seeds are declared in the contexts of the V1-V3
//!   instructions. A divergence from the spec would surface on the very first call
//!   (ConstraintSeeds / AccountNotInitialized).
//! - `decode_event::<E>()` - the first event appears in V1; until then the `emit_cpi`
//!   parsing never runs. `custom_error_code` is exercised here on dispatcher code
//!   101, and `err()` in the error-code pinning test (checking "variant → code"
//!   needs no executed instruction, and waiting for an on-chain error until V1 is not
//!   an option: this is exactly the window in which a variant gets inserted into the
//!   middle of the enum).

use anchor_lang::error::ErrorCode;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;
use vesting::errors::VestingError;
use vesting_test_common::vesting_expected::vesting::{DAY_SECS, TREASURY_FULL_RELEASE_DAY};
use vesting_test_common::{at_period, custom_error_code, err, TestEnv, SLOTS_PER_SECOND};

/// Treasury's full-release period: `s + M − 1 = 9 + 25 − 1 = 33` (Arch §10.5). Day 991
/// from `vesting_expected::vesting` and period 33 are two records of one and the same
/// count.
const TREASURY_FULL_RELEASE_PERIOD: i64 = 33;

/// A call to the program with a non-existent 8-byte discriminator. A program without
/// a fallback function must answer `InstructionFallbackNotFound` - and receiving that
/// code is itself proof that `vesting.so` EXECUTED under the ProgramData fixture (the
/// code is born in the dispatch inside the program, not in the runner).
/// We return the native litesvm TransactionResult as is (as `send_ixs` does).
#[allow(clippy::result_large_err)]
fn call_unknown(env: &mut TestEnv, caller: &Keypair) -> litesvm::types::TransactionResult {
    let ix = Instruction::new_with_bytes(vesting::id(), &[0xAB; 8], vec![]);
    env.send_ixs(&[ix], &caller.pubkey(), &[caller])
}

#[test]
fn warp_990_days_keeps_clock_consistent_and_transactions_executing() {
    let mut env = TestEnv::new();
    let tge_ts = env.now();
    let before_slot = env.clock().slot;

    let target = env.warp_to_day(tge_ts, TREASURY_FULL_RELEASE_DAY);

    // Day 991 = TGE + 990 days; it is also period 33 per Arch §10.1 - two records of one
    // and the same count that must agree, or the time helpers have drifted apart.
    assert_eq!(
        target,
        tge_ts + 990 * DAY_SECS,
        "day 991 is tge_ts + 990 days"
    );
    assert_eq!(
        target,
        at_period(tge_ts, TREASURY_FULL_RELEASE_PERIOD),
        "day 991 coincides with the start of period 33 (s + M − 1 for Treasury)"
    );
    assert_eq!(
        env.now(),
        target,
        "Clock.unix_timestamp was set exactly to the target day"
    );

    // A hand-written reference value: 990 days × 86,400 s × 2 slots = 171,072,000 slots
    // in a single set_sysvar - the jump's magnitude from the S-A spike.
    const EXPECTED_SLOT_DELTA: u64 = 171_072_000;
    assert_eq!(
        env.clock().slot - before_slot,
        EXPECTED_SLOT_DELTA,
        "the slot grew consistently with the timestamp: {EXPECTED_SLOT_DELTA} slots were expected"
    );
    assert_eq!(
        EXPECTED_SLOT_DELTA,
        990 * 86_400 * SLOTS_PER_SECOND,
        "the SLOTS_PER_SECOND ratio is pinned; changing it changes the jump's reference value"
    );

    // The jump did not render the environment unusable: a transaction AFTER it executes
    // and produces an observable effect.
    let from = env.actors.owner.insecure_clone();
    let to = env.actors.stranger.pubkey();
    let before_balance = env.lamports_of(&to);
    let amount = 1_234_567;
    env.send_ixs(
        &[solana_system_interface::instruction::transfer(
            &from.pubkey(),
            &to,
            amount,
        )],
        &from.pubkey(),
        &[&from],
    )
    .expect("a transaction after the 990-day jump executes");
    assert_eq!(
        env.lamports_of(&to),
        before_balance + amount,
        "the transfer after the jump was credited to the recipient"
    );
}

#[test]
fn unknown_discriminator_fails_with_instruction_fallback_not_found() {
    let mut env = TestEnv::new();
    let caller = env.actors.stranger.insecure_clone();

    let expected = u32::from(ErrorCode::InstructionFallbackNotFound);
    assert_eq!(
        expected, 101,
        "the nominal value of the Anchor 1.0.2 dispatcher code"
    );

    let result = call_unknown(&mut env, &caller);
    assert_eq!(
        custom_error_code(&result),
        Some(expected),
        "an unknown discriminator → Custom(101), the full result: {result:?}"
    );

    // The same call after the 990-day jump: set_sysvar(Clock) moves LiteSVM's program
    // cache slot too - the program must stay executable.
    let tge_ts = env.now();
    env.warp_to_day(tge_ts, TREASURY_FULL_RELEASE_DAY);
    env.expire_blockhash();
    let after_warp = call_unknown(&mut env, &caller);
    assert_eq!(
        custom_error_code(&after_warp),
        Some(expected),
        "after the 990-day jump the program still executes: {after_warp:?}"
    );
}

#[test]
fn programdata_fixture_resolves_with_expected_upgrade_authority() {
    let env = TestEnv::new();

    let programdata = env
        .svm
        .get_account(&TestEnv::programdata_address())
        .expect("ProgramData exists at the canonical PDA");

    let metadata_len = UpgradeableLoaderState::size_of_programdata_metadata();
    assert!(
        programdata.data.len() > metadata_len,
        "the ELF tail is preserved behind the 45 metadata bytes: {} bytes long",
        programdata.data.len()
    );

    // bincode reads the metadata from the start of the buffer and the ELF tail does not
    // interfere - the same way Anchor and agave deserialize ProgramData.
    let state: UpgradeableLoaderState =
        bincode::deserialize(&programdata.data).expect("the ProgramData metadata parses");
    match state {
        UpgradeableLoaderState::ProgramData {
            upgrade_authority_address,
            ..
        } => assert_eq!(
            upgrade_authority_address,
            Some(env.actors.owner.pubkey()),
            "the default upgrade authority is the owner actor (the authorization of initialize rests on it)"
        ),
        other => panic!("UpgradeableLoaderState::ProgramData was expected, got {other:?}"),
    }

    // An arbitrary authority - the same path: the V1 negatives substitute it with a key
    // that does not hold the upgrade right.
    let custom = Pubkey::new_unique();
    let custom_env = TestEnv::with_upgrade_authority(custom);
    let forged = custom_env
        .svm
        .get_account(&TestEnv::programdata_address())
        .expect("ProgramData exists");
    let state: UpgradeableLoaderState =
        bincode::deserialize(&forged.data).expect("the ProgramData metadata parses");
    match state {
        UpgradeableLoaderState::ProgramData {
            upgrade_authority_address,
            ..
        } => assert_eq!(
            upgrade_authority_address,
            Some(custom),
            "with_upgrade_authority sets the key that was passed"
        ),
        other => panic!("UpgradeableLoaderState::ProgramData was expected, got {other:?}"),
    }
}

#[test]
fn identical_tx_replay_rejected_until_blockhash_expires() {
    let mut env = TestEnv::new();
    let from = env.actors.owner.insecure_clone();
    let to = env.actors.recipients[0].pubkey();
    let transfer =
        || solana_system_interface::instruction::transfer(&from.pubkey(), &to, 1_000_000);

    env.send_ixs(&[transfer()], &from.pubkey(), &[&from])
        .expect("the first send passes");

    // The same blockhash plus the same signers ⇒ a byte-for-byte identical tx: LiteSVM's
    // history rejects it as AlreadyProcessed BEFORE execution. That is precisely why
    // replay tests of the program's guards (a repeated claim, a repeated activity close)
    // MUST refresh the blockhash - otherwise they verify the runner's deduplication
    // rather than the guard.
    let replayed = env
        .send_ixs(&[transfer()], &from.pubkey(), &[&from])
        .expect_err("an identical tx without expire_blockhash is rejected");
    assert_eq!(
        replayed.err,
        TransactionError::AlreadyProcessed,
        "the repeat without a fresh blockhash is filtered out by the history rather than by the logic: {replayed:?}"
    );

    // A fresh blockhash ⇒ a new signature ⇒ the transaction reaches execution.
    env.expire_blockhash();
    env.send_ixs(&[transfer()], &from.pubkey(), &[&from])
        .expect("after expire_blockhash the repeat executes");
}

/// A pinning test for the on-chain error codes: the order of the `VestingError`
/// variants is frozen, and the 6000+ codes are part of the contract with off-chain
/// consumers (the indexer, the panel, the tests). Inserting a variant into the middle
/// of the enum shifts the codes of everything after it, and `err(variant)` computes
/// the code from the enum itself and would not notice that shift: the expectation and
/// the result would move together.
///
/// An EXCEPTION to the "no 6xxx literals in tests" rule: here the literal IS
/// the expectation, tied to an external contract rather than to the code. It is the
/// only place where it is admissible; in every other test an error is named by
/// `err(VestingError::…)`.
///
/// A variant may only be APPENDED to the end of the list - existing codes then stay
/// put. Any other edit of the enum turns this test red and demands a deliberate
/// decision (an indexer migration, a redeploy).
#[test]
fn error_codes_are_frozen() {
    let frozen: [(VestingError, u32); 21] = [
        (VestingError::MathOverflow, 6000),
        (VestingError::NotOwner, 6001),
        (VestingError::NotUpgradeAuthority, 6002),
        (VestingError::NotAuthorized, 6003),
        (VestingError::InvalidAuthority, 6004),
        (VestingError::InvalidMint, 6005),
        (VestingError::InvalidMintDecimals, 6006),
        (VestingError::InvalidTge, 6007),
        (VestingError::InvalidParams, 6008),
        (VestingError::InvalidReferenceAmount, 6009),
        (VestingError::InvalidBps, 6010),
        (VestingError::InvalidSchedule, 6011),
        (VestingError::InvalidBeneficiary, 6012),
        (VestingError::ScheduleStarted, 6013),
        (VestingError::ScheduleNotStarted, 6014),
        (VestingError::NothingToClaim, 6015),
        (VestingError::InsufficientVaultBalance, 6016),
        (VestingError::NotFullyClaimed, 6017),
        (VestingError::InvalidRentCollector, 6018),
        (VestingError::NoPendingOwner, 6019),
        (VestingError::NotAdmin, 6020),
    ];

    for (variant, code) in frozen {
        assert_eq!(
            err(variant),
            code,
            "the code of variant {variant:?} diverged from the frozen {code}: the order of the VestingError variants changed and the on-chain codes have shifted"
        );
    }
}

#[test]
#[should_panic(expected = "warp_to only moves forward")]
fn warp_to_rejects_time_rollback() {
    let mut env = TestEnv::new();
    let now = env.now();
    env.warp_to(now + 100);
    env.warp_to(now + 50); // backwards relative to the current time - a panic
}
