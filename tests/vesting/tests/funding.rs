//! Vault coverage: the `assert_funded` check (Arch §9.9, card I10); the
//! claim's behaviour under a surplus and an under-mint.
//! `assert_funded` changes no state, emits no events and does not check the signer;
//! its only observable effect is whether the transaction passed or failed.
//!
//! Why the tests are laid out the way they are.
//!
//! 1. The instruction is meant to be a link in the atomic minting transaction
//!    (Arch §2): `mint_to` → `assert_funded` for every activity →
//!    `SetAuthority(None)`. So exactly two things are checked - the "passes / does not
//!    pass" boundary to the precision of a single token unit, and the complete absence
//!    of side effects.
//! 2. The boundary is taken as a pair: exactly `reference_amount` is accepted and one
//!    unit less is rejected. Under-funding is produced HONESTLY (by minting a smaller
//!    amount) wherever possible: that is exactly how an under-mint arises in
//!    production.
//! 3. The property under test is the coverage of the UNRELEASED remainder
//!    (`vault.amount + claimed >= reference_amount`) rather than of the whole
//!    reference amount. The second half of that invariant is observable only when
//!    `claimed > 0`, so cases 5-6 produce it with a forge: an arbitrary intermediate
//!    state is thus obtained without running the whole chain of claims. Without a
//!    non-zero `claimed` the absolute form `vault.amount >= reference_amount` would
//!    pass every test in the file and a swap of the formula would go unnoticed; the
//!    check after REAL claims lives elsewhere.
//! 4. "Permissionless" is verified positively and across several keys: there is no
//!    `Signer` slot in the context at all, so the only signer of the transaction is
//!    its fee payer, and that same fee payer proves no role is asked for. The
//!    assertion "the key is neither Owner nor Operator" stands explicitly, or the test
//!    could quietly degenerate.
//! 5. State immutability is checked against the FULL set of the record's fields and
//!    the vault's balance - both on success and on rejection.
//! 6. The binding to the requested activity is checked by a separate pair of cases on
//!    TWO activities with different reference amounts: on a single activity both slots
//!    (`vesting` and `vault`, derived from the `activity_id` argument) are degenerate -
//!    a program with a hard-coded zero id would pass every other case in the file.
//!    Exactly one activity is funded, and with an amount sufficient for the SMALLER
//!    reference: substituting the record and substituting the vault then produce
//!    observably different results.
//!
//! 7. A separate pair of cases stands at the `u64` edge: a reference of `u64::MAX`
//!    with coverage greater than `u64::MAX`. At production-scale amounts (10¹⁶) the
//!    sum `vault.amount + claimed` does not leave `u64`, so the widening to `u128` is
//!    untouched by the other cases, and a return to `u64` arithmetic would reject a
//!    funded activity.
//! 8. The last two cases look at the same coverage from the other side - the payout's:
//!    a surplus in the vault does not increase the unlock, and an under-mint is pinned
//!    as EXPECTED behaviour of an accepted risk (Arch §12.2) rather than as a defect.
//!    The same place checks `assert_funded` after REAL claims, with no forge of
//!    `claimed`.
//! 9. The funding-transaction cases exercise the instruction in its regular usage - as a link in ONE
//!    atomic funding transaction (Arch §2): `mint_to` into TokenSale's `token_vault`
//!    and into each of the five vesting vaults → `assert_funded` for every activity →
//!    `SetAuthority(None)`. The TokenSale link is supplied as a foreign token account
//!    under the same mint: that program is not deployed into this environment, and
//!    what matters is exactly its contribution to the transaction's size and its part
//!    in the rollback. Both the whole bundle is checked (after it no further issuance
//!    is possible while the claims still work) and its atomicity (an under-mint of ONE
//!    vault fails the transaction and the mint authority stays in place). The
//!    transaction's size is checked against the 1232-byte packet limit here as well:
//!    the surfpool run is `#[ignore]`d and does not go into CI.
//!
//! ===== DELIBERATELY DEFERRED =====
//! - Slot substitutions: a foreign `token_mint`, a decoy `VestingConfig`,
//!   another activity's vault, substituting `vault_authority` - the substitution
//!   matrix. The `AssertFunded` context's attributes are the same as
//!   `CloseActivity`'s and `Claim`'s and are covered there.
//! - The 10×5 role matrix: this instruction has no role by construction.

use vesting::errors::VestingError;
use vesting_test_common::vesting_expected::vesting::{
    EDGE_MAX_REFERENCE, LIQUIDITY_REFERENCE, LIQUIDITY_VESTED_DAY_31, V1_REFERENCE,
    V1_VESTED_FROM_TGE, V2_REFERENCE, V2_VESTED_DAY_151, V3_REFERENCE, V4_REFERENCE,
    V5_CLAIMABLE_DAY_541, V5_CLAIMED_AFTER_DAY_151,
};
use vesting_test_common::{
    assert_funded_ix, assert_funded_ix_with_slots, close_vesting_ix, config_pda, custom_error_code,
    err, funding_tx_ixs, mint_to_vault_ix, recipient_ata, vault_authority_pda, vault_pda,
    vesting_pda, ActivitySpec, TestEnv, DEFAULT_TGE_OFFSET,
};

use litesvm::types::TransactionResult;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;

/// The activity's reference amount: the V2 bucket of §10.5, 10⁸ $1WIN (100,000,000) in
/// token units.
const REFERENCE: u64 = V2_REFERENCE;

