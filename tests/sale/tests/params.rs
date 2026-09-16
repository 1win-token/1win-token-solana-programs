//! The parameter setters (Arch §9.2) - the detailed pass: every setter gets a
//! positive case in PreSale (the stored field changes) plus negatives on range and
//! schedule validation with specific codes. tests-bind-to-real-code.
//!
//! The phase and role gates of these setters are covered in phase_matrix.rs /
//! role_matrix.rs; here it is value validation inside the permitted phase (PreSale).
//! The `set_claim_schedule` pass also runs in SaleEnded, where both bounds on
//! `claim_start_date` follow `now` rather than the sale's end.

use anchor_lang::error::ERROR_CODE_OFFSET;
use sale::errors::SaleError;
use sale::state::{
    MAX_CLAIM_DURATION, MAX_CLAIM_START_DELAY, MAX_SALE_DURATION, MAX_SALE_START_DELAY,
    MIN_CLAIM_DURATION,
};
use sale_test_common::{
    custom_error_code, set_claim_schedule_ix, set_emission_ix, set_price_ix, set_sale_schedule_ix,
    set_tx_limits_ix, InitParams, TestEnv,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

const TOKEN_UNITS: u64 = 100_000_000;

/// A ready environment in PreSale plus the parameters used and the admin keypair.
fn setup() -> (TestEnv, InitParams, Keypair) {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();
    (env, p, admin)
}

// We return the native litesvm TransactionResult as is (as `send_ixs` does).
#[allow(clippy::result_large_err)]
fn send(
    env: &mut TestEnv,
    ix: solana_instruction::Instruction,
    admin: &Keypair,
) -> litesvm::types::TransactionResult {
    env.send_ixs(&[ix], &admin.pubkey(), &[admin])
}

// ===================================================================
// set_price
// ===================================================================

#[test]
fn set_price_positive_and_negatives() {
    let (mut env, _p, admin) = setup();

    // POSITIVE: a new valid value is written.
    send(&mut env, set_price_ix(admin.pubkey(), 55_000), &admin).expect("set_price is valid");
    assert_eq!(env.sale_state().price_usdc, 55_000, "the price was written");

    // NEGATIVE: 0.
    env.expire_blockhash();
    let r = send(&mut env, set_price_ix(admin.pubkey(), 0), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "price 0: {r:?}"
    );

    // NEGATIVE: == TOKEN_UNITS (the lower edge of the invalid range, >= TOKEN_UNITS).
    env.expire_blockhash();
    let r = send(&mut env, set_price_ix(admin.pubkey(), TOKEN_UNITS), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "price >= TOKEN_UNITS: {r:?}"
    );

    // The value was not corrupted by the negatives.
    assert_eq!(
        env.sale_state().price_usdc,
        55_000,
        "the price is the one left by the positive case"
    );
}

// ===================================================================
// set_emission
// ===================================================================

#[test]
fn set_emission_positive_and_zero_negative() {
    let (mut env, _p, admin) = setup();

    send(&mut env, set_emission_ix(admin.pubkey(), 7_777), &admin).expect("set_emission is valid");
    assert_eq!(env.sale_state().emission, 7_777, "the emission was written");

    env.expire_blockhash();
    let r = send(&mut env, set_emission_ix(admin.pubkey(), 0), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "emission 0: {r:?}"
    );
    assert_eq!(
        env.sale_state().emission,
        7_777,
        "the emission is not corrupted"
    );
}

// ===================================================================
// set_tx_limits
// ===================================================================

#[test]
fn set_tx_limits_positive_and_negatives() {
    let (mut env, _p, admin) = setup();

    // POSITIVE: both fields are updated.
    send(&mut env, set_tx_limits_ix(admin.pubkey(), 100, 200), &admin).expect("tx_limits is valid");
    let s = env.sale_state();
    assert_eq!(s.min_per_tx, 100, "min was updated");
    assert_eq!(s.max_per_tx, 200, "max was updated");

    // NEGATIVE: min == 0.
    env.expire_blockhash();
    let r = send(&mut env, set_tx_limits_ix(admin.pubkey(), 0, 200), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "min 0: {r:?}"
    );

    // NEGATIVE: min > max.
    env.expire_blockhash();
    let r = send(&mut env, set_tx_limits_ix(admin.pubkey(), 300, 200), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "min > max: {r:?}"
    );

    // The fields were not corrupted by the negatives.
    let s = env.sale_state();
    assert_eq!(
        (s.min_per_tx, s.max_per_tx),
        (100, 200),
        "the limits are the ones left by the positive case"
    );
}

// ===================================================================
// set_sale_schedule (sale_start_date + sale_end_date in one instruction)
// ===================================================================

#[test]
fn set_sale_schedule_positive_and_negatives() {
    let (mut env, p, admin) = setup();
    // The defaults: start = now+1000, end = now+2000, claim_start_date = end+1000 = now+3000.

    // POSITIVE: both dates are valid (now < start < end < claim_start_date;
    // claim_start_date <= end+30d). Both are written.
    let start = env.now() + 1_200;
    let end = env.now() + 2_500;
    assert!(env.now() < start && start < end && end < p.claim_start_date);
    send(
        &mut env,
        set_sale_schedule_ix(admin.pubkey(), start, end),
        &admin,
    )
    .expect("set_sale_schedule is valid");
    let s = env.sale_state();
    assert_eq!(s.sale_start_date, start, "sale_start_date was written");
    assert_eq!(s.sale_end_date, end, "sale_end_date was written");

    // NEGATIVE: start <= now (== now) → InvalidDeadline.
    env.expire_blockhash();
    let now = env.now();
    let r = send(
        &mut env,
        set_sale_schedule_ix(admin.pubkey(), now, now + 2_000),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidDeadline)),
        "start == now: {r:?}"
    );

    // NEGATIVE: start >= end → InvalidDeadline.
    env.expire_blockhash();
    let s0 = env.now() + 2_000;
    let e0 = env.now() + 1_500;
    let r = send(
        &mut env,
        set_sale_schedule_ix(admin.pubkey(), s0, e0),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidDeadline)),
        "start >= end: {r:?}"
    );

    // NEGATIVE: end >= claim_start_date → ClaimStartInPast (a dedicated code - the same
    // single invariant "sale_end_date < claim_start_date" as in set_claim_schedule and
    // initialize).
    env.expire_blockhash();
    let claim_start_now = env.sale_state().claim_start_date;
    let start_ok = env.now() + 1_000;
    let r = send(
        &mut env,
        set_sale_schedule_ix(admin.pubkey(), start_ok, claim_start_now),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimStartInPast)),
        "end >= claim_start_date → ClaimStartInPast: {r:?}"
    );

    // The state was not corrupted by any of the negatives.
    let s = env.sale_state();
    assert_eq!(
        s.sale_start_date, start,
        "sale_start_date is the one left by the positive case"
    );
    assert_eq!(
        s.sale_end_date, end,
        "sale_end_date is the one left by the positive case"
    );
}

