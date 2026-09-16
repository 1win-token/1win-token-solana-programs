//! `create_vesting` (Arch §9.5) - the positive plus the negative matrix.
//!
//! Why the tests are laid out the way they are.
//!
//! 1. The instruction is protected by three layers, and every negative addresses
//!    exactly one: the context's structure (`has_one`, `seeds`, `address`, the
//!    `SystemAccount` type) fires during account RESOLUTION, before the body; the
//!    argument validation happens in the body; and the effects are the two created
//!    accounts plus the counter's increment. So every negative deviates EXACTLY one
//!    field of the valid set ([`ActivitySpec::valid`] / the canonical slots) and leaves
//!    everything else passable - otherwise the failure could not be attributed to the
//!    guard under test.
//! 2. The order of the checks in the body is observable (Arch §9.5): the TGE gate → the
//!    recipient (zero, `vault_authority`, off-curve) → `reference_amount > 0` → the
//!    `label` length → the schedule (`InvalidBps` → the share coupling → the
//!    `cliff + M` ceiling). A schedule
//!    negative must supply a valid amount and label, or it would fail earlier and
//!    verify the wrong invariant.
//! 3. Both `init`s are declared BEFORE the `recipient` slot and resolve ahead of it,
//!    but a rejection at any layer rolls the whole transaction back. So every negative
//!    asserts the triple "the counter did not move + there is no record + there is no
//!    vault" ([`assert_activity_not_created`]): a partially created activity would
//!    occupy an address and break the next `create_vesting`.
//! 4. `activity_count` is an id allocator rather than a counter of amounts: it grows
//!    only in the body after a success. Its immutability on a negative is a separate
//!    assertion rather than a consequence of "the account is absent".
//! 5. The positive runs in the same config as the rejection: it proves the
//!    rejection was caused by the edited field rather than by a broken test
//!    environment.
//! 6. `MAX_SCHEDULE_MONTHS` is imported from the program, and a pinning assertion on
//!    the literal stands next to its only consumer: the test's input is derived from
//!    the ceiling (cliff = ceiling − M), so the import keeps the boundary consistent
//!    while the pinning test keeps a change of the constant itself from passing
//!    silently - an import without it would be comparing the code against itself
//!    (tests-bind-to-real-code). `MAX_LABEL_LEN` is written as a literal: it IS the
//!    value under test and the test's input is not derived from it. Both boundaries are
//!    checked in pairs, "the boundary passes / the boundary+1 is rejected".
//!
//! ===== DELIBERATELY DEFERRED =====
//! - The full 10×5 role matrix and the 10×3 TGE matrix; here five
//!   wrong actors and the `now == tge_ts` point are covered for one instruction.
//! - Substituting the derived `vesting` / `vault` slots away from the current counter,
//!   and the `#[event_cpi]` service slots - the substitution matrix. What is
//!   substituted here is `config`, `token_mint`, `recipient` and
//!   `vault_authority`: the first two carry a binding, the third is the only slot whose
//!   type is itself the validation, and the fourth holds the "recipient !=
//!   vault_authority" gate.
//! - `cancel_vesting` / `assert_funded` over a created activity
//!   (`cancel_close.rs`, `funding.rs`); the claim - T3.x.
//! - The `create_vesting` CU benchmark.
//! - `MathOverflow` inside `tranches_m` (an overflow of `rest + step`): unreachable on
//!   `u16` arguments - `10000 + 10000` does not overflow in u32; the branch is covered
//!   by the math unit tests (T0.x).

use anchor_lang::error::ErrorCode;
use litesvm::types::TransactionResult;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting::errors::VestingError;
use vesting::events::VestingCreated;
use vesting::state::MAX_SCHEDULE_MONTHS;
use vesting_test_common::vesting_expected::vesting::{
    BRIDGE_CLIFF_MONTHS, BRIDGE_MONTHLY_BPS, BRIDGE_TGE_BPS, COMMUNITY_CLIFF_MONTHS,
    COMMUNITY_MONTHLY_BPS, COMMUNITY_TGE_BPS, ECOSYSTEM_CLIFF_MONTHS, ECOSYSTEM_MONTHLY_BPS,
    ECOSYSTEM_TGE_BPS, EDGE_MAX_REFERENCE, EDGE_M_BPS_0_1, LIQUIDITY_CLIFF_MONTHS,
    LIQUIDITY_MONTHLY_BPS, LIQUIDITY_REFERENCE, LIQUIDITY_TGE_BPS, MARKETING_CLIFF_MONTHS,
    MARKETING_MONTHLY_BPS, MARKETING_TGE_BPS, TREASURY_CLIFF_MONTHS, TREASURY_MONTHLY_BPS,
    TREASURY_TGE_BPS, V1_REFERENCE, V2_REFERENCE, V3_REFERENCE, V4_REFERENCE,
};
use vesting_test_common::{
    config_pda, create_vesting_ix, create_vesting_ix_with_slots, custom_error_code, decode_event,
    err, event_authority_pda, foreign_program_pda, vault_authority_pda, vault_pda, vesting_pda,
    ActivitySpec, TestEnv, DEFAULT_TGE_OFFSET,
};

/// The label length ceiling in BYTES (Arch §9.5). A literal in the test:
/// `vesting::state::MAX_LABEL_LEN` IS the value under test.
const MAX_LABEL_BYTES: usize = 32;

/// A label of exactly [`MAX_LABEL_BYTES`] bytes made of TWO-BYTE characters: 16
/// accented letters × 2 B = 32 B. It passes.
const LABEL_32_BYTES: &str = "éééééééééééééééé";

/// A label of 33 bytes: the same 16 letters plus a one-byte `!` - 17 characters,
/// 33 B. The key negative for counting in bytes: by CHARACTERS (17 ≤ 32) it would have
/// passed.
const LABEL_33_BYTES: &str = "éééééééééééééééé!";

