//! The phase matrix on SVM (Arch §4) - every phase-gated instruction is checked in
//! its permitted and forbidden phases through a REAL call plus a specific code
//! (tests-bind-to-real-code). The transition boundaries `t−1 / t / t+1` are executed
//! through `warp_to`. The post-fixation phases are reached by a real
//! `activate_claim` (on an empty pool - undersubscribed with no deposits).

use anchor_lang::error::ERROR_CODE_OFFSET;
use sale::errors::SaleError;
use sale_test_common::{
    activate_claim_ix, custom_error_code, pause_ix, set_claim_schedule_ix, set_emission_ix,
    set_price_ix, set_sale_schedule_ix, set_tx_limits_ix, unpause_ix, TestEnv,
};
use solana_keypair::Keypair;
use solana_signer::Signer;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

/// Sends a single instruction signed by `admin`.
/// We return the native litesvm TransactionResult as is (as `send_ixs` does).
#[allow(clippy::result_large_err)]
fn send(
    env: &mut TestEnv,
    ix: solana_instruction::Instruction,
    admin: &Keypair,
) -> litesvm::types::TransactionResult {
    env.send_ixs(&[ix], &admin.pubkey(), &[admin])
}

// ===================================================================
// The parameter setters: permitted only in PreSale; in Sale and SaleEnded →
// SaleAlreadyStarted. (set_price is taken as representative, plus one run of each
// remaining setter on the start boundary.)
// ===================================================================

#[test]
fn set_price_allowed_in_presale_denied_after_start() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    // PreSale: it passes and the price is written.
    send(&mut env, set_price_ix(admin.pubkey(), 40_000), &admin).expect("set_price in PreSale");
    assert_eq!(
        env.sale_state().price_usdc,
        40_000,
        "the price was written in PreSale"
    );

    // Sale (now == sale_start_date): SaleAlreadyStarted.
    env.warp_to(p.sale_start_date);
    env.expire_blockhash();
    let r = send(&mut env, set_price_ix(admin.pubkey(), 50_000), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::SaleAlreadyStarted)),
        "set_price in Sale → SaleAlreadyStarted: {r:?}"
    );
    assert_eq!(
        env.sale_state().price_usdc,
        40_000,
        "the price did not change in Sale"
    );

    // SaleEnded (now == sale_end_date): also SaleAlreadyStarted.
    env.warp_to(p.sale_end_date);
    env.expire_blockhash();
    let r = send(&mut env, set_price_ix(admin.pubkey(), 60_000), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::SaleAlreadyStarted)),
        "set_price in SaleEnded → SaleAlreadyStarted: {r:?}"
    );
    assert_eq!(
        env.sale_state().price_usdc,
        40_000,
        "the price did not change in SaleEnded"
    );
}

#[test]
fn all_economic_setters_denied_in_sale() {
    // One run of each economic setter on the Sale start boundary.
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();
    env.warp_to(p.sale_start_date); // exactly the start of Sale

    for (label, ix) in [
        ("set_emission", set_emission_ix(admin.pubkey(), 1_000)),
        ("set_tx_limits", set_tx_limits_ix(admin.pubkey(), 1, 2)),
        (
            "set_sale_schedule",
            set_sale_schedule_ix(admin.pubkey(), p.sale_start_date + 10, p.sale_end_date + 10),
        ),
    ] {
        env.expire_blockhash();
        let r = send(&mut env, ix, &admin);
        assert_eq!(
            custom_error_code(&r),
            Some(err(SaleError::SaleAlreadyStarted)),
            "{label} in Sale → SaleAlreadyStarted: {r:?}"
        );
    }
}

// ===================================================================
// The sale_start_date boundaries for the parameter setters: t−1 is OK, t →
// SaleAlreadyStarted.
// ===================================================================

#[test]
fn set_price_boundary_at_start_minus_one_ok_at_start_denied() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    // t = sale_start_date − 1: still PreSale → it passes.
    env.warp_to(p.sale_start_date - 1);
    send(&mut env, set_price_ix(admin.pubkey(), 31_000), &admin)
        .expect("set_price at start−1 (PreSale)");
    assert_eq!(env.sale_state().price_usdc, 31_000);

    // t = sale_start_date: already Sale → SaleAlreadyStarted.
    env.warp_to(p.sale_start_date);
    env.expire_blockhash();
    let r = send(&mut env, set_price_ix(admin.pubkey(), 32_000), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::SaleAlreadyStarted)),
        "set_price exactly at start → SaleAlreadyStarted: {r:?}"
    );
    assert_eq!(
        env.sale_state().price_usdc,
        31_000,
        "it did not change on the boundary"
    );
}

// ===================================================================
// set_claim_schedule: its editing window is wider than the other setters' and is
// named by phase explicitly - PreSale, Sale and SaleEnded (plus !pool_fixed). A
// positive case for each of the three phases; the negatives are Claim (the phase)
// and pool fixation (the flag).
// ===================================================================

