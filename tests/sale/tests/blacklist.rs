//! The behaviour of `blacklist_add` / `blacklist_remove` (Arch §9.7) - the first
//! slice of the deposit path. tests-bind-to-real-code: a real call, an assertion on
//! the presence or absence of the PDA marker, and a specific error code.
//!
//! The invariant: the presence of the PDA `[b"blacklist", wallet]` means the wallet
//! is blocked. add is `init` (a repeated add fails structurally - System `Custom(0)`);
//! remove is `close = operator` (a repeat will not find the account →
//! `AccountNotInitialized`).
//!
//! The role is Operator (§8): `has_one = operator` against `SaleState`. A wrong
//! signer fails with `Unauthorized` during account resolution (before the body).
//!
//! The phase does not gate add/remove (§9.7 "at any time"): the tests run in PreSale,
//! and the phase does not affect the outcome.

use anchor_lang::error::{ErrorCode as AnchorError, ERROR_CODE_OFFSET};
use sale::errors::SaleError;
use sale_test_common::{
    blacklist_add_ix, blacklist_add_ix_with_slot, blacklist_pda, blacklist_remove_ix,
    blacklist_remove_ix_with_slot, custom_error_code, TestEnv,
};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

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
// POSITIVE: the Operator adds → the PDA exists with the correct wallet;
//           the Operator removes → the PDA is closed.
// ===================================================================

#[test]
fn operator_add_then_remove_lifecycle() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    assert!(
        !env.is_blacklisted(&wallet),
        "before the add there must be no marker"
    );

    // add: the PDA appears and the stored wallet matches the seed wallet.
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), wallet),
        &operator,
    )
    .expect("operator: blacklist_add passes");
    assert!(
        env.is_blacklisted(&wallet),
        "after the add the marker exists"
    );
    assert_eq!(
        env.blacklist_entry(&wallet).wallet,
        wallet,
        "the stored wallet == the wallet being blocked (the seed and the field are one truth)"
    );

    // remove: the PDA is closed (we observe the fact of closing through the account's
    // absence).
    env.expire_blockhash();
    send(
        &mut env,
        blacklist_remove_ix(operator.pubkey(), wallet),
        &operator,
    )
    .expect("operator: blacklist_remove passes");
    assert!(
        !env.is_blacklisted(&wallet),
        "after the remove the marker is closed (the account is gone)"
    );
}

#[test]
fn add_remove_add_reblocks_same_wallet() {
    // Removal closes the PDA completely → the same wallet can be blocked again (close
    // leaves no "occupied" account behind).
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();

    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), wallet),
        &operator,
    )
    .expect("the first add");
    env.expire_blockhash();
    send(
        &mut env,
        blacklist_remove_ix(operator.pubkey(), wallet),
        &operator,
    )
    .expect("remove");
    env.expire_blockhash();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), wallet),
        &operator,
    )
    .expect("a repeated add after a remove passes");
    assert!(
        env.is_blacklisted(&wallet),
        "the wallet is blocked again after add→remove→add"
    );
}

// ===================================================================
// NEGATIVE: a non-Operator (owner / admin / user) → Unauthorized (has_one).
// ===================================================================

#[test]
fn add_rejects_non_operator() {
    let mut env = TestEnv::new();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    let wrong: [(&str, Keypair); 3] = [
        ("owner", env.actors.owner.insecure_clone()),
        ("admin", env.actors.admin.insecure_clone()),
        ("user", env.actors.users[0].insecure_clone()),
    ];

    for (label, actor) in wrong {
        env.expire_blockhash();
        let r = send(&mut env, blacklist_add_ix(actor.pubkey(), wallet), &actor);
        assert_eq!(
            custom_error_code(&r),
            Some(err(SaleError::Unauthorized)),
            "{label} invoked blacklist_add → Unauthorized was expected: {r:?}"
        );
        assert!(
            !env.is_blacklisted(&wallet),
            "{label}: a failed add creates no marker"
        );
    }
}

#[test]
fn remove_rejects_non_operator() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    env.init_default_sale();

    // First we block validly (as operator), then we try to remove with the wrong role.
    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), wallet),
        &operator,
    )
    .expect("operator: an add to set things up");

    let wrong: [(&str, Keypair); 3] = [
        ("owner", env.actors.owner.insecure_clone()),
        ("admin", env.actors.admin.insecure_clone()),
        ("user", env.actors.users[0].insecure_clone()),
    ];

    for (label, actor) in wrong {
        env.expire_blockhash();
        let r = send(
            &mut env,
            blacklist_remove_ix(actor.pubkey(), wallet),
            &actor,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(err(SaleError::Unauthorized)),
            "{label} invoked blacklist_remove → Unauthorized was expected: {r:?}"
        );
        assert!(
            env.is_blacklisted(&wallet),
            "{label}: a failed remove does not close the marker"
        );
    }
}