/// `create_vesting` from the given signer onto the next free id.
/// We return the native litesvm TransactionResult as is - just like `send_ixs`.
#[allow(clippy::result_large_err)]
fn create_as(
    env: &mut TestEnv,
    signer: &Keypair,
    recipient: Pubkey,
    spec: &ActivitySpec,
) -> TransactionResult {
    let next_id = env.next_activity_id();
    let mint = env.win_mint;
    let ix = create_vesting_ix(signer.pubkey(), recipient, next_id, mint, spec);
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// `create_vesting` from the `owner` actor - the main path of every case in the file.
#[allow(clippy::result_large_err)]
fn create(env: &mut TestEnv, recipient: Pubkey, spec: &ActivitySpec) -> TransactionResult {
    let owner = env.actors.owner.insecure_clone();
    create_as(env, &owner, recipient, spec)
}

/// The rejection left no trace: the id counter is in place and there is neither a
/// record nor a vault at the rejected activity's addresses.
fn assert_activity_not_created(env: &TestEnv, id: u64, count_before: u64, case: &str) {
    assert_eq!(
        env.vesting_config().activity_count,
        count_before,
        "{case}: the rejection must not move the id allocator"
    );
    assert!(
        !env.vesting_exists(id),
        "{case}: the activity record was not created"
    );
    assert!(!env.vault_exists(id), "{case}: the vault was not created");
}

/// The environment's six actors, none of which is the config's Owner. Admin stands in
/// the list on equal terms: only Owner creates an activity - it pays the new accounts'
/// rent.
fn wrong_owners(env: &TestEnv) -> Vec<(&'static str, Keypair)> {
    vec![
        ("admin", env.actors.admin.insecure_clone()),
        ("operator", env.actors.operator.insecure_clone()),
        ("recipient", env.actors.recipients[0].insecure_clone()),
        ("beneficiary", env.actors.beneficiary.insecure_clone()),
        ("rent_collector", env.actors.rent_collector.insecure_clone()),
        ("stranger", env.actors.stranger.insecure_clone()),
    ]
}

// ===================================================================
// POSITIVE: the fields, the vault, the event, consecutive ids
// ===================================================================

#[test]
fn create_vesting_stores_fields_and_opens_empty_vault() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();

    // The "adapter" schedule (vesting_expected::vesting): all four values are pairwise
    // distinguishable (4×10¹⁵ / 1000 / 500 / 9), so swapping any two fields on the way
    // into the account shows up in an assertion rather than being masked by a
    // coincidence.
    let spec = ActivitySpec {
        reference_amount: V3_REFERENCE,
        tge_bps: BRIDGE_TGE_BPS,
        monthly_bps: BRIDGE_MONTHLY_BPS,
        cliff_months: BRIDGE_CLIFF_MONTHS,
        label: String::from("treasury"),
    };

    let r = create(&mut env, recipient, &spec);
    r.as_ref().expect("a valid create_vesting passes");

    let v = env.vesting(0);
    assert_eq!(
        v.recipient, recipient,
        "the recipient was taken from the context's SLOT"
    );
    assert_eq!(
        v.reference_amount, spec.reference_amount,
        "reference_amount was written from the argument"
    );
    assert_eq!(
        v.claimed, 0,
        "a new activity has released nothing - claimed starts at zero"
    );
    assert_eq!(v.tge_bps, spec.tge_bps, "tge_bps was written");
    assert_eq!(v.monthly_bps, spec.monthly_bps, "monthly_bps was written");
    assert_eq!(
        v.cliff_months, spec.cliff_months,
        "cliff_months was written"
    );

    assert_eq!(
        env.vesting_config().activity_count,
        1,
        "the id counter moved by exactly one activity"
    );

    // The vault: created, empty and bound to the config's mint under the PDA owner.
    assert!(env.vault_exists(0), "the activity's vault was created");
    assert_eq!(
        env.vault_balance(0),
        0,
        "creating an activity does not fund it (Arch §2: the mint is a separate transaction)"
    );
    let (mint, authority) = env.token_account_mint_owner(&vault_pda(0));
    assert_eq!(
        mint,
        env.vesting_config().token_mint,
        "the vault holds the $1WIN pinned in the config"
    );
    assert_eq!(
        authority,
        vault_authority_pda(),
        "the vault's owner is the program's PDA rather than Owner: otherwise the tokens could be taken out bypassing the schedule"
    );

    let ev: VestingCreated = decode_event(&r).expect("create_vesting emits VestingCreated");
    assert_eq!(ev.activity_id, 0, "the event: the first id is zero");
    assert_eq!(ev.recipient, recipient, "the event: the recipient");
    assert_eq!(
        ev.reference_amount, spec.reference_amount,
        "the event: reference_amount"
    );
    assert_eq!(ev.tge_bps, spec.tge_bps, "the event: tge_bps");
    assert_eq!(ev.monthly_bps, spec.monthly_bps, "the event: monthly_bps");
    assert_eq!(
        ev.cliff_months, spec.cliff_months,
        "the event: cliff_months"
    );
    assert_eq!(
        ev.label, spec.label,
        "the label is not stored on-chain (Arch §5.2) - the event is its only carrier"
    );
}

#[test]
fn second_create_vesting_takes_next_id_on_distinct_pdas() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let first_recipient = env.actors.recipients[0].pubkey();
    let second_recipient = env.actors.recipients[1].pubkey();
    assert_ne!(
        first_recipient, second_recipient,
        "the activities have different recipients - otherwise overwriting the first record with the second would go unnoticed"
    );

    let first_spec = ActivitySpec::valid(V2_REFERENCE);
    let second_spec = ActivitySpec::valid(V3_REFERENCE);

    let r0 = create(&mut env, first_recipient, &first_spec);
    r0.as_ref().expect("the first activity is created");
    env.expire_blockhash();
    let r1 = create(&mut env, second_recipient, &second_spec);
    r1.as_ref().expect("the second activity is created");

    let ev0: VestingCreated = decode_event(&r0).expect("the first VestingCreated");
    let ev1: VestingCreated = decode_event(&r1).expect("the second VestingCreated");
    assert_eq!(
        ev0.activity_id, 0,
        "the first activity's event carries id 0"
    );
    assert_eq!(
        ev1.activity_id, 1,
        "the second activity's event carries id 1 - the counter issues ids consecutively"
    );

    assert_eq!(
        env.vesting_config().activity_count,
        2,
        "two activities - the counter equals two"
    );
    assert_ne!(
        vesting_pda(0),
        vesting_pda(1),
        "the records live at different PDAs (the id is part of the seeds)"
    );
    assert_ne!(
        vault_pda(0),
        vault_pda(1),
        "the vaults live at different PDAs - a shared vault would mix the buckets"
    );

    let v0 = env.vesting(0);
    let v1 = env.vesting(1);
    assert_eq!(
        v0.recipient, first_recipient,
        "the first record was not overwritten by the second"
    );
    assert_eq!(
        v1.recipient, second_recipient,
        "the second record is in its own place"
    );
    assert_eq!(
        v0.reference_amount, first_spec.reference_amount,
        "the first activity's amount is untouched"
    );
    assert_eq!(
        v1.reference_amount, second_spec.reference_amount,
        "the second activity's amount is its own"
    );
    assert!(
        env.vault_exists(0) && env.vault_exists(1),
        "every activity has its own vault"
    );
}