#[test]
fn set_claim_schedule_allowed_in_presale() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    let new_start = p.sale_end_date + 2_000;
    let new_dur = 100 * 24 * 60 * 60;
    send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), new_start, new_dur),
        &admin,
    )
    .expect("set_claim_schedule in PreSale");
    let s = env.sale_state();
    assert_eq!(
        s.claim_start_date, new_start,
        "claim_start_date was updated in PreSale"
    );
    assert_eq!(
        s.claim_duration, new_dur,
        "claim_duration was updated in PreSale"
    );
}

#[test]
fn set_claim_schedule_allowed_in_sale() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    // Contributions are being accepted: the other setters are already closed here while
    // this one is open.
    env.warp_to(p.sale_start_date);

    let new_start = p.sale_end_date + 4_000;
    let new_dur = 110 * 24 * 60 * 60;
    env.expire_blockhash();
    send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), new_start, new_dur),
        &admin,
    )
    .expect("set_claim_schedule in Sale");
    let s = env.sale_state();
    assert_eq!(
        s.claim_start_date, new_start,
        "claim_start_date was updated in Sale"
    );
    assert_eq!(
        s.claim_duration, new_dur,
        "claim_duration was updated in Sale"
    );

    // The contrast: in that same phase an economic setter is already frozen.
    env.expire_blockhash();
    let r = send(&mut env, set_price_ix(admin.pubkey(), 40_000), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::SaleAlreadyStarted)),
        "set_price in Sale → SaleAlreadyStarted (the setters' windows differ): {r:?}"
    );
}

#[test]
fn set_claim_schedule_allowed_in_sale_ended_before_fixation() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    // SaleEnded before fixation: now ≥ sale_end_date, pool_fixed == false.
    env.warp_to(p.sale_end_date);
    assert!(
        !env.sale_state().pool_fixed,
        "the pool is not fixed (no activate_claim)"
    );

    let new_start = p.sale_end_date + 3_000;
    let new_dur = 120 * 24 * 60 * 60;
    send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), new_start, new_dur),
        &admin,
    )
    .expect("set_claim_schedule in SaleEnded-before-fixation");
    let s = env.sale_state();
    assert_eq!(
        s.claim_start_date, new_start,
        "claim_start_date was updated in SaleEnded"
    );
    assert_eq!(
        s.claim_duration, new_dur,
        "claim_duration was updated in SaleEnded"
    );
}

// ===================================================================
// set_claim_schedule after the pool is fixed (§9.2, I8): PoolAlreadyFixed. Fixation
// is done by a real activate_claim on an empty pool (undersubscribed: pool = 0,
// distributable = 0, refund_vault is not passed).
// ===================================================================

#[test]
fn set_claim_schedule_after_pool_fixed_rejected() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    env.warp_to(p.sale_end_date); // SaleEnded before fixation
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activate_claim on an empty pool passes");
    assert!(
        env.sale_state().pool_fixed,
        "precondition: the pool is fixed"
    );

    // The same SaleEnded phase, but after fixation → PoolAlreadyFixed (the positive
    // before fixation is set_claim_schedule_allowed_in_sale_ended_before_fixation).
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), p.sale_end_date + 3_000, 100 * 24 * 60 * 60),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::PoolAlreadyFixed)),
        "set_claim_schedule after fixation → PoolAlreadyFixed: {r:?}"
    );
    let s = env.sale_state();
    assert_eq!(
        s.claim_start_date, p.claim_start_date,
        "claim_start_date did not change"
    );
    assert_eq!(
        s.claim_duration, p.claim_duration,
        "claim_duration did not change"
    );
}

// ===================================================================
// set_claim_schedule outside the named phases: in Claim → WrongPhase. A different
// code from PoolAlreadyFixed: the Claim phase is cut off by the phase gate, while
// SaleEnded-after-fixation is cut off by the flag (the test above).
// ===================================================================

#[test]
fn set_claim_schedule_in_claim_phase_rejected_wrong_phase() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    env.warp_to(p.sale_end_date);
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    )
    .expect("activate_claim on an empty pool passes");
    env.warp_to(p.claim_start_date); // the Claim phase

    env.expire_blockhash();
    let r = send(
        &mut env,
        set_claim_schedule_ix(admin.pubkey(), p.sale_end_date + 3_000, 100 * 24 * 60 * 60),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::WrongPhase)),
        "set_claim_schedule in Claim → WrongPhase: {r:?}"
    );
    let s = env.sale_state();
    assert_eq!(
        s.claim_start_date, p.claim_start_date,
        "claim_start_date did not change"
    );
    assert_eq!(
        s.claim_duration, p.claim_duration,
        "claim_duration did not change"
    );
}

// ===================================================================
// pause is permitted only in Sale; in PreSale and SaleEnded → WrongPhase. The
// boundaries: pause at start−1 → WrongPhase, at start → OK, at end → WrongPhase.
// unpause is not phase-gated and passes in any of the five phases.
// ===================================================================