/// The part already released in the "after a partial claim" cases: the V2 bucket's
/// accumulated `claimed` on day 151 (§10.5, example V5).
const CLAIMED: u64 = V5_CLAIMED_AFTER_DAY_151;

/// The obligation's unreleased remainder at that `claimed`: `reference − claimed`. A
/// literal from the reference crate (day 541's claimable in example V5) rather than a
/// difference computed in the test.
const REMAINING: u64 = V5_CLAIMABLE_DAY_541;

// The three constants are taken from `vesting-expected` independently of one another,
// and the point of cases 5-6 rests solely on their consistency. A compile-time check:
// should the references drift apart, the file will not build instead of quietly
// checking something else.
const _: () = assert!(
    CLAIMED + REMAINING == REFERENCE,
    "the references are inconsistent: claimed + remaining must equal reference_amount"
);

/// The reference amount of the SECOND activity in the binding tests: the V3 bucket of
/// §10.5. Strictly smaller than [`REFERENCE`] - the assertion "the reference is taken
/// from the requested activity" rests on that: an amount sufficient for the smaller
/// activity cannot fund the larger one.
const SMALLER_REFERENCE: u64 = V3_REFERENCE;

const _: () = assert!(
    SMALLER_REFERENCE < REFERENCE,
    "the binding references must differ, and SMALLER_REFERENCE must be the smaller one"
);

/// An environment with a config and one created activity for [`REFERENCE`]; the vault
/// is empty. Returns `(env, activity_id)`.
fn setup() -> (TestEnv, u64) {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let activity_id = env.create_activity(&ActivitySpec::valid(REFERENCE));
    (env, activity_id)
}

/// An environment with TWO activities and empty vaults: the zeroth for
/// [`SMALLER_REFERENCE`] and the first for [`REFERENCE`]. Their recipients differ so
/// that the activities coincide in no observable field.
/// Returns `(env, small_id, large_id)`.
fn setup_two() -> (TestEnv, u64, u64) {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let first = env.actors.recipients[0].pubkey();
    let second = env.actors.recipients[1].pubkey();
    let small_id = env.create_activity_for(&first, &ActivitySpec::valid(SMALLER_REFERENCE));
    let large_id = env.create_activity_for(&second, &ActivitySpec::valid(REFERENCE));
    assert_ne!(small_id, large_id, "the activities got different ids");
    (env, small_id, large_id)
}

/// Sends `assert_funded` on behalf of `signer` (which is also the fee payer: the
/// instruction has no `Signer` slot).
#[allow(clippy::result_large_err)]
fn send_assert_funded(
    env: &mut TestEnv,
    activity_id: u64,
    signer: &Keypair,
) -> litesvm::types::TransactionResult {
    let ix = assert_funded_ix(activity_id, env.win_mint);
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// The activity's observable state: every field of the record plus the vault's balance
/// and the activity counter. Comparing the snapshots before and after is the
/// "the instruction changes no state" assertion in a single line.
#[derive(Debug, PartialEq, Eq)]
struct ActivityState {
    /// Every field of the `Vesting` record as a tuple: the type does not derive `PartialEq`.
    vesting_fields: (Pubkey, u64, u64, u16, u16, u16),
    vault_balance: u64,
    activity_count: u64,
}

fn state_of(env: &TestEnv, activity_id: u64) -> ActivityState {
    let v = env.vesting(activity_id);
    ActivityState {
        vesting_fields: (
            v.recipient,
            v.reference_amount,
            v.claimed,
            v.tge_bps,
            v.monthly_bps,
            v.cliff_months,
        ),
        vault_balance: env.vault_balance(activity_id),
        activity_count: env.vesting_config().activity_count,
    }
}

// ===================================================================
// [I10] - the coverage boundary, permissionless access, the absence of effects
// ===================================================================

#[test]
fn assert_funded_passes_at_exactly_reference_amount() {
    let (mut env, id) = setup();
    // The honest funding path (Arch §2): minting the emission straight into the vault.
    env.fund_vault(id, REFERENCE);
    assert_eq!(
        env.vault_balance(id),
        REFERENCE,
        "the boundary's precondition"
    );

    let payer = env.payer.insecure_clone();
    let r = send_assert_funded(&mut env, id, &payer);
    r.as_ref()
        .expect("vault.amount == reference_amount - funded exactly at the boundary");
}

#[test]
fn assert_funded_rejects_one_unit_short() {
    let (mut env, id) = setup();
    // The under-mint is produced honestly: minting one unit below the reference amount -
    // exactly the scenario the instruction was introduced for (Arch §12.2).
    env.fund_vault(id, REFERENCE - 1);

    let payer = env.payer.insecure_clone();
    let r = send_assert_funded(&mut env, id, &payer);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "vault.amount == reference_amount − 1 → InsufficientVaultBalance: {r:?}"
    );

    // A POSITIVE in the same environment: the missing unit lifts the rejection.
    let win_mint = env.win_mint;
    env.mint_to(&win_mint, &vault_pda(id), 1);
    env.expire_blockhash();
    let r = send_assert_funded(&mut env, id, &payer);
    r.as_ref()
        .expect("topping up by one token unit to the reference amount makes the activity funded");
}