// ===================================================================
// the amounts and the share ceiling
// ===================================================================

#[test]
fn create_vesting_rejects_zero_reference_amount() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();

    let r = create(&mut env, recipient, &ActivitySpec::valid(0));
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidReferenceAmount)),
        "reference_amount == 0 → InvalidReferenceAmount: an activity with no obligation would occupy an id and a vault: {r:?}"
    );
    assert_activity_not_created(&env, 0, 0, "a zero amount");

    // A POSITIVE one value over: 1 unit is the minimum permitted amount.
    env.expire_blockhash();
    create(&mut env, recipient, &ActivitySpec::valid(1))
        .expect("1 token unit is the minimum permitted amount");
    assert_eq!(
        env.vesting(0).reference_amount,
        1,
        "the boundary to the unit: 0 was rejected and 1 accepted"
    );
}

#[test]
fn create_vesting_accepts_u64_max_reference_amount() {
    // There is NO ceiling on reference_amount: only `> 0` is validated.
    // The case pins that as a requirement - the appearance of a sanity ceiling must turn
    // red here rather than quietly cutting off a legitimate amount.
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();

    let spec = ActivitySpec::valid(EDGE_MAX_REFERENCE);
    let r = create(&mut env, recipient, &spec);
    r.as_ref()
        .expect("reference_amount = u64::MAX is accepted - there is no ceiling");
    assert_eq!(
        env.vesting(0).reference_amount,
        EDGE_MAX_REFERENCE,
        "the maximum amount was written without truncation"
    );
    let ev: VestingCreated = decode_event(&r).expect("VestingCreated");
    assert_eq!(
        ev.reference_amount, EDGE_MAX_REFERENCE,
        "the event carries that same maximum amount"
    );
}

#[test]
fn create_vesting_rejects_bps_above_denominator() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let base = ActivitySpec::valid(V2_REFERENCE);

    // 10001 is the first value beyond 100%. BOTH fields are checked: the shared
    // `> BPS_DENOM` check covers them together, but losing one of the two operands in a
    // refactor is visible only per field.
    let over: [(&str, ActivitySpec); 2] = [
        (
            "tge_bps",
            ActivitySpec {
                tge_bps: 10_001,
                ..base.clone()
            },
        ),
        (
            "monthly_bps",
            ActivitySpec {
                monthly_bps: 10_001,
                ..base.clone()
            },
        ),
    ];
    for (field, spec) in over {
        env.expire_blockhash();
        let r = create(&mut env, recipient, &spec);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::InvalidBps)),
            "{field} = 10001 (> 100%) → InvalidBps: {r:?}"
        );
        assert_activity_not_created(&env, 0, 0, field);
    }

    // A POSITIVE: exactly 10000 in `tge_bps` is a permitted share (the "100% at TGE"
    // bucket).
    env.expire_blockhash();
    let at_ceiling = ActivitySpec {
        tge_bps: 10_000,
        monthly_bps: 0,
        cliff_months: 0,
        ..base
    };
    create(&mut env, recipient, &at_ceiling).expect("tge_bps == 10000 is a lawful share");
    assert_eq!(
        env.vesting(0).tge_bps,
        10_000,
        "the share boundary is inclusive: 10000 was accepted and 10001 rejected"
    );
}

// ===================================================================
// the share coupling (on-chain negatives paired with the math unit tests)
// ===================================================================

#[test]
fn create_vesting_rejects_broken_share_pairing() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let base = ActivitySpec::valid(V2_REFERENCE);

    // The coupling of Arch §9.5: `monthly_bps == 0 ⟹ tge_bps == 10000` and
    // `monthly_bps > 0 ⟹ tge_bps < 10000`. Violating either side means the schedule does
    // not unlock the reference amount in full, or unlocks more than it.
    let broken: [(&str, ActivitySpec); 3] = [
        (
            "monthly_bps == 0 with tge_bps != 10000 (the schedule would stall at 10%)",
            ActivitySpec {
                tge_bps: 1_000,
                monthly_bps: 0,
                cliff_months: 0,
                ..base.clone()
            },
        ),
        (
            "monthly_bps > 0 with tge_bps == 10000 (the shares would sum above the reference amount)",
            ActivitySpec {
                tge_bps: 10_000,
                monthly_bps: 500,
                cliff_months: 0,
                ..base.clone()
            },
        ),
        (
            "(0, 0) - it never unlocks",
            ActivitySpec {
                tge_bps: 0,
                monthly_bps: 0,
                cliff_months: 0,
                ..base.clone()
            },
        ),
    ];
    for (case, spec) in broken {
        env.expire_blockhash();
        let r = create(&mut env, recipient, &spec);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::InvalidSchedule)),
            "{case} → InvalidSchedule: {r:?}"
        );
        assert_activity_not_created(&env, 0, 0, case);
    }

    // A POSITIVE for both lawful branches of the coupling in the same config.
    env.expire_blockhash();
    let all_at_tge = ActivitySpec {
        tge_bps: 10_000,
        monthly_bps: 0,
        cliff_months: 0,
        ..base.clone()
    };
    create(&mut env, recipient, &all_at_tge).expect("(10000, 0) - the \\\"100% at TGE\\\" branch");
    env.expire_blockhash();
    create(&mut env, recipient, &base).expect("(1000, 500) - the monthly-tranche branch");
    assert_eq!(
        env.vesting_config().activity_count,
        2,
        "exactly the two lawful combinations passed and the three violating ones did not"
    );
}

// ===================================================================
// a cliff with no monthly tranches
// ===================================================================