// ===================================================================
// NEGATIVE: a repeated add of the same wallet → System `Custom(0)` (init on an
// occupied account; the same fact as double-initialize, iteration 1).
// ===================================================================

#[test]
fn double_add_same_wallet_fails() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), wallet),
        &operator,
    )
    .expect("the first add passes");

    env.expire_blockhash();
    let r = send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), wallet),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(0),
        "a repeated add → System `account already in use` = Custom(0): {r:?}"
    );
    assert!(
        env.is_blacklisted(&wallet),
        "the first marker is still in place"
    );
}

// ===================================================================
// NEGATIVE: removing a non-existent record → AccountNotInitialized.
// ===================================================================

#[test]
fn remove_nonexistent_fails() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    assert!(
        !env.is_blacklisted(&wallet),
        "precondition: there is no marker"
    );

    let r = send(
        &mut env,
        blacklist_remove_ix(operator.pubkey(), wallet),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "removing an absent record → AccountNotInitialized: {r:?}"
    );
}

// ===================================================================
// NEGATIVE: a seed mismatch - the blacklist slot is derived from a DIFFERENT wallet
// than the wallet account → ConstraintSeeds (the PDA does not match the wallet).
// ===================================================================

#[test]
fn add_seed_mismatch_fails() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    let other = Pubkey::new_unique();
    // The slot is derived from `other` while the wallet account is `wallet`: the seeds
    // will not agree.
    let mismatched_slot = blacklist_pda(&other);

    let r = send(
        &mut env,
        blacklist_add_ix_with_slot(operator.pubkey(), wallet, mismatched_slot),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "an add whose slot is not derived from wallet → ConstraintSeeds: {r:?}"
    );
    assert!(
        !env.is_blacklisted(&wallet),
        "on a seed mismatch no marker is created"
    );
}

#[test]
fn remove_seed_mismatch_fails() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    env.init_default_sale();

    // We block `wallet` validly.
    let wallet = Pubkey::new_unique();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), wallet),
        &operator,
    )
    .expect("an add to set things up");

    // A remove whose slot belongs to another wallet while the wallet account is the
    // blocked one. The `other` slot is empty (nobody blocked it), so for a `close`
    // account Anchor checks initialization EARLIER than the seeds and answers
    // AccountNotInitialized: closing a record that does not belong to `wallet` is
    // impossible by construction.
    let other = Pubkey::new_unique();
    let mismatched_slot = blacklist_pda(&other);

    env.expire_blockhash();
    let r = send(
        &mut env,
        blacklist_remove_ix_with_slot(operator.pubkey(), wallet, mismatched_slot),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(anchor_err(AnchorError::AccountNotInitialized)),
        "a remove whose slot is not derived from wallet (an empty slot) → AccountNotInitialized: {r:?}"
    );
    assert!(
        env.is_blacklisted(&wallet),
        "on a seed mismatch the marker is untouched"
    );
}

#[test]
fn remove_existing_slot_for_wrong_wallet_fails() {
    // The slot is the REALLY existing marker of wallet `other`, while the wallet account
    // is `wallet`. The account is initialized (the discriminator is there), so the seed
    // check does fire and answers ConstraintSeeds: someone else's marker cannot be
    // closed.
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    env.init_default_sale();

    let wallet = Pubkey::new_unique();
    let other = Pubkey::new_unique();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), wallet),
        &operator,
    )
    .expect("add wallet");
    env.expire_blockhash();
    send(
        &mut env,
        blacklist_add_ix(operator.pubkey(), other),
        &operator,
    )
    .expect("add other");

    // remove: the wallet account is `wallet`, but the slot is `other`'s marker (which
    // exists).
    env.expire_blockhash();
    let r = send(
        &mut env,
        blacklist_remove_ix_with_slot(operator.pubkey(), wallet, blacklist_pda(&other)),
        &operator,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(anchor_err(AnchorError::ConstraintSeeds)),
        "removing a live foreign slot → ConstraintSeeds: {r:?}"
    );
    assert!(env.is_blacklisted(&wallet), "wallet's marker is untouched");
    assert!(env.is_blacklisted(&other), "other's marker is untouched");
}