#[test]
fn assert_funded_is_permissionless() {
    let (mut env, id) = setup();
    env.fund_vault(id, REFERENCE);

    let owner = env.actors.owner.pubkey();
    let operator = env.actors.operator.pubkey();
    let callers = [
        ("stranger", env.actors.stranger.insecure_clone()),
        ("beneficiary", env.actors.beneficiary.insecure_clone()),
        ("rent_collector", env.actors.rent_collector.insecure_clone()),
        ("recipient", env.actors.recipients[0].insecure_clone()),
    ];

    for (label, caller) in callers {
        assert_ne!(caller.pubkey(), owner, "{label} must not be Owner");
        assert_ne!(caller.pubkey(), operator, "{label} must not be Operator");
        env.expire_blockhash();
        let r = send_assert_funded(&mut env, id, &caller);
        r.as_ref().unwrap_or_else(|e| {
            panic!("{label} does not pass the permissionless coverage check: {e:?}")
        });
    }
}

#[test]
fn assert_funded_changes_nothing() {
    let (mut env, id) = setup();
    env.fund_vault(id, REFERENCE);
    let payer = env.payer.insecure_clone();

    // A successful call.
    let before = state_of(&env, id);
    send_assert_funded(&mut env, id, &payer).expect("a funded activity passes the check");
    assert_eq!(
        state_of(&env, id),
        before,
        "a successful check changed not a single field of the record and did not touch the vault"
    );

    // A rejection. The balance cannot be understated honestly (it is only ever moved down
    // by `claim` and by a cancellation or close, and both stay consistent with the
    // accounting), hence the forge.
    env.set_vault_amount(id, REFERENCE - 1);
    let before_failing = state_of(&env, id);
    env.expire_blockhash();
    let r = send_assert_funded(&mut env, id, &payer);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "precondition: the call must fail: {r:?}"
    );
    assert_eq!(
        state_of(&env, id),
        before_failing,
        "the rejection changed nothing either"
    );
}

#[test]
fn assert_funded_covers_remaining_obligation_after_partial_claim() {
    let (mut env, id) = setup();
    // `claimed` is forged: its only writer is `claim`, which does not exist in the
    // program yet, and without a non-zero `claimed` the formula under test is
    // indistinguishable from the absolute `vault.amount >= reference_amount`.
    env.forge_claimed(id, CLAIMED);
    // The balance after such a claim: the reference amount minus what was released.
    env.fund_vault(id, REMAINING);

    assert_eq!(
        env.vesting(id).claimed,
        CLAIMED,
        "precondition: part of the obligation has already been released"
    );
    assert!(
        env.vault_balance(id) < REFERENCE,
        "precondition: the balance is below the reference amount - the absolute form of the check would reject the activity"
    );

    let payer = env.payer.insecure_clone();
    let r = send_assert_funded(&mut env, id, &payer);
    r.as_ref().expect(
        "vault.amount + claimed == reference_amount: the unreleased remainder is fully covered",
    );
}

#[test]
fn assert_funded_rejects_remaining_obligation_short_by_one() {
    let (mut env, id) = setup();
    env.forge_claimed(id, CLAIMED);
    env.fund_vault(id, REMAINING - 1);

    let payer = env.payer.insecure_clone();
    let r = send_assert_funded(&mut env, id, &payer);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "vault.amount + claimed == reference_amount − 1 → InsufficientVaultBalance: {r:?}"
    );

    // A POSITIVE in the same environment: topping up by one unit closes the
    // remainder.
    let win_mint = env.win_mint;
    env.mint_to(&win_mint, &vault_pda(id), 1);
    env.expire_blockhash();
    let r = send_assert_funded(&mut env, id, &payer);
    r.as_ref()
        .expect("topping up to full coverage of the unreleased remainder lifts the rejection");
}

// ===================================================================
// [I10] - binding the check to the REQUESTED activity
//
// The cases above work with a single activity, so both of the context's bindings
// (`vesting` and `vault` derived from the `activity_id` argument) are degenerate in
// them: a program reading a fixed zeroth activity would pass them all. Below there
// are two activities with DIFFERENT reference amounts and exactly one of them is
// funded - then each binding becomes observable on its own.
// ===================================================================

#[test]
fn assert_funded_binds_vault_to_requested_activity() {
    let (mut env, small_id, large_id) = setup_two();
    // Only the zeroth activity is funded - to exactly its own reference amount.
    env.fund_vault(small_id, SMALLER_REFERENCE);
    let payer = env.payer.insecure_clone();

    let r = send_assert_funded(&mut env, small_id, &payer);
    r.as_ref().expect("the funded activity is covered");

    // The vault is taken from the REQUESTED activity: were the program to substitute a
    // fixed zeroth id, the call would see the neighbour's full vault.
    env.expire_blockhash();
    let r = send_assert_funded(&mut env, large_id, &payer);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "an unfunded activity is not covered, however much sits in its neighbour's vault: {r:?}"
    );

    // A POSITIVE in the same environment: its own funding lifts the rejection.
    env.fund_vault(large_id, REFERENCE);
    env.expire_blockhash();
    let r = send_assert_funded(&mut env, large_id, &payer);
    r.as_ref()
        .expect("the activity is funded by its own vault rather than the neighbour's");
}

#[test]
fn assert_funded_binds_reference_to_requested_activity() {
    let (mut env, small_id, large_id) = setup_two();
    // The mirror of the previous case: the SECOND activity is funded, and to exactly the
    // FIRST one's reference amount - an amount sufficient for the smaller activity but
    // not for the one whose vault holds it.
    env.fund_vault(large_id, SMALLER_REFERENCE);
    let payer = env.payer.insecure_clone();

    let r = send_assert_funded(&mut env, large_id, &payer);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "the reference amount is taken from the requested activity: were the program to read the zeroth one's record, this amount would suffice and there would be no rejection: {r:?}"
    );

    let r = send_assert_funded(&mut env, small_id, &payer);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "the zeroth activity's vault is empty: were the program to read the neighbour's vault, the amount sitting there would suffice for exactly this reference: {r:?}"
    );

    // A POSITIVE in the same environment: every activity is funded through its own
    // "record + vault" pair.
    env.fund_vault(large_id, REFERENCE - SMALLER_REFERENCE);
    env.fund_vault(small_id, SMALLER_REFERENCE);
    env.expire_blockhash();
    send_assert_funded(&mut env, large_id, &payer).expect("the second activity is covered");
    env.expire_blockhash();
    send_assert_funded(&mut env, small_id, &payer).expect("the first activity is covered");
}