#[test]
fn create_vesting_rejects_cliff_without_monthly_tranches() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();

    // The `monthly_bps == 0` branch (100% at TGE) does not read `cliff_months` in a
    // single line of `vested()`. Meaningful input must not be silently ignored: a
    // cliff that delays nothing is an operator error, and it must be visible at the entry
    // rather than after nine months of waiting.
    let with_cliff = ActivitySpec {
        reference_amount: V1_REFERENCE,
        tge_bps: MARKETING_TGE_BPS,
        monthly_bps: MARKETING_MONTHLY_BPS,
        cliff_months: 1,
        label: String::from("marketing"),
    };
    let r = create(&mut env, recipient, &with_cliff);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidSchedule)),
        "monthly_bps == 0 with cliff_months > 0 → InvalidSchedule: the cliff would be ignored by the payout: {r:?}"
    );
    assert_activity_not_created(&env, 0, 0, "a cliff with no monthly tranches");

    // A POSITIVE: the same bucket without a cliff passes - the rejection was the cliff's doing.
    env.expire_blockhash();
    let without_cliff = ActivitySpec {
        cliff_months: MARKETING_CLIFF_MONTHS,
        ..with_cliff
    };
    create(&mut env, recipient, &without_cliff).expect("\\\"100% at TGE\\\" with no cliff passes");
    let v = env.vesting(0);
    assert_eq!(
        v.monthly_bps, 0,
        "a bucket with no monthly tranches was written"
    );
    assert_eq!(v.cliff_months, 0, "the cliff is zero");
}

// ===================================================================
// the schedule ceiling and the five real buckets of §10.5
// ===================================================================

#[test]
fn create_vesting_schedule_months_boundary() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();

    // A pinning test for the imported ceiling: the constant is fixed by agreement (120
    // months) and changing it moves both boundaries of this test - without the assertion
    // it would pass silently (the expectation would move along with the code).
    assert_eq!(
        MAX_SCHEDULE_MONTHS, 120,
        "the `cliff_months + M` schedule ceiling is fixed by agreement"
    );

    // Derived by hand: at `tge_bps = 0, monthly_bps = 100`,
    // `M = ceil((10000 − 0) / 100) = 100`. Then `cliff + M` equals 120 at a cliff of 20
    // (exactly the ceiling) and 121 at a cliff of 21 (the ceiling plus one month).
    let m: u32 = 100;
    let cliff_at_ceiling =
        u16::try_from(MAX_SCHEDULE_MONTHS - m).expect("the cliff at the boundary in u16");
    assert_eq!(
        cliff_at_ceiling, 20,
        "the boundary's arithmetic: 120 − M(=100) = 20 months of cliff"
    );
    let base = ActivitySpec {
        reference_amount: V2_REFERENCE,
        tge_bps: 0,
        monthly_bps: 100,
        cliff_months: cliff_at_ceiling + 1,
        label: String::from("boundary"),
    };

    // The ceiling plus one month.
    let r = create(&mut env, recipient, &base);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidSchedule)),
        "cliff + M = 121 > MAX_SCHEDULE_MONTHS = 120 → InvalidSchedule: {r:?}"
    );
    assert_activity_not_created(&env, 0, 0, "a schedule one month longer than the ceiling");

    // A POSITIVE exactly on the boundary: 20 + 100 = 120.
    env.expire_blockhash();
    let at_ceiling = ActivitySpec {
        cliff_months: cliff_at_ceiling,
        ..base
    };
    create(&mut env, recipient, &at_ceiling)
        .expect("cliff + M is exactly 120 - the ceiling is inclusive");
    let v = env.vesting(0);
    assert_eq!(
        v.cliff_months, cliff_at_ceiling,
        "the boundary to the month: 121 was rejected and 120 accepted"
    );
    assert_eq!(
        v.monthly_bps, 100,
        "the monthly tranche's share was written"
    );
}

#[test]
fn create_vesting_rejects_monthly_bps_of_one() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();

    // `monthly_bps = 1` yields `M = ceil(10000 / 1) = 10000` months (≈833 years) -
    // exactly the schedule the ceiling was introduced for: the activity would become
    // eternal, and it cannot be closed until the full payout (I8).
    assert_eq!(
        EDGE_M_BPS_0_1, 10_000,
        "the reference: M at (tge_bps 0, monthly_bps 1) is 10000 tranches"
    );
    let endless = ActivitySpec {
        reference_amount: V2_REFERENCE,
        tge_bps: 0,
        monthly_bps: 1,
        cliff_months: 0,
        label: String::from("endless"),
    };
    let r = create(&mut env, recipient, &endless);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidSchedule)),
        "monthly_bps = 1 (M = 10000 months) → InvalidSchedule: {r:?}"
    );
    assert_activity_not_created(&env, 0, 0, "monthly_bps = 1");

    // A POSITIVE at the minimum passing share with no cliff:
    // `M = ceil(10000 / 84) = 120` (84 × 119 = 9996 < 10000, 84 × 120 = 10080), that is,
    // exactly the ceiling; 83 would give `M = 121` and a rejection.
    env.expire_blockhash();
    let shortest_allowed = ActivitySpec {
        monthly_bps: 84,
        ..endless
    };
    create(&mut env, recipient, &shortest_allowed)
        .expect("monthly_bps = 84 yields M = 120 - the longest permitted schedule");
    assert_eq!(
        env.vesting(0).monthly_bps,
        84,
        "the minimum share that fits the ceiling was accepted"
    );
}

