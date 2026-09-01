//! The behaviour of `graylist_create` / `graylist_update` / `graylist_remove`
//! (Arch §9.7a) - the second slice of the deposit path, after blacklist.
//! tests-bind-to-real-code: a real call, an assertion on the presence or absence of
//! the record and on the stored limits, and a specific error code.
//!
//! The invariant: the presence of the record `[b"graylist", wallet]` means the wallet
//! is under individual limits instead of the global ones. The record stores data
//! (`min_per_tx`, `max_per_tx`, `max_total`) rather than "presence = permission".
//!
//! `graylist_set` (the collective name of §9.7a) is split into two instructions
//! because `init_if_needed` is forbidden (§13):
//! - `graylist_create` = `init` (payer = admin); a repeated creation fails
//!   structurally on `init` → System `Custom(0)` (there is no dedicated code);
//! - `graylist_update` = `mut` on an existing record (there are no counters, so
//!   editing non-cumulative fields is correct); updating an absent record →
//!   `AccountNotInitialized`.
//!
//! Both emit `GraylistSet` and validate `0 < min ≤ max ≤ max_total`.
//! `graylist_remove` = `close = admin` (the rent goes back to admin).
//!
//! The role is Admin (§8): `has_one = admin` against `SaleState`. A wrong signer fails
//! with `Unauthorized` during account resolution (before the body).
//!
//! The phase does not gate create/update/remove (§9.7a "at any time"): the tests run
//! in PreSale, and the phase does not affect the outcome.

use anchor_lang::error::{ErrorCode as AnchorError, ERROR_CODE_OFFSET};
use sale::errors::SaleError;
use sale_test_common::{
    custom_error_code, graylist_create_ix, graylist_create_ix_with_slot, graylist_pda,
    graylist_remove_ix, graylist_remove_ix_with_slot, graylist_update_ix,
    graylist_update_ix_with_slot, TestEnv,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

// A self-consistent baseline set of limits: 0 < min ≤ max ≤ max_total. The negative
// tests build their deviations from exactly this one.
const MIN: u64 = 10;
const MAX: u64 = 1_000;
const TOTAL: u64 = 10_000;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

fn anchor_err(code: AnchorError) -> u32 {
    code as u32
}

#[allow(clippy::result_large_err)]
fn send(
    env: &mut TestEnv,
    ix: solana_instruction::Instruction,
    signer: &Keypair,
) -> litesvm::types::TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

// ===================================================================
// POSITIVE: the Admin creates a record → the PDA exists and the fields match.
// ===================================================================

#[test]
fn admin_create_stores_limits() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    assert!(
        !env.is_graylisted(&wallet),
        "before the create there must be no record"
    );

    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("admin: graylist_create passes");

    assert!(
        env.is_graylisted(&wallet),
        "after the create the record exists"
    );
    let entry = env.graylist_entry(&wallet);
    assert_eq!(
        entry.wallet, wallet,
        "the stored wallet == the seed wallet (one truth)"
    );
    assert_eq!(entry.min_per_tx, MIN, "min_per_tx was stored");
    assert_eq!(entry.max_per_tx, MAX, "max_per_tx was stored");
    assert_eq!(entry.max_total, TOTAL, "max_total was stored");
}

// ===================================================================
// POSITIVE: the Admin overwrites the limits of an existing record → the new values.
// ===================================================================

#[test]
fn admin_update_overwrites_limits() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("a create to set things up");

    // New limits, different from the baseline ones (still self-consistent).
    let new_min = 50;
    let new_max = 2_000;
    let new_total = 50_000;

    env.expire_blockhash();
    send(
        &mut env,
        graylist_update_ix(admin.pubkey(), wallet, new_min, new_max, new_total),
        &admin,
    )
    .expect("admin: graylist_update passes");

    let entry = env.graylist_entry(&wallet);
    assert_eq!(entry.min_per_tx, new_min, "min_per_tx was overwritten");
    assert_eq!(entry.max_per_tx, new_max, "max_per_tx was overwritten");
    assert_eq!(entry.max_total, new_total, "max_total was overwritten");
    assert_eq!(entry.wallet, wallet, "wallet is unchanged after the update");
}

// ===================================================================
// POSITIVE: the Admin closes the record → the PDA is gone; the wallet is back under
// the global limits (there is no record).
// ===================================================================

#[test]
fn admin_remove_closes_entry() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("a create to set things up");
    assert!(env.is_graylisted(&wallet), "the record was created");

    env.expire_blockhash();
    send(&mut env, graylist_remove_ix(admin.pubkey(), wallet), &admin)
        .expect("admin: graylist_remove passes");
    assert!(
        !env.is_graylisted(&wallet),
        "after the remove the record is closed (the account is gone) - the wallet is back under the global limits"
    );
}