// ===================================================================
// B [a seeds+bump mutation on AssertFunded] - mismatched vault/vesting slots
//
// The `..._binds_..._to_requested_activity` cases above substitute the `activity_id`
// argument, from which the program derives BOTH slots, so the two stay mutually
// consistent. Here `_with_slots` supplies ANOTHER activity's vault or record while
// the `activity_id` argument names this one - the `seeds + bump` binding must reject
// the mismatch with `ConstraintSeeds` (2006). Removing `seeds+bump` from the `vault`
// slot (case 1) or the `vesting` slot (case 2) are two uncaught mutations: without
// them `assert_funded` would compute the coverage against the SUBSTITUTED vault or
// reference rather than against the requested activity, and both substitutions would
// pass (the amounts are chosen so that under the mutation there would be no
// rejection).
// ===================================================================

#[test]
fn assert_funded_rejects_vault_slot_of_another_activity() {
    let (mut env, small_id, large_id) = setup_two();
    // The SMALLER activity is funded - and with a surplus that would suffice for the
    // larger reference: under the mutation (no seeds on vault) reading its balance as the
    // larger activity's vault would pass the check.
    env.fund_vault(small_id, REFERENCE);
    let win_mint = env.win_mint;
    let payer = env.payer.insecure_clone();

    let before_small = state_of(&env, small_id);
    let before_large = state_of(&env, large_id);

    // The argument is the large activity, the record is its own, but the vault is
    // supplied from the smaller one.
    let ix = assert_funded_ix_with_slots(
        config_pda(),
        vesting_pda(large_id),
        vault_pda(small_id),
        vault_authority_pda(),
        win_mint,
        large_id,
    );
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(anchor_lang::error::ErrorCode::ConstraintSeeds)),
        "another activity's vault with the argument large_id → ConstraintSeeds: the vault's address is derived from the same activity_id as the record being checked: {r:?}"
    );
    assert_eq!(
        state_of(&env, small_id),
        before_small,
        "the smaller activity is intact"
    );
    assert_eq!(
        state_of(&env, large_id),
        before_large,
        "the larger activity is intact"
    );

    // A POSITIVE: the large activity's own vault lifts the rejection.
    env.fund_vault(large_id, REFERENCE);
    env.expire_blockhash();
    send_assert_funded(&mut env, large_id, &payer)
        .expect("the activity is funded by its own vault rather than the substituted one");
}

#[test]
fn assert_funded_rejects_vesting_slot_of_another_activity() {
    let (mut env, small_id, large_id) = setup_two();
    // The LARGER activity's vault is funded to exactly the SMALLER reference amount:
    // under the mutation (no seeds on vesting) reading the smaller activity's record
    // would make that amount sufficient and the check would pass.
    env.fund_vault(large_id, SMALLER_REFERENCE);
    let win_mint = env.win_mint;
    let payer = env.payer.insecure_clone();

    let before_small = state_of(&env, small_id);
    let before_large = state_of(&env, large_id);

    // The argument and the vault belong to the large activity while the record is
    // supplied from the smaller one.
    let ix = assert_funded_ix_with_slots(
        config_pda(),
        vesting_pda(small_id),
        vault_pda(large_id),
        vault_authority_pda(),
        win_mint,
        large_id,
    );
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(anchor_lang::error::ErrorCode::ConstraintSeeds)),
        "another activity's record with the argument large_id → ConstraintSeeds: the reference is checked against the requested activity rather than the substituted one: {r:?}"
    );
    assert_eq!(
        state_of(&env, small_id),
        before_small,
        "the smaller activity is intact"
    );
    assert_eq!(
        state_of(&env, large_id),
        before_large,
        "the larger activity is intact"
    );

    // A POSITIVE: topping up to the large activity's own reference amount lifts
    // the rejection.
    env.fund_vault(large_id, REFERENCE - SMALLER_REFERENCE);
    env.expire_blockhash();
    send_assert_funded(&mut env, large_id, &payer)
        .expect("the activity is covered by a check against its OWN record");
}

// ===================================================================
// [the u64 boundary] - the coverage is computed in u128
//
// Every case above works on amounts of order 10¹⁶, where the sum
// `vault.amount + claimed` does not leave `u64`, so the widening to `u128` is
// untouched by them: should anyone return the addition to `u64`, a FUNDED activity
// would get `MathOverflow` and the funding transaction (Arch §2) would not assemble
// at all - while there would be nothing left to undo it with, the mint authority
// being revoked in that very transaction. Here the reference amount is the maximum
// (`u64::MAX`) and the coverage is deliberately greater than `u64::MAX`, so such an
// edit turns the test red.
//
// The vault's balance is set by an honest mint: `u64::MAX` units mint freely at a
// zero supply. Only `claimed` is forged - its sole writer is `claim`, and it never
// produces a value that large honestly.
// ===================================================================

/// An activity's maximum reference amount: `u64::MAX` token units. `reference_amount`
/// has no business ceiling, so the value is reachable through the public
/// interface.
const MAX_REFERENCE: u64 = EDGE_MAX_REFERENCE;