#[test]
fn create_vesting_accepts_all_five_real_sections() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();

    // The five schedules of Arch §10.5 - exactly the ones that will go to mainnet. Should
    // the validation turn out stricter than the spec, it is this case that turns red
    // rather than an abstract boundary.
    let sections: [(&str, ActivitySpec); 5] = [
        (
            "Marketing (100% TGE)",
            ActivitySpec {
                reference_amount: V1_REFERENCE,
                tge_bps: MARKETING_TGE_BPS,
                monthly_bps: MARKETING_MONTHLY_BPS,
                cliff_months: MARKETING_CLIFF_MONTHS,
                label: String::from("marketing"),
            },
        ),
        (
            "Ecosystem",
            ActivitySpec {
                reference_amount: V2_REFERENCE,
                tge_bps: ECOSYSTEM_TGE_BPS,
                monthly_bps: ECOSYSTEM_MONTHLY_BPS,
                cliff_months: ECOSYSTEM_CLIFF_MONTHS,
                label: String::from("ecosystem"),
            },
        ),
        (
            "Community Rewards",
            ActivitySpec {
                reference_amount: V4_REFERENCE,
                tge_bps: COMMUNITY_TGE_BPS,
                monthly_bps: COMMUNITY_MONTHLY_BPS,
                cliff_months: COMMUNITY_CLIFF_MONTHS,
                label: String::from("community"),
            },
        ),
        (
            "Treasury (a 9-month cliff)",
            ActivitySpec {
                reference_amount: V3_REFERENCE,
                tge_bps: TREASURY_TGE_BPS,
                monthly_bps: TREASURY_MONTHLY_BPS,
                cliff_months: TREASURY_CLIFF_MONTHS,
                label: String::from("treasury"),
            },
        ),
        (
            "Liquidity",
            ActivitySpec {
                reference_amount: LIQUIDITY_REFERENCE,
                tge_bps: LIQUIDITY_TGE_BPS,
                monthly_bps: LIQUIDITY_MONTHLY_BPS,
                cliff_months: LIQUIDITY_CLIFF_MONTHS,
                label: String::from("liquidity"),
            },
        ),
    ];

    for (index, (section, spec)) in sections.into_iter().enumerate() {
        env.expire_blockhash();
        let id = u64::try_from(index).expect("the bucket's index in u64");
        let r = create(&mut env, recipient, &spec);
        assert!(
            r.is_ok(),
            "the bucket \\\"{section}\\\" of Arch §10.5 must be accepted: {r:?}"
        );
        let v = env.vesting(id);
        assert_eq!(v.tge_bps, spec.tge_bps, "{section}: tge_bps was written");
        assert_eq!(
            v.monthly_bps, spec.monthly_bps,
            "{section}: monthly_bps was written"
        );
        assert_eq!(
            v.cliff_months, spec.cliff_months,
            "{section}: cliff_months was written"
        );
        assert_eq!(
            v.reference_amount, spec.reference_amount,
            "{section}: the amount was written"
        );
        assert!(env.vault_exists(id), "{section}: the vault was created");
    }

    assert_eq!(
        env.vesting_config().activity_count,
        5,
        "all five buckets were created - none was rejected by the validation"
    );
}

// ===================================================================
// the label's length is counted in bytes, not characters
// ===================================================================

#[test]
fn create_vesting_counts_label_length_in_bytes() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();

    // Pinning the labels' own properties: both are multi-byte UTF-8, and by CHARACTERS
    // each fits into 32. So a `chars().count()` check would have let the 33-byte one
    // through - and that is exactly the mutation the negative below catches.
    assert_eq!(LABEL_33_BYTES.len(), 33, "the negative label: 33 bytes");
    assert_eq!(
        LABEL_33_BYTES.chars().count(),
        17,
        "and 17 characters: a 32-character limit would have accepted it"
    );
    assert_eq!(
        LABEL_32_BYTES.len(),
        MAX_LABEL_BYTES,
        "the positive label: exactly 32 bytes"
    );
    assert_eq!(
        LABEL_32_BYTES.chars().count(),
        16,
        "and 16 two-byte characters"
    );

    let base = ActivitySpec::valid(V2_REFERENCE);
    let too_long = ActivitySpec {
        label: String::from(LABEL_33_BYTES),
        ..base.clone()
    };
    let r = create(&mut env, recipient, &too_long);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidParams)),
        "a 33-byte label → InvalidParams (the limit holds the size of the event and the transaction): {r:?}"
    );
    assert_activity_not_created(&env, 0, 0, "a 33-byte label");

    // A POSITIVE exactly on the boundary: 32 bytes of multi-byte UTF-8.
    env.expire_blockhash();
    let at_ceiling = ActivitySpec {
        label: String::from(LABEL_32_BYTES),
        ..base
    };
    let r = create(&mut env, recipient, &at_ceiling);
    r.as_ref()
        .expect("32 bytes is an inclusive boundary and multi-byte text is not cut");
    let ev: VestingCreated = decode_event(&r).expect("VestingCreated");
    assert_eq!(
        ev.label, LABEL_32_BYTES,
        "the label reached the event in full, without truncation by characters"
    );
}

// ===================================================================
// the recipient slot: the type is itself the check
// ===================================================================

#[test]
fn create_vesting_rejects_non_system_recipient() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let spec = ActivitySpec::valid(V2_REFERENCE);

    let expected = u32::from(ErrorCode::AccountNotSystemOwned);
    assert_eq!(
        expected, 3011,
        "the nominal anchor code AccountNotSystemOwned (Anchor 1.0.2)"
    );

    // A token account in the recipient slot is the other half of the class: a recipient
    // not owned by the System Program would make the claim structurally impossible (the
    // `recipient` slot in claim is of the same type), and after TGE the activity would
    // also become uncancellable - a vault carrying an eternal obligation (Arch §9.5).
    let beneficiary = env.actors.beneficiary.pubkey();
    let mint = env.win_mint;
    let token_account = env.create_ata(&beneficiary, &mint);

    let substitutions: [(&str, Pubkey); 2] = [
        // Pubkey::default() is the System Program's own address; its account belongs to the
        // loader rather than to the System Program, so the slot's type rejects it before the
        // body.
        (
            "Pubkey::default() (the System Program's address)",
            Pubkey::default(),
        ),
        ("a token account", token_account),
    ];
    for (case, recipient) in substitutions {
        env.expire_blockhash();
        let r = create(&mut env, recipient, &spec);
        assert_eq!(
            custom_error_code(&r),
            Some(expected),
            "{case} in the recipient slot → AccountNotSystemOwned during context resolution, before the body: {r:?}"
        );
        assert_activity_not_created(&env, 0, 0, case);
    }

    // A POSITIVE: an ordinary wallet passes in the same config.
    env.expire_blockhash();
    let recipient = env.actors.recipients[0].pubkey();
    create(&mut env, recipient, &spec).expect("a system owner in the recipient slot passes");
    assert_eq!(
        env.vesting(0).recipient,
        recipient,
        "the recipient was written from the slot"
    );
}