#[test]
fn create_remove_create_re_sets_same_wallet() {
    // Removal closes the PDA completely → the same wallet can be listed again.
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("the first create");
    env.expire_blockhash();
    send(&mut env, graylist_remove_ix(admin.pubkey(), wallet), &admin).expect("remove");
    env.expire_blockhash();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("a repeated create after a remove passes");
    assert!(
        env.is_graylisted(&wallet),
        "the wallet is in the graylist again after create→remove→create"
    );
}

// ===================================================================
// BOUNDARY: min == max == max_total (equalities are allowed) → create passes.
// ===================================================================

#[test]
fn create_accepts_all_equal_limits() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    let v = 500;
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, v, v, v),
        &admin,
    )
    .expect("min == max == max_total is allowed");

    let entry = env.graylist_entry(&wallet);
    assert_eq!(entry.min_per_tx, v);
    assert_eq!(entry.max_per_tx, v);
    assert_eq!(entry.max_total, v);
}

// ===================================================================
// NEGATIVE, role: a non-Admin (owner / operator / user) → Unauthorized (has_one).
// ===================================================================

#[test]
fn create_rejects_non_admin() {
    let mut env = TestEnv::new();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    let wrong: [(&str, Keypair); 3] = [
        ("owner", env.actors.owner.insecure_clone()),
        ("operator", env.actors.operator.insecure_clone()),
        ("user", env.actors.users[0].insecure_clone()),
    ];

    for (label, actor) in wrong {
        env.expire_blockhash();
        let r = send(
            &mut env,
            graylist_create_ix(actor.pubkey(), wallet, MIN, MAX, TOTAL),
            &actor,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(err(SaleError::Unauthorized)),
            "{label} invoked graylist_create → Unauthorized was expected: {r:?}"
        );
        assert!(
            !env.is_graylisted(&wallet),
            "{label}: a failed create creates no record"
        );
    }
}

#[test]
fn update_rejects_non_admin() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("a create to set things up");

    let wrong: [(&str, Keypair); 3] = [
        ("owner", env.actors.owner.insecure_clone()),
        ("operator", env.actors.operator.insecure_clone()),
        ("user", env.actors.users[0].insecure_clone()),
    ];

    for (label, actor) in wrong {
        env.expire_blockhash();
        let r = send(
            &mut env,
            graylist_update_ix(actor.pubkey(), wallet, 50, 2_000, 50_000),
            &actor,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(err(SaleError::Unauthorized)),
            "{label} invoked graylist_update → Unauthorized was expected: {r:?}"
        );
        let entry = env.graylist_entry(&wallet);
        assert_eq!(
            entry.min_per_tx, MIN,
            "{label}: a failed update does not touch the limits"
        );
    }
}

#[test]
fn remove_rejects_non_admin() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("a create to set things up");

    let wrong: [(&str, Keypair); 3] = [
        ("owner", env.actors.owner.insecure_clone()),
        ("operator", env.actors.operator.insecure_clone()),
        ("user", env.actors.users[0].insecure_clone()),
    ];

    for (label, actor) in wrong {
        env.expire_blockhash();
        let r = send(&mut env, graylist_remove_ix(actor.pubkey(), wallet), &actor);
        assert_eq!(
            custom_error_code(&r),
            Some(err(SaleError::Unauthorized)),
            "{label} invoked graylist_remove → Unauthorized was expected: {r:?}"
        );
        assert!(
            env.is_graylisted(&wallet),
            "{label}: a failed remove does not close the record"
        );
    }
}

// ===================================================================
// NEGATIVE, validation (on create AND on update): violations of
// `0 < min ≤ max ≤ max_total` → InvalidParams. One test per boundary.
// ===================================================================

#[test]
fn create_rejects_zero_min() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    let r = send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, 0, MAX, TOTAL),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "create min_per_tx == 0 → InvalidParams: {r:?}"
    );
    assert!(
        !env.is_graylisted(&wallet),
        "an invalid create creates no record"
    );
}

#[test]
fn create_rejects_min_above_max() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    // min > max: MAX+1 > MAX, with max_total large enough (it is not the one violated).
    let r = send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MAX + 1, MAX, TOTAL),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "create min > max → InvalidParams: {r:?}"
    );
    assert!(
        !env.is_graylisted(&wallet),
        "an invalid create creates no record"
    );
}

#[test]
fn create_rejects_max_above_total() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    // max > max_total: TOTAL+1 > TOTAL, with min ≤ max satisfied.
    let r = send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, TOTAL + 1, TOTAL),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "create max > max_total → InvalidParams: {r:?}"
    );
    assert!(
        !env.is_graylisted(&wallet),
        "an invalid create creates no record"
    );
}