#[test]
fn pause_allowed_in_sale_only() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    // PreSale (now < start): WrongPhase.
    let r = send(&mut env, pause_ix(admin.pubkey()), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::WrongPhase)),
        "pause in PreSale → WrongPhase: {r:?}"
    );
    assert!(!env.sale_state().paused, "paused was not set in PreSale");

    // Sale (now == start): it passes.
    env.warp_to(p.sale_start_date);
    env.expire_blockhash();
    send(&mut env, pause_ix(admin.pubkey()), &admin).expect("pause in Sale");
    assert!(env.sale_state().paused, "paused was set in Sale");

    // unpause in Sale: it passes.
    env.expire_blockhash();
    send(&mut env, unpause_ix(admin.pubkey()), &admin).expect("unpause in Sale");
    assert!(!env.sale_state().paused, "paused was cleared in Sale");

    // SaleEnded (now == end): WrongPhase.
    env.warp_to(p.sale_end_date);
    env.expire_blockhash();
    let r = send(&mut env, pause_ix(admin.pubkey()), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::WrongPhase)),
        "pause in SaleEnded → WrongPhase: {r:?}"
    );
    assert!(!env.sale_state().paused, "paused was not set in SaleEnded");
}

#[test]
fn pause_boundary_start_minus_one_denied_start_ok_end_denied() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    // t = start − 1: still PreSale → WrongPhase.
    env.warp_to(p.sale_start_date - 1);
    let r = send(&mut env, pause_ix(admin.pubkey()), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::WrongPhase)),
        "pause at start−1 (PreSale) → WrongPhase: {r:?}"
    );

    // t = start: Sale → OK.
    env.warp_to(p.sale_start_date);
    env.expire_blockhash();
    send(&mut env, pause_ix(admin.pubkey()), &admin).expect("pause exactly at start (Sale)");
    assert!(env.sale_state().paused);

    // t = end − 1: still Sale → unpause passes.
    env.warp_to(p.sale_end_date - 1);
    env.expire_blockhash();
    send(&mut env, unpause_ix(admin.pubkey()), &admin).expect("unpause at end−1 (Sale)");
    assert!(!env.sale_state().paused);

    // t = end: SaleEnded → WrongPhase.
    env.warp_to(p.sale_end_date);
    env.expire_blockhash();
    let r = send(&mut env, pause_ix(admin.pubkey()), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::WrongPhase)),
        "pause exactly at end (SaleEnded) → WrongPhase: {r:?}"
    );
}

#[test]
fn unpause_allowed_outside_sale() {
    // A pause set during Sale and not lifted before sale_end_date is lifted in
    // SaleEnded: unpause has no phase gate, so the flag cannot be locked in. The
    // positive within Sale itself is pause_allowed_in_sale_only.
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    // PreSale: it passes (the flag is clear anyway).
    send(&mut env, unpause_ix(admin.pubkey()), &admin).expect("unpause in PreSale");
    assert!(!env.sale_state().paused, "paused was cleared in PreSale");

    // We pause in Sale and do NOT lift it before the phase ends.
    env.warp_to(p.sale_start_date);
    env.expire_blockhash();
    send(&mut env, pause_ix(admin.pubkey()), &admin).expect("pause in Sale");
    assert!(env.sale_state().paused, "the pause is in place");

    // SaleEnded: it is lifted.
    env.warp_to(p.sale_end_date);
    env.expire_blockhash();
    send(&mut env, unpause_ix(admin.pubkey()), &admin).expect("unpause in SaleEnded");
    assert!(!env.sale_state().paused, "paused was cleared outside Sale");

    // ClaimClosed - the terminal phase, and unpause still passes.
    env.warp_to(p.claim_start_date + p.claim_duration);
    env.expire_blockhash();
    send(&mut env, unpause_ix(admin.pubkey()), &admin).expect("unpause in ClaimClosed");
    assert!(
        !env.sale_state().paused,
        "paused was cleared in the terminal phase"
    );
}

// ===================================================================
// The sale_end_date boundary for set_sale_schedule (a parameter setter): t = end−1
// is still Sale → SaleAlreadyStarted (the setter is forbidden throughout Sale, not
// only on the PreSale boundary). The setter's positive branch in PreSale is covered
// in params.rs.
// ===================================================================

#[test]
fn param_setter_denied_throughout_sale_window() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default_sale();

    // Inside the Sale window (mid-way): SaleAlreadyStarted.
    let mid = (p.sale_start_date + p.sale_end_date) / 2;
    env.warp_to(mid);
    let r = send(&mut env, set_tx_limits_ix(admin.pubkey(), 5, 10), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::SaleAlreadyStarted)),
        "set_tx_limits mid-Sale → SaleAlreadyStarted: {r:?}"
    );

    // At end−1 (the last second of Sale): also SaleAlreadyStarted.
    env.warp_to(p.sale_end_date - 1);
    env.expire_blockhash();
    let r = send(&mut env, set_tx_limits_ix(admin.pubkey(), 5, 10), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::SaleAlreadyStarted)),
        "set_tx_limits at end−1 (Sale) → SaleAlreadyStarted: {r:?}"
    );
}