// The Sale schedule ceilings in set_sale_schedule: the same two bounds as in
// initialize. Both checks stand before the claim_start_date invariants, so the
// negatives are reachable directly - there is no need to move the claim schedule.

#[test]
fn set_sale_schedule_rejects_sale_start_date_too_far() {
    let (mut env, _p, admin) = setup();
    let before = env.sale_state();

    env.expire_blockhash();
    let start = env.now() + MAX_SALE_START_DELAY + 1; // one second past the ceiling
    let r = send(
        &mut env,
        set_sale_schedule_ix(admin.pubkey(), start, start + 1_000),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::StartDateTooFar)),
        "sale_start_date > now+90d → StartDateTooFar: {r:?}"
    );
    let after = env.sale_state();
    assert_eq!(
        after.sale_start_date, before.sale_start_date,
        "sale_start_date is untouched"
    );
    assert_eq!(
        after.sale_end_date, before.sale_end_date,
        "sale_end_date is untouched"
    );
}

#[test]
fn set_sale_schedule_rejects_sale_duration_too_long() {
    let (mut env, _p, admin) = setup();
    let before = env.sale_state();

    env.expire_blockhash();
    let start = env.now() + 1_000;
    let r = send(
        &mut env,
        set_sale_schedule_ix(admin.pubkey(), start, start + MAX_SALE_DURATION + 1),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::SaleDurationTooLong)),
        "end-start > 30d → SaleDurationTooLong: {r:?}"
    );
    let after = env.sale_state();
    assert_eq!(
        after.sale_start_date, before.sale_start_date,
        "sale_start_date is untouched"
    );
    assert_eq!(
        after.sale_end_date, before.sale_end_date,
        "sale_end_date is untouched"
    );
}