#[test]
fn update_rejects_zero_min() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("a create to set things up");

    env.expire_blockhash();
    let r = send(
        &mut env,
        graylist_update_ix(admin.pubkey(), wallet, 0, MAX, TOTAL),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "update min_per_tx == 0 → InvalidParams: {r:?}"
    );
    assert_eq!(
        env.graylist_entry(&wallet).min_per_tx,
        MIN,
        "an invalid update does not touch the limits"
    );
}

#[test]
fn update_rejects_min_above_max() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("a create to set things up");

    env.expire_blockhash();
    let r = send(
        &mut env,
        graylist_update_ix(admin.pubkey(), wallet, MAX + 1, MAX, TOTAL),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "update min > max → InvalidParams: {r:?}"
    );
    assert_eq!(
        env.graylist_entry(&wallet).min_per_tx,
        MIN,
        "an invalid update does not touch the limits"
    );
}

#[test]
fn update_rejects_max_above_total() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("a create to set things up");

    env.expire_blockhash();
    let r = send(
        &mut env,
        graylist_update_ix(admin.pubkey(), wallet, MIN, TOTAL + 1, TOTAL),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(SaleError::InvalidParams)),
        "update max > max_total → InvalidParams: {r:?}"
    );
    assert_eq!(
        env.graylist_entry(&wallet).max_per_tx,
        MAX,
        "an invalid update does not touch the limits"
    );
}

// ===================================================================
// NEGATIVE: a repeated creation for the same wallet → System `Custom(0)` (init on an
// occupied account).
// ===================================================================

#[test]
fn double_create_same_wallet_fails() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("the first create passes");

    env.expire_blockhash();
    let r = send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, 50, 2_000, 50_000),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(0),
        "a repeated create → System `account already in use` = Custom(0): {r:?}"
    );
    // The first record is in place and was not overwritten by the repeated create.
    assert_eq!(
        env.graylist_entry(&wallet).min_per_tx,
        MIN,
        "the first record still holds its original limits"
    );
}

// ===================================================================
// NEGATIVE: updating a non-existent record → AccountNotInitialized.
// ===================================================================

#[test]
fn update_nonexistent_fails() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    assert!(
        !env.is_graylisted(&wallet),
        "precondition: there is no record"
    );

    let r = send(
        &mut env,
        graylist_update_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "updating an absent record → AccountNotInitialized: {r:?}"
    );
}

// ===================================================================
// NEGATIVE: a seed mismatch - the graylist slot is derived from a DIFFERENT wallet
// than the wallet account → ConstraintSeeds (create); and a live foreign slot on
// update/remove → ConstraintSeeds (as in the blacklist test).
// ===================================================================

#[test]
fn create_seed_mismatch_fails() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    let other = Pubkey::new_unique();
    // The slot is derived from `other` while the wallet account is `wallet`: the seeds
    // will not agree.
    let mismatched_slot = graylist_pda(&other);

    let r = send(
        &mut env,
        graylist_create_ix_with_slot(admin.pubkey(), wallet, mismatched_slot, MIN, MAX, TOTAL),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "a create whose slot is not derived from wallet → ConstraintSeeds: {r:?}"
    );
    assert!(
        !env.is_graylisted(&wallet),
        "on a seed mismatch no record is created"
    );
}

#[test]
fn update_existing_slot_for_wrong_wallet_fails() {
    // The slot is the REALLY existing record of wallet `other` while the wallet account
    // is `wallet`. The account is initialized (the discriminator is there), so the seed
    // check fires → ConstraintSeeds: someone else's record cannot be updated.
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    let other = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("create wallet");
    env.expire_blockhash();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), other, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("create other");

    env.expire_blockhash();
    let r = send(
        &mut env,
        graylist_update_ix_with_slot(
            admin.pubkey(),
            wallet,
            graylist_pda(&other),
            50,
            2_000,
            50_000,
        ),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "updating a live foreign slot → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        env.graylist_entry(&other).min_per_tx,
        MIN,
        "other's record is untouched"
    );
}

#[test]
fn remove_existing_slot_for_wrong_wallet_fails() {
    // The slot is `other`'s existing record and the wallet account is `wallet`. The
    // account is initialized → ConstraintSeeds: someone else's record cannot be closed.
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    let other = Pubkey::new_unique();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), wallet, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("create wallet");
    env.expire_blockhash();
    send(
        &mut env,
        graylist_create_ix(admin.pubkey(), other, MIN, MAX, TOTAL),
        &admin,
    )
    .expect("create other");

    env.expire_blockhash();
    let r = send(
        &mut env,
        graylist_remove_ix_with_slot(admin.pubkey(), wallet, graylist_pda(&other)),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "removing a live foreign slot → ConstraintSeeds: {r:?}"
    );
    assert!(env.is_graylisted(&wallet), "wallet's record is untouched");
    assert!(env.is_graylisted(&other), "other's record is untouched");
}