/// An environment with a config and one activity for [`MAX_REFERENCE`]; the vault is
/// empty. Returns `(env, activity_id)`.
fn setup_max_reference() -> (TestEnv, u64) {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let activity_id = env.create_activity(&ActivitySpec::valid(MAX_REFERENCE));
    assert_eq!(
        env.vesting(activity_id).reference_amount,
        MAX_REFERENCE,
        "precondition: the reference amount was written unaltered"
    );
    (env, activity_id)
}

#[test]
fn assert_funded_passes_when_coverage_exceeds_u64() {
    let (mut env, id) = setup_max_reference();
    // A full vault plus one already-released unit: the coverage = `u64::MAX + 1` - one
    // more than `u64` can hold.
    env.fund_vault(id, MAX_REFERENCE);
    env.forge_claimed(id, 1);

    let payer = env.payer.insecure_clone();
    let r = send_assert_funded(&mut env, id, &payer);
    r.as_ref().expect(
        "vault.amount + claimed > u64::MAX: the coverage is computed in u128 and a funded activity must pass",
    );
}

#[test]
fn assert_funded_rejects_coverage_short_by_one_at_max_reference() {
    let (mut env, id) = setup_max_reference();
    // Coverage one unit below the maximum reference amount: `u64::MAX − 2` in the vault
    // plus one released unit.
    env.fund_vault(id, MAX_REFERENCE - 2);
    env.forge_claimed(id, 1);

    let payer = env.payer.insecure_clone();
    let r = send_assert_funded(&mut env, id, &payer);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "vault.amount + claimed == reference_amount − 1 → InsufficientVaultBalance even at the very edge of u64: {r:?}"
    );

    // A POSITIVE in the same environment: topping up by one unit brings the
    // coverage exactly to the maximum reference amount - the range's upper bound is
    // accepted.
    let win_mint = env.win_mint;
    env.mint_to(&win_mint, &vault_pda(id), 1);
    env.expire_blockhash();
    let r = send_assert_funded(&mut env, id, &payer);
    r.as_ref()
        .expect("vault.amount + claimed == u64::MAX - funded exactly at the boundary");
}

// ===================================================================
// Coverage from the payout's side
// ===================================================================

/// The day of the first claim in example V5 (§10.5, the "Ecosystem" bucket):
/// [`V2_VESTED_DAY_151`] has accrued = the TGE share plus five monthly tranches.
const DAY_151: i64 = 151;

/// The period following day 151 (`p = 6`): the under-mint case needs it - "the claim
/// reverts in two CONSECUTIVE periods", meaning the claim only grows.
const DAY_181: i64 = 181;

/// A stray transfer to the vault's address beyond the reference amount (Arch §7): it
/// does not move the unlock. The value is a multiple of neither a tranche nor the
/// reference amount.
const STRAY_TRANSFER: u64 = 3_141_592;

/// An environment with a config, an activity on the "Ecosystem" schedule for
/// [`REFERENCE`], and an empty vault. Unlike [`setup`], it also returns `tge_ts` - the
/// claim cases need to warp time by the schedule's days.
fn setup_for_claim() -> (TestEnv, u64, i64) {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let activity_id = env.create_activity(&ActivitySpec::valid(REFERENCE));
    (env, activity_id, params.tge_ts)
}

/// A claim by the activity's recipient (`actors.recipients[0]`); sent through
/// [`TestEnv::claim_as`] (the fee payer is the environment's `payer`).
#[allow(clippy::result_large_err)]
fn claim(env: &mut TestEnv, activity_id: u64) -> litesvm::types::TransactionResult {
    let recipient = env.actors.recipients[0].insecure_clone();
    env.claim_as(&recipient, activity_id, recipient.pubkey())
}

// ---------- a surplus does not increase the payout, topping up resumes it ----------

#[test]
fn stray_transfer_above_reference_does_not_increase_the_payout() {
    let (mut env, id, tge_ts) = setup_for_claim();
    // The emission plus a stray transfer to the same address: the program keeps no
    // accounting by balance (accounting-integrity), and the balance is only a gate to it.
    env.fund_vault(id, REFERENCE + STRAY_TRANSFER);
    env.warp_to_day(tge_ts, DAY_151);

    claim(&mut env, id).expect("a claim on an over-funded vault passes");

    let recipient = env.actors.recipients[0].pubkey();
    assert_eq!(
        env.token_balance(&recipient_ata(&recipient, &env.win_mint)),
        V2_VESTED_DAY_151,
        "exactly the scheduled tranche was released: the vault's surplus did not increase the unlock (I11)"
    );
    assert_eq!(
        env.vesting(id).claimed,
        V2_VESTED_DAY_151,
        "the accounting did not notice the surplus either"
    );
    assert_eq!(
        env.vault_balance(id),
        REFERENCE + STRAY_TRANSFER - V2_VESTED_DAY_151,
        "the vault holds the reference unreleased remainder plus the whole surplus"
    );

    // The coverage check after a REAL claim (with no forge of `claimed`): the unreleased
    // remainder is covered with room to spare.
    let payer = env.payer.insecure_clone();
    env.expire_blockhash();
    send_assert_funded(&mut env, id, &payer)
        .expect("after the claim the activity is still covered");
}