#[test]
fn set_sale_schedule_accepts_sale_duration_at_ceiling() {
    // A window exactly at the ceiling: first we move claim_start_date forward, otherwise
    // the new sale_end_date would hit the sale_end_date < claim_start_date invariant.
    let (mut env, p, admin) = setup();
    let far_claim_start = p.sale_end_date + MAX_CLAIM_START_DELAY;
    send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), far_claim_start, p.claim_duration),
        &admin,
    )
    .expect("set_claim_schedule moves claim_start_date to its maximum");

    env.expire_blockhash();
    let start = env.now() + 1_000;
    let end = start + MAX_SALE_DURATION;
    assert!(
        end < far_claim_start,
        "precondition: end < claim_start_date"
    );
    send(
        &mut env,
        set_sale_schedule_ix(admin.pubkey(), start, end),
        &admin,
    )
    .expect("a length exactly at the ceiling passes");
    let s = env.sale_state();
    assert_eq!(
        s.sale_end_date - s.sale_start_date,
        MAX_SALE_DURATION,
        "the window was written at its full permitted length"
    );
}

#[test]
fn set_sale_schedule_claim_start_date_too_far_boundary() {
    // ClaimStartTooFar in set_sale_schedule is reachable in two steps: first
    // set_claim_schedule moves claim_start_date to its maximum (end + 30d - a valid ≤
    // boundary), then set_sale_schedule shortens end - and claim_start_date ends up
    // beyond new_end + MAX_CLAIM_START_DELAY.
    let (mut env, p, admin) = setup();

    // Step 1: claim_start_date = end + MAX_CLAIM_START_DELAY (the ≤ boundary passes).
    let far_claim_start = p.sale_end_date + MAX_CLAIM_START_DELAY;
    send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), far_claim_start, p.claim_duration),
        &admin,
    )
    .expect("set_claim_schedule: claim_start_date == end + 30d passes");
    assert_eq!(env.sale_state().claim_start_date, far_claim_start);

    // POSITIVE boundary: end unchanged - claim_start_date == new_end + 30d (≤).
    env.expire_blockhash();
    let start_ok = env.now() + 1_200;
    send(
        &mut env,
        set_sale_schedule_ix(admin.pubkey(), start_ok, p.sale_end_date),
        &admin,
    )
    .expect("set_sale_schedule: claim_start_date == new_end + 30d - the boundary passes");

    // NEGATIVE: end one second earlier → claim_start_date > new_end + 30d.
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_sale_schedule_ix(admin.pubkey(), start_ok, p.sale_end_date - 1),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimStartTooFar)),
        "claim_start_date > new_end + 30d → ClaimStartTooFar: {r:?}"
    );

    // The state is the one left by the positive cases.
    let s = env.sale_state();
    assert_eq!(
        s.sale_start_date, start_ok,
        "sale_start_date is the one left by the positive case"
    );
    assert_eq!(
        s.sale_end_date, p.sale_end_date,
        "sale_end_date is the one left by the positive case"
    );
    assert_eq!(
        s.claim_start_date, far_claim_start,
        "claim_start_date is untouched"
    );
}

// ===================================================================
// set_claim_schedule
// ===================================================================

#[test]
fn set_claim_schedule_positive_and_negatives() {
    let (mut env, p, admin) = setup();

    // POSITIVE: a valid schedule - both fields are updated.
    let valid_start = p.sale_end_date + 5_000;
    let valid_dur = 200 * 24 * 60 * 60; // 200 days < MAX_CLAIM_DURATION (365)
    send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), valid_start, valid_dur),
        &admin,
    )
    .expect("set_claim_schedule is valid");
    let s = env.sale_state();
    assert_eq!(
        s.claim_start_date, valid_start,
        "claim_start_date was written"
    );
    assert_eq!(s.claim_duration, valid_dur, "claim_duration was written");

    // NEGATIVE: claim_start_date <= sale_end_date (== sale_end_date) → ClaimStartInPast.
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), p.sale_end_date, valid_dur),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimStartInPast)),
        "claim_start_date <= end → ClaimStartInPast: {r:?}"
    );

    // NEGATIVE: claim_start_date > sale_end_date + MAX_CLAIM_START_DELAY → ClaimStartTooFar.
    env.expire_blockhash();
    let too_far = p.sale_end_date + MAX_CLAIM_START_DELAY + 1;
    let r = send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), too_far, valid_dur),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimStartTooFar)),
        "claim_start_date > end+30d → ClaimStartTooFar: {r:?}"
    );

    // NEGATIVE: claim_duration < MIN_CLAIM_DURATION → ClaimDurationTooShort.
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), valid_start, MIN_CLAIM_DURATION - 1),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimDurationTooShort)),
        "claim_duration < MIN → ClaimDurationTooShort: {r:?}"
    );

    // NEGATIVE: claim_duration > MAX_CLAIM_DURATION → ClaimDurationTooLong.
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), valid_start, MAX_CLAIM_DURATION + 1),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimDurationTooLong)),
        "claim_duration > MAX → ClaimDurationTooLong: {r:?}"
    );

    // The fields were not corrupted by the negatives.
    let s = env.sale_state();
    assert_eq!(
        s.claim_start_date, valid_start,
        "claim_start_date is the one left by the positive case"
    );
    assert_eq!(
        s.claim_duration, valid_dur,
        "claim_duration is the one left by the positive case"
    );
}