#[test]
fn create_vesting_rejects_off_curve_recipient() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let spec = ActivitySpec::valid(V2_REFERENCE);

    // A data-less program-derived address belongs to the System Program, so the slot's
    // type lets it through; the body's curve check is what rejects it. Were any of these
    // to pass, claim would create a token account for an address no key can sign for
    // and the payout would be lost there - whichever program the address is derived
    // under. The activity PDAs are taken one id ahead: at the current id both `init`s
    // resolve before the `recipient` slot, and the type would answer first.
    let next_id = env.next_activity_id();
    let future_id = next_id + 1;
    let substitutions: [(&str, Pubkey); 4] = [
        (
            "a foreign program's PDA",
            foreign_program_pda(b"multisig-vault"),
        ),
        (
            "this program's future activity record",
            vesting_pda(future_id),
        ),
        ("this program's future activity vault", vault_pda(future_id)),
        ("the event authority", event_authority_pda()),
    ];
    for (case, recipient) in substitutions {
        assert!(
            !recipient.is_on_curve(),
            "{case}: the fixture must be off-curve - otherwise the case checks the wrong thing"
        );
        env.expire_blockhash();
        let r = create(&mut env, recipient, &spec);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::PayeeNotOnCurve)),
            "{case} in the recipient slot → PayeeNotOnCurve: {r:?}"
        );
        assert_activity_not_created(&env, next_id, next_id, case);
    }

    // A POSITIVE in the same config: an ordinary wallet passes.
    env.expire_blockhash();
    let recipient = env.actors.recipients[0].pubkey();
    create(&mut env, recipient, &spec).expect("an on-curve wallet in the recipient slot passes");
    assert_eq!(
        env.vesting(next_id).recipient,
        recipient,
        "the activity was created only with a permitted recipient"
    );
}

#[test]
fn create_vesting_rejects_vault_authority_as_recipient() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let spec = ActivitySpec::valid(V2_REFERENCE);

    // `vault_authority` is a data-less PDA (owner = the System Program), so the
    // `SystemAccount` type lets it through, and the body's named gate rejects it ahead
    // of the curve check: the vaults' owner is off-curve too, but it keeps its own
    // code. Were the substitution to pass, claim would pay out to `vault_authority`'s
    // own ATA - that is, back into the program's custody, from which nothing can
    // withdraw the funds.
    let r = create(&mut env, vault_authority_pda(), &spec);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidAuthority)),
        "recipient == vault_authority → InvalidAuthority: the bucket would fall into the program's custody forever: {r:?}"
    );
    assert_activity_not_created(&env, 0, 0, "recipient == vault_authority");

    // A POSITIVE in the same config: an ordinary wallet passes.
    env.expire_blockhash();
    let recipient = env.actors.recipients[0].pubkey();
    create(&mut env, recipient, &spec).expect("an ordinary wallet in the recipient slot passes");
    assert_eq!(
        env.vesting(0).recipient,
        recipient,
        "the activity was created only with a permitted recipient"
    );
}

#[test]
fn create_vesting_rejects_non_canonical_vault_authority_slot() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let owner = env.actors.owner.insecure_clone();
    let recipient = env.actors.recipients[0].pubkey();
    let spec = ActivitySpec::valid(V2_REFERENCE);
    let mint = env.win_mint;

    // The `vault_authority` slot is bound by the seeds `[b"vault_authority"]` plus the
    // bump from the config: pass an arbitrary address here and the "recipient !=
    // vault_authority" gate would be checked against a decoy key (that is, disabled by the
    // submission), while the vault would come to sit under a decoy authority.
    let next_id = env.next_activity_id();
    let ix = create_vesting_ix_with_slots(
        owner.pubkey(),
        config_pda(),
        vesting_pda(next_id),
        vault_pda(next_id),
        recipient,
        Pubkey::new_unique(),
        mint,
        &spec,
    );
    let r = env.send_ixs(&[ix], &owner.pubkey(), &[&owner]);
    // The code is pinned by the first execution: ConstraintSeeds (2006) - Anchor derives
    // the slot's address from the seeds and the decoy does not match what was derived.
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a non-canonical address in the vault_authority slot → ConstraintSeeds: {r:?}"
    );
    assert_activity_not_created(&env, next_id, 0, "a decoy vault_authority");

    // A POSITIVE: the canonical slot passes with the same call.
    env.expire_blockhash();
    create(&mut env, recipient, &spec).expect("the canonical vault_authority is accepted");
    let (_, authority) = env.token_account_mint_owner(&vault_pda(next_id));
    assert_eq!(
        authority,
        vault_authority_pda(),
        "the vault was created under the canonical PDA owner"
    );
}

// ===================================================================
// exhausting the id allocator
// ===================================================================

#[test]
fn create_vesting_rejects_activity_count_at_u64_max() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let spec = ActivitySpec::valid(V2_REFERENCE);

    // A POSITIVE before the forge: the same instruction passes in the same config, so
    // the rejection below is caused precisely by the counter's value.
    create(&mut env, recipient, &spec).expect("an ordinary activity is created");
    let created = env.vesting(0);

    // A forge: the counter grows only on a successful create_vesting, by one per
    // call, and never rolls back - driving it to u64::MAX honestly would mean executing
    // 2⁶⁴ transactions.
    env.forge_activity_count(u64::MAX);
    env.expire_blockhash();
    let r = create(&mut env, recipient, &spec);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::MathOverflow)),
        "activity_count == u64::MAX → MathOverflow on the increment rather than a silent wrap to zero (or id 0 would be issued twice): {r:?}"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        u64::MAX,
        "the rejection did not change the counter"
    );
    assert!(
        !env.vesting_exists(u64::MAX),
        "no record was created at the overflowed id's address"
    );
    assert!(
        !env.vault_exists(u64::MAX),
        "no vault was created at the overflowed id's address"
    );
    assert_eq!(
        env.vesting(0).recipient,
        created.recipient,
        "the existing activity is untouched by the rejection"
    );
}

// ===================================================================
// two activities in one transaction
// ===================================================================