#[test]
fn topping_up_an_underfunded_vault_resumes_claims() {
    let (mut env, id, tge_ts) = setup_for_claim();
    // The under-mint is produced honestly: less was minted than unlocks by day 151.
    env.fund_vault(id, V2_VESTED_DAY_151 - 1);
    env.warp_to_day(tge_ts, DAY_151);

    let payer = env.payer.insecure_clone();
    let r = send_assert_funded(&mut env, id, &payer);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "precondition: the activity is underfunded and the check sees it: {r:?}"
    );
    let r = claim(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "an under-mint blocks the claim entirely - there is no partial payout (I3): {r:?}"
    );
    assert_eq!(env.vesting(id).claimed, 0, "the accounting did not move");

    // Topping up to the full reference amount. After the mint authority is revoked this
    // would no longer be a top-up mint but a direct transfer of existing $1WIN to the
    // vault's address (Arch §12.2) - for the program the two paths are
    // indistinguishable, it reads the balance.
    let win_mint = env.win_mint;
    env.mint_to(
        &win_mint,
        &vault_pda(id),
        REFERENCE - (V2_VESTED_DAY_151 - 1),
    );
    env.expire_blockhash();
    send_assert_funded(&mut env, id, &payer).expect("after the top-up the activity is covered");

    env.expire_blockhash();
    claim(&mut env, id).expect("the top-up resumes the claims");
    assert_eq!(
        env.vesting(id).claimed,
        V2_VESTED_DAY_151,
        "exactly what accrued by day 151 was released - no more, no less"
    );
}

// ---------- the accepted risk of an under-mint as expected behaviour ----------

#[test]
fn undermint_locks_claims_and_keeps_the_activity_unclosable() {
    let (mut env, id, tge_ts) = setup_for_claim();
    // The vault is funded with less than unlocks by day 151 already. From there
    // `claimable` only grows, so the under-mint is irreversible (Arch §12.2).
    let funded = V2_VESTED_DAY_151 - 1;
    env.fund_vault(id, funded);

    env.warp_to_day(tge_ts, DAY_151);
    let r = claim(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "period 5: the all-or-nothing claim reverts: {r:?}"
    );

    // The next period: the claim grew while the balance stayed the same - the rejection
    // repeats.
    env.warp_to_day(tge_ts, DAY_181);
    env.expire_blockhash();
    let r = claim(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "period 6: claimable only grows, so the rejection is permanent: {r:?}"
    );

    assert_eq!(
        env.vault_balance(id),
        funded,
        "what was minted stayed in the vault: the recipient got less than due but got nothing of anyone else's (I3)"
    );
    assert_eq!(env.vesting(id).claimed, 0, "zero was released");

    // The activity becomes permanently unclosable: `claimed < reference_amount`, and the
    // program has no sweep of the unclaimed (VE-14). This is an accepted AVAILABILITY
    // risk, pinned here as expected behaviour rather than a defect.
    let owner = env.actors.owner.insecure_clone();
    // `beneficiary_token` must belong to W (= rent_collector, the binding of §3),
    // otherwise the rejection would come from the InvalidBeneficiary account constraint
    // BEFORE the body and would mask the NotFullyClaimed gate under test.
    let withdrawal = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    let beneficiary_token = env.create_ata(&withdrawal, &win_mint);
    env.expire_blockhash();
    let ix = close_vesting_ix(
        owner.pubkey(),
        id,
        beneficiary_token,
        env.actors.rent_collector.pubkey(),
        win_mint,
    );
    let r = env.send_ixs(&[ix], &owner.pubkey(), &[&owner]);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotFullyClaimed)),
        "an underfunded activity cannot be closed: the rent and the remainder are locked indefinitely: {r:?}"
    );
    assert!(
        env.vesting_exists(id) && env.vault_exists(id),
        "both accounts are alive - which is exactly what Arch §12.2 describes as the residual risk"
    );

    // There is no cancellation either: TGE has arrived (I6). The only way out is topping
    // up the vault, and that is checked in
    // `topping_up_an_underfunded_vault_resumes_claims`.
    env.expire_blockhash();
    let payer = env.payer.insecure_clone();
    let r = send_assert_funded(&mut env, id, &payer);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "the coverage check shows the under-mint on-chain at any moment: {r:?}"
    );
}

// ===================================================================
// THE ATOMIC FUNDING TRANSACTION (Arch §2)
//
// `assert_funded`'s regular usage: one transaction before TGE - `mint_to` into each
// of the five vaults, then `assert_funded` for every activity, then
// `SetAuthority(MintTokens, None)` on the mint. The bundle's shape is defined by the
// shared `funding_tx_ixs` builder (the same one the surfpool run in
// tests/vesting-e2e executes), so the transaction checked here and the one measured
// there are one and the same.
// ===================================================================

/// The SPL Token program's `TokenError::FixedSupply` code: the sixth variant of its
/// enum (`NotRentExempt` = 0 … `FixedSupply` = 5), arriving as `Custom(5)`. It is what
/// `mint_to` answers on a mint with `mint_authority == None`. A literal in the test:
/// this is a foreign interface, and taking its code from the crate would mean checking
/// the encoding against itself; Anchor codes start at 6000 and cannot be confused with
/// it. The value was confirmed by the first execution.
const SPL_ERROR_FIXED_SUPPLY: u32 = 5;

/// The number of instructions in the full bundle of Arch §2 with five vesting buckets:
/// 1 `mint_to` into TokenSale's `token_vault` + 5 `mint_to` into the vesting vaults +
/// 5 `assert_funded` + 1 `SetAuthority`.
const FUNDING_IX_COUNT: usize = 12;