#[test]
fn set_claim_schedule_accepts_duration_at_min() {
    let (mut env, p, admin) = setup();

    // POSITIVE: exactly the floor passes (the default is 90 days - in PreSale the
    // window may still be cut down to the shortest permitted length).
    send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), p.claim_start_date, MIN_CLAIM_DURATION),
        &admin,
    )
    .expect("a duration exactly at the floor passes");
    assert_eq!(
        env.sale_state().claim_duration,
        MIN_CLAIM_DURATION,
        "the window was written at its shortest permitted length"
    );

    // NEGATIVE: one second below it.
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), p.claim_start_date, MIN_CLAIM_DURATION - 1),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimDurationTooShort)),
        "one second below the floor → ClaimDurationTooShort: {r:?}"
    );
    assert_eq!(
        env.sale_state().claim_duration,
        MIN_CLAIM_DURATION,
        "claim_duration is the one left by the positive case"
    );
}

// ===================================================================
// set_claim_schedule in SaleEnded: both bounds follow now. After the sale's end the
// anchor is the moment of the edit, so a start behind now is rejected even though it
// is after sale_end_date, and the ceiling is now + 30d rather than sale_end_date + 30d
// - a sale left past the original ceiling can still be rescheduled.
// ===================================================================

#[test]
fn set_claim_schedule_in_sale_ended_rejects_start_not_ahead_of_now() {
    let (mut env, p, admin) = setup();
    env.warp_to(p.sale_end_date + 10);
    let before = env.sale_state();

    // NEGATIVE: after the sale's end, but already behind now.
    let r = send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), p.sale_end_date + 1, p.claim_duration),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimStartInPast)),
        "a start behind now → ClaimStartInPast: {r:?}"
    );

    // NEGATIVE: exactly now (the bound is strict).
    env.expire_blockhash();
    let now = env.now();
    let r = send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), now, p.claim_duration),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimStartInPast)),
        "a start == now → ClaimStartInPast: {r:?}"
    );
    assert_eq!(
        env.sale_state().claim_start_date,
        before.claim_start_date,
        "claim_start_date is untouched by the negatives"
    );

    // POSITIVE: one second ahead of now is enough.
    env.expire_blockhash();
    send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), now + 1, p.claim_duration),
        &admin,
    )
    .expect("a start one second ahead of now passes");
    assert_eq!(
        env.sale_state().claim_start_date,
        now + 1,
        "claim_start_date was written"
    );
}

#[test]
fn set_claim_schedule_in_sale_ended_ceiling_follows_now() {
    let (mut env, p, admin) = setup();
    // Past the original ceiling: every start the old rule allowed is behind now.
    env.warp_to(p.sale_end_date + MAX_CLAIM_START_DELAY + 1);
    let now = env.now();

    // NEGATIVE: the old ceiling itself is in the past.
    let r = send(
        &mut env,
        set_claim_schedule_ix(
            admin.pubkey(),
            p.sale_end_date + MAX_CLAIM_START_DELAY,
            p.claim_duration,
        ),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimStartInPast)),
        "sale_end_date + 30d behind now → ClaimStartInPast: {r:?}"
    );

    // POSITIVE: the ceiling is measured from now.
    env.expire_blockhash();
    let at_ceiling = now + MAX_CLAIM_START_DELAY;
    send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), at_ceiling, p.claim_duration),
        &admin,
    )
    .expect("now + 30d passes");
    assert_eq!(
        env.sale_state().claim_start_date,
        at_ceiling,
        "claim_start_date was written at the ceiling"
    );

    // NEGATIVE: one second past it.
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), at_ceiling + 1, p.claim_duration),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::ClaimStartTooFar)),
        "now + 30d + 1 → ClaimStartTooFar: {r:?}"
    );
    assert_eq!(
        env.sale_state().claim_start_date,
        at_ceiling,
        "claim_start_date is the one left by the positive case"
    );
}