#[test]
fn two_create_vesting_in_one_transaction_take_consecutive_ids() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let owner = env.actors.owner.insecure_clone();
    let first_recipient = env.actors.recipients[0].pubkey();
    let second_recipient = env.actors.recipients[1].pubkey();
    let mint = env.win_mint;
    let spec = ActivitySpec::valid(V2_REFERENCE);

    // The first activity is created in its own transaction: the pair below then takes ids
    // 1 and 2, and "one" cannot be confused with "the first id of all".
    create(&mut env, first_recipient, &spec).expect("activity 0 is created");
    let n = env.next_activity_id();
    assert_eq!(n, 1, "the next free id is 1");

    // The addresses of BOTH activities are derived in advance: the second instruction
    // needs id n + 1, which the config does not hold yet when the transaction is
    // assembled. Were Anchor not to write the config back between the instructions, the
    // second would fail with ConstraintSeeds - and that is exactly what the transaction
    // checks.
    env.expire_blockhash();
    let ixs = [
        create_vesting_ix(owner.pubkey(), first_recipient, n, mint, &spec),
        create_vesting_ix(owner.pubkey(), second_recipient, n + 1, mint, &spec),
    ];
    let r = env.send_ixs(&ixs, &owner.pubkey(), &[&owner]);
    r.as_ref()
        .expect("two create_vesting calls in one transaction pass");

    assert_eq!(
        env.vesting_config().activity_count,
        n + 2,
        "the counter grew by two activities in a single transaction"
    );
    assert_eq!(
        env.vesting(n).recipient,
        first_recipient,
        "the transaction's first instruction took id n"
    );
    assert_eq!(
        env.vesting(n + 1).recipient,
        second_recipient,
        "the second took id n + 1 - it already sees the counter incremented"
    );
    assert!(
        env.vault_exists(n) && env.vault_exists(n + 1),
        "both activities have their own vault"
    );

    // Only the first event is decoded (`decode_event` returns the first payload); the
    // second activity's id is proven by its PDA's address - stronger proof than the
    // event's field, since the address is derived by the program rather than by the
    // test.
    let ev: VestingCreated = decode_event(&r).expect("the first instruction's VestingCreated");
    assert_eq!(
        ev.activity_id, n,
        "the first instruction's event carries id n"
    );
}

// ===================================================================
// pre-funding lamports onto a future account's address
// ===================================================================

#[test]
fn create_vesting_survives_prefunded_addresses() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let spec = ActivitySpec::valid(V2_REFERENCE);

    // Rent griefing: the addresses of both of an activity's accounts are predictable (the
    // seeds are public), and an outsider sends lamports to them in advance. Were `init` to
    // go only through `create_account`, the instruction would fail with "account already
    // in use" and creating a bucket would become impossible. The pre-funding is BELOW the
    // rent - Anchor must take the transfer-shortfall path.
    let next_id = env.next_activity_id();
    let griefed_lamports = 1_000;
    env.prefund_lamports(&vesting_pda(next_id), griefed_lamports);
    env.prefund_lamports(&vault_pda(next_id), griefed_lamports);

    create(&mut env, recipient, &spec).expect("pre-funding lamports does not break create_vesting");

    assert_eq!(
        env.vesting(next_id).recipient,
        recipient,
        "the record was created over the pre-funded address"
    );
    assert!(env.vault_exists(next_id), "the vault was created");
    assert_eq!(env.vault_balance(next_id), 0, "the vault is empty");
    let (mint, authority) = env.token_account_mint_owner(&vault_pda(next_id));
    assert_eq!(
        mint, env.win_mint,
        "the vault is bound to the config's $1WIN"
    );
    assert_eq!(
        authority,
        vault_authority_pda(),
        "the vault's owner is the program's PDA"
    );

    // The owners differ by construction: the record is an account of the Vesting program
    // while the vault is an SPL token account under the Token Program (its authority being
    // the program's PDA, as verified above). They have one thing in common: after the
    // pre-funding neither must remain with the System Program.
    for (label, address, expected_owner) in [
        ("the record", vesting_pda(next_id), vesting::id()),
        ("the vault", vault_pda(next_id), spl_token_interface::id()),
    ] {
        let acc = env
            .svm
            .get_account(&address)
            .unwrap_or_else(|| panic!("{label} exists"));
        assert_eq!(
            acc.owner, expected_owner,
            "{label}: the account was reassigned to its own program - the pre-funding did not leave it with the System Program"
        );
        assert_eq!(
            acc.lamports,
            env.svm.minimum_balance_for_rent_exemption(acc.data.len()),
            "{label}: Anchor topped up the shortfall to rent exemption over the pre-funded {griefed_lamports} lamports"
        );
    }
}

// ===================================================================
// the context's bindings: the mint, time, the signer, the config
// ===================================================================

#[test]
fn create_vesting_rejects_foreign_token_mint() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let spec = ActivitySpec::valid(V2_REFERENCE);
    let owner = env.actors.owner.insecure_clone();

    // A foreign mint with THE SAME 8 decimals: the rejection must come from the binding
    // to the config's address rather than from a decimals mismatch. Otherwise a bucket's
    // vault would hold a foreign token and `assert_funded` (I10) would count it as
    // funding.
    let foreign = env.foreign_mint;
    assert_ne!(
        foreign,
        env.vesting_config().token_mint,
        "the fixture: the mint really is a foreign one"
    );
    let next_id = env.next_activity_id();
    let ix = create_vesting_ix(owner.pubkey(), recipient, next_id, foreign, &spec);
    let r = env.send_ixs(&[ix], &owner.pubkey(), &[&owner]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintAddress)),
        "a foreign token_mint → ConstraintAddress: the mint's address is set by the `address = config.token_mint` attribute and there is no check in the body: {r:?}"
    );
    assert_activity_not_created(&env, next_id, 0, "a foreign token_mint");

    // A POSITIVE: the same call with the config's mint passes.
    env.expire_blockhash();
    create(&mut env, recipient, &spec).expect("the mint from the config is accepted");
    let (mint, _) = env.token_account_mint_owner(&vault_pda(0));
    assert_eq!(
        mint, env.win_mint,
        "the vault was created under the config's mint"
    );
}