/// The offset of the first `assert_funded` in the transaction: the checks come AFTER
/// all six mints (the order of Arch §2), so the index of activity `i`'s check is
/// `6 + i`.
const FIRST_ASSERT_IX: u8 = 6;

/// Solana's packet size limit in bytes: a larger transaction never reaches the
/// validator at all.
///
/// The measured size of the full bundle is 864 bytes (368 bytes of headroom): the
/// TokenSale link, five vesting mints, five checks and the authority revocation. The
/// vesting half of the same bundle is 817 bytes, and it is exactly that half which was
/// measured on a real validator in tests/vesting-e2e (TokenSale is not deployed
/// there; the size does not depend on the blockhash). The verdict: it fits, and there
/// is no need to split the bundle.
const PACKET_LIMIT: usize = 1232;

/// A proxy for TokenSale's emission amount in the bundle. It does not affect the
/// transaction's size (an amount in `mint_to`'s data is always 8 bytes) and is not the
/// sale's reference: the sale's own suite checks its amounts.
const SALE_VAULT_SHARE: u64 = 1_000_000;

/// The total emission of the five buckets - a hand-written literal:
/// `V1 5×10¹⁵ + V2 10¹⁶ + Community 1000 + Treasury 4×10¹⁵ + Liquidity 10¹⁶`
/// = `29,000,000,000,000,000 + 1,000` token units.
const TOTAL_EMISSION: u64 = 29_000_000_000_001_000;

const _: () = assert!(
    TOTAL_EMISSION
        == V1_REFERENCE + V2_REFERENCE + V4_REFERENCE + V3_REFERENCE + LIQUIDITY_REFERENCE,
    "the total-emission literal diverged from the buckets' reference amounts"
);

/// An environment with a config and the five activities of §10.5; the vaults are empty
/// and every activity has its own recipient. Returns `(env, tge_ts, the recipients, the
/// (activity_id, reference_amount) pairs in table order)`.
fn setup_five() -> (TestEnv, i64, Vec<Keypair>, Vec<(u64, u64)>) {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let mut recipients = Vec::new();
    let mut funding = Vec::new();
    for spec in ActivitySpec::sections_10_5() {
        let recipient = env.new_wallet();
        let activity_id = env.create_activity_for(&recipient.pubkey(), &spec);
        funding.push((activity_id, spec.reference_amount));
        recipients.push(recipient);
    }
    (env, params.tge_ts, recipients, funding)
}

/// The full shape of the funding transaction (Arch §2): `mint_to` into TokenSale's
/// `token_vault`, then `mint_to` into every vesting vault, then `assert_funded` for
/// every activity, then `SetAuthority(MintTokens, None)`. The vesting half is
/// assembled by the shared `funding_tx_ixs` builder - the same one the surfpool run
/// executes.
fn full_funding_ixs(
    mint: &Pubkey,
    mint_authority: &Pubkey,
    sale_vault: &Pubkey,
    funding: &[(u64, u64)],
) -> Vec<Instruction> {
    let sale_mint = spl_token_interface::instruction::mint_to(
        &spl_token_interface::id(),
        mint,
        sale_vault,
        mint_authority,
        &[],
        SALE_VAULT_SHARE,
    )
    .expect("the mint_to instruction into TokenSale's token_vault");
    let mut ixs = vec![sale_mint];
    ixs.extend(funding_tx_ixs(mint, mint_authority, funding));
    ixs
}

/// The index of the instruction the transaction failed on. It is needed to tell "the
/// bundle was brought down by the coverage check" apart from "one of the mints brought
/// it down": their error codes differ, but without the index it is unclear whether
/// execution reached the checks at all.
fn failing_instruction_index(result: &TransactionResult) -> Option<u8> {
    match result {
        Err(failed) => match failed.err {
            TransactionError::InstructionError(index, _) => Some(index),
            _ => None,
        },
        Ok(_) => None,
    }
}