#[test]
fn create_vesting_rejects_at_and_after_tge() {
    let mut env = TestEnv::new();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let spec = ActivitySpec::valid(V2_REFERENCE);

    // A POSITIVE one second before TGE: the editing window is still open.
    env.warp_to(p.tge_ts - 1);
    create(&mut env, recipient, &spec).expect("before TGE activities are created");
    assert_eq!(
        env.vesting_config().activity_count,
        1,
        "the activity was created"
    );

    // The gate is inclusive: at the TGE instant ITSELF the set of activities is already
    // final - otherwise a bucket added after the schedule started would dilute the
    // emission the funded vaults are already counted against (Arch §4.2).
    env.warp_to(p.tge_ts);
    env.expire_blockhash();
    let r = create(&mut env, recipient, &spec);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::ScheduleStarted)),
        "now == tge_ts → ScheduleStarted: {r:?}"
    );
    assert_activity_not_created(&env, 1, 1, "now == tge_ts");

    env.warp_to(p.tge_ts + 1);
    env.expire_blockhash();
    let r = create(&mut env, recipient, &spec);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::ScheduleStarted)),
        "now == tge_ts + 1 → ScheduleStarted: {r:?}"
    );
    assert_activity_not_created(&env, 1, 1, "now == tge_ts + 1");
}

#[test]
fn create_vesting_rejects_wrong_signers() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let owner = env.actors.owner.insecure_clone();
    let recipient = env.actors.recipients[0].pubkey();
    let spec = ActivitySpec::valid(V2_REFERENCE);

    // The closure puts the foreign key BOTH into the owner slot AND into the signers:
    // pass it only into the slot and the failure would come from the missing signature,
    // leaving the role guard unchecked. Every call is valid in all other respects,
    // so the rejection is a role one.
    for (label, actor) in wrong_owners(&env) {
        env.expire_blockhash();
        let r = create_as(&mut env, &actor, recipient, &spec);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::NotOwner)),
            "{label} signed create_vesting → NotOwner was expected: creating a bucket is Owner's exclusive right: {r:?}"
        );
        assert_activity_not_created(&env, 0, 0, label);
    }

    // A POSITIVE: the correct owner passes with the same call.
    env.expire_blockhash();
    create_as(&mut env, &owner, recipient, &spec).expect("the correct owner passes");
    assert_eq!(
        env.vesting_config().activity_count,
        1,
        "only Owner created the activity"
    );
}

#[test]
fn create_vesting_rejects_forged_config() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let owner = env.actors.owner.insecure_clone();
    let recipient = env.actors.recipients[0].pubkey();
    let spec = ActivitySpec::valid(V2_REFERENCE);
    let mint = env.win_mint;

    // A forge: a program-owned account with a genuine discriminator and a
    // copy of a live config's fields, but at a random address. Unreachable honestly:
    // `initialize` only ever creates the config at the PDA of the seeds `[b"config"]`.
    // Were the substitution to pass, the decoy config's Owner (which could be anyone)
    // would be creating activities in the real program, and the vaults would sit under
    // that same `vault_authority`.
    let forged = Pubkey::new_unique();
    let snapshot = env.vesting_config();
    env.forge_config(&forged, &snapshot);
    assert_ne!(
        forged,
        config_pda(),
        "the forge sits outside the canonical address"
    );

    // The created accounts' addresses are derived from the decoy config's counter - that
    // is, exactly as the program would derive them had it accepted the substitution.
    let next_id = snapshot.activity_count;
    let ix = create_vesting_ix_with_slots(
        owner.pubkey(),
        forged,
        vesting_pda(next_id),
        vault_pda(next_id),
        recipient,
        vault_authority_pda(),
        mint,
        &spec,
    );
    let r = env.send_ixs(&[ix], &owner.pubkey(), &[&owner]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a config outside the canonical PDA → ConstraintSeeds: Anchor derives the address from the seeds `[b\\\"config\\\"]` with the canonical bump: {r:?}"
    );
    assert_activity_not_created(&env, next_id, snapshot.activity_count, "a decoy config");

    // A POSITIVE: the same call on the canonical config passes.
    env.expire_blockhash();
    create(&mut env, recipient, &spec).expect("the canonical config accepts create_vesting");
    assert_eq!(
        env.vesting_config().activity_count,
        next_id + 1,
        "only the canonical config moved the counter"
    );
}

// ===================================================================
// E [#6, decision 4] - cliff_months == 1 is rejected on the real instruction
//
// The `math::validate_schedule` unit test already pins this (decision 4,
// `cliff_one_rejected_zero_and_two_accepted`); here it is bound to the REAL
// `create_vesting` instruction (tests-bind-to-real-code): with `monthly_bps > 0` the
// value `cliff_months == 1` is identical to zero through `.max(1)` in `vested()`, and
// meaningful input must not be silently ignored (the same semantics as for
// `monthly_bps == 0`). The valid cliffs are 0 or >= 2.
// ===================================================================

#[test]
fn create_vesting_rejects_cliff_of_one_month() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();

    // The "Ecosystem" schedule (monthly_bps > 0) with the cliff edited: 1 is
    // rejected while 0 and 2 are accepted. The formula is not duplicated - what is
    // asserted is the error code and the fact of creation.
    let base = ActivitySpec::valid(V2_REFERENCE);
    assert!(
        base.monthly_bps > 0,
        "precondition: there are monthly tranches"
    );

    let cliff_one = ActivitySpec {
        cliff_months: 1,
        ..base.clone()
    };
    let r = create(&mut env, recipient, &cliff_one);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidSchedule)),
        "cliff_months == 1 with monthly_bps > 0 → InvalidSchedule (ambiguous input, identical to zero): {r:?}"
    );
    assert_activity_not_created(&env, 0, 0, "cliff_months == 1");

    // A POSITIVE: cliff == 2 is accepted and creates activity id 0.
    env.expire_blockhash();
    let cliff_two = ActivitySpec {
        cliff_months: 2,
        ..base.clone()
    };
    create(&mut env, recipient, &cliff_two).expect("cliff_months == 2 is a valid cliff");
    assert_eq!(
        env.vesting(0).cliff_months,
        2,
        "a cliff of 2 was written into activity 0"
    );

    // The second POSITIVE: cliff == 0 is accepted too (pinning decision 4 - 0 and 2 are
    // valid).
    env.expire_blockhash();
    let cliff_zero = ActivitySpec {
        cliff_months: 0,
        ..base
    };
    create(&mut env, recipient, &cliff_zero).expect("cliff_months == 0 - no cliff");
    assert_eq!(
        env.vesting(1).cliff_months,
        0,
        "a cliff of 0 was written into activity 1"
    );
}