#[test]
fn funding_transaction_funds_five_vaults_and_revokes_mint_authority() {
    let (mut env, tge_ts, recipients, funding) = setup_five();
    let payer = env.payer.insecure_clone();
    let mint = env.win_mint;

    assert_eq!(
        env.mint_authority(&mint),
        Some(payer.pubkey()),
        "precondition: the issuer still holds the right to mint further"
    );
    assert_eq!(
        env.mint_supply(&mint),
        0,
        "precondition: not a single unit was issued before the funding"
    );
    for (id, _) in &funding {
        assert_eq!(
            env.vault_balance(*id),
            0,
            "precondition: the vaults are empty"
        );
    }

    // The bundle's TokenSale link per §2: a foreign token account under the same mint.
    let sale_vault = env.create_ata(&env.actors.beneficiary.pubkey(), &mint);
    let ixs = full_funding_ixs(&mint, &payer.pubkey(), &sale_vault, &funding);
    assert_eq!(
        ixs.len(),
        FUNDING_IX_COUNT,
        "the bundle of Arch §2: the sale's mint, five vesting mints, five checks, the authority revocation"
    );

    // The size is checked here rather than only in the surfpool run: that one is
    // `#[ignore]`d and does not go into CI, while "it did not fit into the packet" must
    // be caught by the suite, not at TGE. The value does not depend on the blockhash and
    // matches the real one.
    let size = env.tx_size(&ixs, &payer.pubkey(), &[&payer]);
    println!("the full funding transaction (the sale plus the five buckets): {size} bytes out of {PACKET_LIMIT}");
    assert!(
        size <= PACKET_LIMIT,
        "the funding transaction does not fit into the packet: {size} > {PACKET_LIMIT}"
    );

    env.send_ixs(&ixs, &payer.pubkey(), &[&payer])
        .expect("funding all five buckets to exactly their reference amounts passes in a single transaction");

    for (id, reference) in &funding {
        assert_eq!(
            env.vault_balance(*id),
            *reference,
            "activity {id}: the vault holds exactly the bucket's reference amount"
        );
    }
    assert_eq!(
        env.token_balance(&sale_vault),
        SALE_VAULT_SHARE,
        "exactly its own amount went into the sale's vault"
    );
    assert_eq!(
        env.mint_supply(&mint),
        TOTAL_EMISSION + SALE_VAULT_SHARE,
        "exactly as much was issued as went into the five vesting vaults and the sale's vault - not a single unit past them"
    );
    assert_eq!(
        env.mint_authority(&mint),
        None,
        "the bundle's closing link revoked the right to issue further (T-6)"
    );

    // No further issuance is possible after the revocation: SPL Token rejects `mint_to`
    // itself.
    let first_vault = funding[0].0;
    let before = env.vault_balance(first_vault);
    env.expire_blockhash();
    let r = env.send_ixs(
        &[mint_to_vault_ix(&mint, &payer.pubkey(), first_vault, 1)],
        &payer.pubkey(),
        &[&payer],
    );
    assert_eq!(
        custom_error_code(&r),
        Some(SPL_ERROR_FIXED_SUPPLY),
        "after SetAuthority(None) the mint is fixed: mint_to → TokenError::FixedSupply: {r:?}"
    );
    assert_eq!(
        env.vault_balance(first_vault),
        before,
        "the rejected further issuance did not touch the vault"
    );
    assert_eq!(
        env.mint_supply(&mint),
        TOTAL_EMISSION + SALE_VAULT_SHARE,
        "supply did not move"
    );

    // Revoking the mint authority does not break the payout: a claim moves what was
    // already issued out of the vault rather than out of the mint.
    let marketing = funding[0].0;
    env.claim_at_day(tge_ts, 1, marketing, &recipients[0])
        .expect("100% at TGE: the bucket is released in full at the TGE instant - after the authority revocation too");
    assert_eq!(
        env.recipient_ata_balance(&recipients[0].pubkey()),
        V1_VESTED_FROM_TGE,
        "Marketing: the recipient received the bucket's whole reference amount"
    );
    assert_eq!(
        env.vault_balance(marketing),
        0,
        "the Marketing vault emptied out by exactly what was released"
    );

    let liquidity = funding[4].0;
    env.claim_at_day(tge_ts, 31, liquidity, &recipients[4])
        .expect("Liquidity: the first monthly tranche is released on day 31");
    assert_eq!(
        env.recipient_ata_balance(&recipients[4].pubkey()),
        LIQUIDITY_VESTED_DAY_31,
        "Liquidity: the TGE share plus the first monthly tranche (17.5% of the reference)"
    );
    assert_eq!(
        env.mint_authority(&mint),
        None,
        "the claims passed with the authority revoked - the right to issue further was not restored"
    );
}

#[test]
fn funding_transaction_reverts_entirely_on_one_undermined_vault() {
    let (mut env, _tge_ts, _recipients, funding) = setup_five();
    let payer = env.payer.insecure_clone();
    let mint = env.win_mint;

    // The under-mint is placed on the third bucket (Community, a reference of 1000 units):
    // one unit below the reference amount - the smallest possible shortfall.
    const UNDERMINED: usize = 2;
    let mut short = funding.clone();
    short[UNDERMINED].1 -= 1;

    let sale_vault = env.create_ata(&env.actors.beneficiary.pubkey(), &mint);
    let ixs = full_funding_ixs(&mint, &payer.pubkey(), &sale_vault, &short);
    let r = env.send_ixs(&ixs, &payer.pubkey(), &[&payer]);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "an under-mint of one vault fails the whole funding transaction: {r:?}"
    );
    assert_eq!(
        failing_instruction_index(&r),
        Some(FIRST_ASSERT_IX + UNDERMINED as u8),
        "it was brought down by the underfunded activity's check rather than by a mint or a neighbouring check: {r:?}"
    );

    // A full rollback: the other buckets' mints, the sale's mint and the authority
    // revocation did not survive.
    for (id, _) in &funding {
        assert_eq!(
            env.vault_balance(*id),
            0,
            "activity {id}: the mint rolled back along with the transaction"
        );
    }
    assert_eq!(
        env.token_balance(&sale_vault),
        0,
        "the mint into the sale's vault rolled back along with the transaction, even though it stands BEFORE the failing check"
    );
    assert_eq!(
        env.mint_supply(&mint),
        0,
        "not a single unit was issued: the bundle is atomic"
    );
    assert_eq!(
        env.mint_authority(&mint),
        Some(payer.pubkey()),
        "the right to issue further is in place - that is exactly why the check stands BEFORE SetAuthority (Arch §2)"
    );

    // A POSITIVE in the same environment: the full amounts carry the bundle
    // through.
    env.expire_blockhash();
    let ixs = full_funding_ixs(&mint, &payer.pubkey(), &sale_vault, &funding);
    env.send_ixs(&ixs, &payer.pubkey(), &[&payer])
        .expect("the same bundle with full funding passes");
    assert_eq!(
        env.mint_supply(&mint),
        TOTAL_EMISSION + SALE_VAULT_SHARE,
        "the repeat after fixing the under-mint issued exactly the vesting total plus the sale's amount"
    );
    assert_eq!(
        env.mint_authority(&mint),
        None,
        "the right to issue further was revoked only on the successful bundle"
    );
}
