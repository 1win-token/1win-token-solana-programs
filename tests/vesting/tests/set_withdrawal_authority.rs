//! `set_withdrawal_authority` - Admin reassigns the withdrawal target: the address
//! that receives the remaining $1WIN and the rent when an activity is cancelled or
//! closed.
//!
//! Why the tests are laid out the way they are.
//!
//! 1. "The field was updated" is not enough: the value is read only by
//!    `cancel_vesting` and `close_vesting`, and the point of the change is that the
//!    funds really arrive at the new address. So both positives are chains: change
//!    the target, then cancel (before TGE) and close (after TGE) an activity,
//!    asserting the token-account balances and the lamports of both addresses - the
//!    new one and the previous one.
//! 2. After the change the previous target must stop passing: its ATA is rejected as
//!    `InvalidBeneficiary` and the target itself in the rent slot as
//!    `InvalidRentCollector`. Both negatives stand in the same transaction sequence as
//!    the positive, and before it.
//! 3. The instruction does not check time, so the change is executed both before TGE
//!    (the cancellation positive) and after (the close positive). The three moments
//!    to the second live in `tge_matrix.rs`.
//! 4. The role negative with a foreign key checks only the address comparison, which
//!    requires no signature. So a separate case covers "the Admin key is correct, only
//!    the signature flag was cleared" - without it, changing the slot's type to
//!    `UncheckedAccount` would let an outsider redirect every remainder and all the
//!    rent.
//! 5. The `config` and `vault_authority` slots are checked by substitution: against a
//!    decoy config the role check would run on foreign fields, and against a decoy
//!    `vault_authority` the "the withdrawal target != the vaults' owner" condition
//!    would be checked against an address supplied by the caller, that is, disabled
//!    by them.
//! 6. Every negative asserts the config is unchanged, and the chains additionally
//!    assert that the vault, the activity record and the previous target's account are
//!    unchanged: a partial effect here is more dangerous than the rejection itself.
//!
//! ===== DELIBERATELY DEFERRED =====
//! - Substituting the service slots `event_authority` / `program` of the
//!   `#[event_cpi]` context - `account_substitution.rs`.
//! - A multisig as the real Admin signer: the tests use a single key, as does the rest
//!   of the suite.

use anchor_lang::error::ErrorCode;
use litesvm::types::TransactionResult;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting::errors::VestingError;
use vesting::events::WithdrawalAuthorityUpdated;
use vesting_test_common::vesting_expected::vesting::{ECOSYSTEM_FULL_RELEASE_DAY, V2_REFERENCE};
use vesting_test_common::{
    cancel_vesting_ix, close_vesting_ix, config_pda, custom_error_code, decode_event, err,
    set_withdrawal_authority_ix, set_withdrawal_authority_ix_with_slots, vault_authority_pda,
    vault_pda, vesting_pda, without_signer, ActivitySpec, TestEnv, DEFAULT_TGE_OFFSET,
};

/// The activity's reference amount: the V2 bucket of §10.5 in token units.
const REFERENCE: u64 = V2_REFERENCE;

/// How much sits in the vault by the time of the cancellation. It is not equal to
/// `REFERENCE`: what is withdrawn is the vault's actual balance rather than the
/// reference amount.
const VAULT_FUNDED: u64 = 3_333_000_000_000;

/// A stray transfer beyond the reference amount: the recipient cannot claim it, and on
/// a close it goes to the withdrawal target. The value is not a multiple of the
/// reference amount so that the remainder cannot be confused with a tranche or with
/// the full balance.
const VAULT_EXCESS: u64 = 555_111;

/// The balance on the previous target's token account BEFORE the change: non-zero, so
/// that "the account did not change" differs from "the account was zeroed".
const OLD_TARGET_PREFUNDED: u64 = 4_242;

// We return the native litesvm TransactionResult as is - just like `send_ixs`.
#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// The keys the instruction must reject: every actor in the environment except Admin.
/// Owner is in the list on equal terms: the withdrawal target is part of the
/// configuration and only Admin edits it.
fn wrong_admins(env: &TestEnv) -> Vec<(&'static str, Keypair)> {
    vec![
        ("owner", env.actors.owner.insecure_clone()),
        ("operator", env.actors.operator.insecure_clone()),
        ("recipient", env.actors.recipients[0].insecure_clone()),
        ("beneficiary", env.actors.beneficiary.insecure_clone()),
        ("rent_collector", env.actors.rent_collector.insecure_clone()),
        ("stranger", env.actors.stranger.insecure_clone()),
    ]
}

/// An environment with a config and an activity whose vault is funded with
/// `vault_amount`. The previous withdrawal target is the `rent_collector` actor
/// (written by `init_default`), and it already has a token account holding
/// [`OLD_TARGET_PREFUNDED`]. Returns `(env, activity_id, tge_ts, the old token
/// account)`.
fn setup(vault_amount: u64) -> (TestEnv, u64, i64, Pubkey) {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    assert_eq!(
        params.withdrawal_authority,
        env.actors.rent_collector.pubkey(),
        "the fixture: the previous withdrawal target is the rent_collector actor"
    );
    let recipient = env.actors.recipients[0].pubkey();
    let activity_id = env.create_activity_for(&recipient, &ActivitySpec::valid(REFERENCE));
    env.fund_vault(activity_id, vault_amount);

    let win_mint = env.win_mint;
    let old_token = env.create_ata(&params.withdrawal_authority, &win_mint);
    env.mint_to(&win_mint, &old_token, OLD_TARGET_PREFUNDED);

    (env, activity_id, params.tge_ts, old_token)
}

/// The rejection did not rewrite the withdrawal target in the config.
fn assert_target_unchanged(env: &TestEnv, expected: Pubkey, case: &str) {
    let c = env.vesting_config();
    assert_eq!(
        c.withdrawal_authority, expected,
        "{case}: the withdrawal target was not changed"
    );
}

// ===================================================================
// POSITIVE: the field and the event
// ===================================================================

#[test]
fn set_withdrawal_authority_updates_field_and_emits_event() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let fresh = env.actors.beneficiary.pubkey();
    assert_ne!(
        fresh, before.withdrawal_authority,
        "the fixture: the withdrawal target really does change"
    );

    let r = send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), fresh),
        &admin,
    );
    r.as_ref().expect("admin changes the withdrawal target");

    let c = env.vesting_config();
    assert_eq!(
        c.withdrawal_authority, fresh,
        "the withdrawal target was taken from the slot"
    );
    assert_eq!(c.owner, before.owner, "owner is untouched");
    assert_eq!(c.admin, before.admin, "admin is untouched");
    assert_eq!(c.operator, before.operator, "operator is untouched");
    assert_eq!(c.token_mint, before.token_mint, "the mint is untouched");
    assert_eq!(c.tge_ts, before.tge_ts, "the TGE date is untouched");
    assert_eq!(c.activity_count, 0, "the activity counter is untouched");

    let ev: WithdrawalAuthorityUpdated =
        decode_event(&r).expect("WithdrawalAuthorityUpdated is emitted");
    assert_eq!(
        ev.withdrawal_authority, fresh,
        "the event carries the value after the change"
    );

    // A repeated call restores the previous address: a mistaken target is fixable while
    // no activity has been closed - which is why the handover is one-step.
    env.expire_blockhash();
    send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), before.withdrawal_authority),
        &admin,
    )
    .expect("a repeated call restores the previous withdrawal target");
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        before.withdrawal_authority,
        "the withdrawal target returned to its previous address"
    );
}

// ===================================================================
// A POSITIVE chain before TGE: change the target → cancel the activity
// ===================================================================

#[test]
fn cancel_vesting_pays_the_new_withdrawal_authority() {
    let (mut env, id, _tge, old_token) = setup(VAULT_FUNDED);
    let admin = env.actors.admin.insecure_clone();
    let owner = env.actors.owner.insecure_clone();
    let old_target = env.actors.rent_collector.pubkey();
    let new_target = env.actors.beneficiary.pubkey();
    let win_mint = env.win_mint;
    let new_token = env.create_ata(&new_target, &win_mint);

    // Admin edits the withdrawal target while Owner cancels the activity: both keys take
    // part in the chain.
    send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), new_target),
        &admin,
    )
    .expect("admin changes the withdrawal target before TGE");
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        new_target,
        "the config holds the new withdrawal target"
    );

    // The previous target no longer passes in either of the two slots: its ATA is not the
    // withdrawal target's account, and the target itself is not the rent addressee.
    for (case, beneficiary_token, rent_collector, code) in [
        (
            "the previous target's ATA",
            old_token,
            new_target,
            err(VestingError::InvalidBeneficiary),
        ),
        (
            "the previous target in the rent slot",
            new_token,
            old_target,
            err(VestingError::InvalidRentCollector),
        ),
    ] {
        env.expire_blockhash();
        let ix = cancel_vesting_ix(
            owner.pubkey(),
            id,
            beneficiary_token,
            rent_collector,
            win_mint,
        );
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom_error_code(&r),
            Some(code),
            "{case} after the withdrawal target changed: {r:?}"
        );
        assert!(
            env.vesting_exists(id) && env.vault_exists(id),
            "{case}: the rejection did not close the activity's accounts"
        );
        assert_eq!(
            env.vault_balance(id),
            VAULT_FUNDED,
            "{case}: the rejection did not touch the vault"
        );
    }

    let vesting_rent = env.lamports_of(&vesting_pda(id));
    let vault_rent = env.lamports_of(&vault_pda(id));
    let new_lamports_before = env.lamports_of(&new_target);
    let old_lamports_before = env.lamports_of(&old_target);

    env.expire_blockhash();
    let ix = cancel_vesting_ix(owner.pubkey(), id, new_token, new_target, win_mint);
    send(&mut env, ix, &owner).expect("the cancellation withdraws the remainder to the new target");

    assert!(!env.vault_exists(id), "the vault is closed");
    assert!(!env.vesting_exists(id), "the activity record is closed");
    assert_eq!(
        env.token_balance(&new_token),
        VAULT_FUNDED,
        "the vault's remainder went to the NEW target's token account"
    );
    assert_eq!(
        env.token_balance(&old_token),
        OLD_TARGET_PREFUNDED,
        "the previous target's account neither changed nor was zeroed"
    );
    assert_eq!(
        env.lamports_of(&new_target) - new_lamports_before,
        vesting_rent + vault_rent,
        "the rent of both accounts arrived at the new target"
    );
    assert_eq!(
        env.lamports_of(&old_target),
        old_lamports_before,
        "the previous target got not a single lamport"
    );
}

// ===================================================================
// A POSITIVE chain after TGE: change the target → close a drawn-down activity
// ===================================================================

#[test]
fn close_vesting_pays_the_new_withdrawal_authority_after_tge() {
    let (mut env, id, tge_ts, old_token) = setup(REFERENCE + VAULT_EXCESS);
    let admin = env.actors.admin.insecure_clone();
    let owner = env.actors.owner.insecure_clone();
    let recipient = env.actors.recipients[0].insecure_clone();
    let old_target = env.actors.rent_collector.pubkey();
    let new_target = env.actors.beneficiary.pubkey();
    let win_mint = env.win_mint;
    let new_token = env.create_ata(&new_target, &win_mint);

    // The terminal claim: the activity is fully drawn down and only the stray transfer
    // beyond the reference amount remains in the vault.
    env.claim_at_day(tge_ts, ECOSYSTEM_FULL_RELEASE_DAY, id, &recipient)
        .expect("the terminal claim releases the whole reference amount");
    assert_eq!(
        env.vesting(id).claimed,
        REFERENCE,
        "precondition: the activity is fully drawn down"
    );
    assert_eq!(
        env.vault_balance(id),
        VAULT_EXCESS,
        "precondition: a surplus remained in the vault"
    );
    assert!(env.now() > tge_ts, "precondition: TGE has already arrived");

    // The instruction does not check time: the value is committed to nothing, and every
    // close reads the config's current field.
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), new_target),
        &admin,
    );
    r.as_ref()
        .expect("admin changes the withdrawal target after TGE");
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        new_target,
        "after TGE the withdrawal target changes too"
    );
    let ev: WithdrawalAuthorityUpdated =
        decode_event(&r).expect("WithdrawalAuthorityUpdated is emitted");
    assert_eq!(ev.withdrawal_authority, new_target, "the event after TGE");

    for (case, beneficiary_token, rent_collector, code) in [
        (
            "the previous target's ATA",
            old_token,
            new_target,
            err(VestingError::InvalidBeneficiary),
        ),
        (
            "the previous target in the rent slot",
            new_token,
            old_target,
            err(VestingError::InvalidRentCollector),
        ),
    ] {
        env.expire_blockhash();
        let ix = close_vesting_ix(
            owner.pubkey(),
            id,
            beneficiary_token,
            rent_collector,
            win_mint,
        );
        let r = send(&mut env, ix, &owner);
        assert_eq!(
            custom_error_code(&r),
            Some(code),
            "{case} after the withdrawal target changed: {r:?}"
        );
        assert!(
            env.vesting_exists(id) && env.vault_exists(id),
            "{case}: the rejection did not close the activity's accounts"
        );
        assert_eq!(
            env.vault_balance(id),
            VAULT_EXCESS,
            "{case}: the rejection did not touch the surplus in the vault"
        );
    }

    let vesting_rent = env.lamports_of(&vesting_pda(id));
    let vault_rent = env.lamports_of(&vault_pda(id));
    let new_lamports_before = env.lamports_of(&new_target);
    let old_lamports_before = env.lamports_of(&old_target);

    env.expire_blockhash();
    let ix = close_vesting_ix(owner.pubkey(), id, new_token, new_target, win_mint);
    send(&mut env, ix, &owner).expect("the close withdraws the surplus to the new target");

    assert!(!env.vault_exists(id), "the vault is closed");
    assert!(!env.vesting_exists(id), "the activity record is closed");
    assert_eq!(
        env.token_balance(&new_token),
        VAULT_EXCESS,
        "the surplus went to the NEW target's token account"
    );
    assert_eq!(
        env.token_balance(&old_token),
        OLD_TARGET_PREFUNDED,
        "the previous target's account did not change"
    );
    assert_eq!(
        env.lamports_of(&new_target) - new_lamports_before,
        vesting_rent + vault_rent,
        "the rent of both accounts arrived at the new target"
    );
    assert_eq!(
        env.lamports_of(&old_target),
        old_lamports_before,
        "the previous target got not a single lamport"
    );
}

// ===================================================================
// NEGATIVES: the right to sign
// ===================================================================

#[test]
fn set_withdrawal_authority_rejects_wrong_signers() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let fresh = env.actors.beneficiary.pubkey();

    // The most dangerous case in the list is Owner: it creates, cancels and closes
    // activities, yet it is not the one that sets their withdrawal address. The actor's
    // key goes BOTH into the admin slot AND into the signers - otherwise the rejection
    // would come from the missing signature.
    for (label, actor) in wrong_admins(&env) {
        env.expire_blockhash();
        let ix = set_withdrawal_authority_ix(actor.pubkey(), fresh);
        let r = send(&mut env, ix, &actor);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::NotAdmin)),
            "{label} signed set_withdrawal_authority → NotAdmin was expected: {r:?}"
        );
        assert_target_unchanged(&env, before.withdrawal_authority, label);
    }

    // A POSITIVE in the same config: the correct Admin passes.
    env.expire_blockhash();
    send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), fresh),
        &admin,
    )
    .expect("the correct admin passes");
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        fresh,
        "only Admin changed the withdrawal target"
    );
}

#[test]
fn set_withdrawal_authority_rejects_unsigned_admin_slot() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let stranger = env.actors.stranger.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let fresh = env.actors.beneficiary.pubkey();

    let expected = u32::from(ErrorCode::AccountNotSigner);
    assert_eq!(
        expected, 3010,
        "the nominal anchor code AccountNotSigner (Anchor 1.0.2)"
    );

    // The key in the `admin` slot is correct and only the signature flag was cleared: an
    // address comparison would let such a call through, and it is the `Signer` type that
    // rejects it. The fee is paid by an outsider - otherwise the slot would get its
    // signature back.
    let ix = without_signer(
        set_withdrawal_authority_ix(admin.pubkey(), fresh),
        &admin.pubkey(),
    );
    let r = env.send_ixs(&[ix], &stranger.pubkey(), &[&stranger]);
    assert_eq!(
        custom_error_code(&r),
        Some(expected),
        "admin's signature was cleared → AccountNotSigner during account resolution, before the body: {r:?}"
    );
    assert_target_unchanged(
        &env,
        before.withdrawal_authority,
        "admin's signature was cleared",
    );

    // POSITIVE: the same call with a signature passes.
    env.expire_blockhash();
    send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), fresh),
        &admin,
    )
    .expect("the same call with admin's signature passes");
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        fresh,
        "the withdrawal target was changed by the signed call"
    );
}

// ===================================================================
// NEGATIVES: the target's address and slot substitutions
// ===================================================================

#[test]
fn set_withdrawal_authority_rejects_vault_authority_as_target() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);

    // `vault_authority` is a data-less PDA (owned by the System Program), so the
    // `SystemAccount` type lets it through and the check in the body rejects it. Were the
    // substitution to pass, the remainder and the rent would end up in the program's own
    // custody, and the program has no instruction that takes funds off that address.
    let r = send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), vault_authority_pda()),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidAuthority)),
        "the withdrawal target == vault_authority → InvalidAuthority: {r:?}"
    );
    assert_target_unchanged(
        &env,
        before.withdrawal_authority,
        "the withdrawal target == vault_authority",
    );

    // A POSITIVE in the same config: an ordinary wallet passes.
    env.expire_blockhash();
    let fresh = env.actors.beneficiary.pubkey();
    send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), fresh),
        &admin,
    )
    .expect("an ordinary wallet in the withdrawal target slot passes");
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        fresh,
        "the withdrawal target changed only to a permitted address"
    );
}

#[test]
fn set_withdrawal_authority_rejects_non_canonical_vault_authority_slot() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let fresh = env.actors.beneficiary.pubkey();

    // The `vault_authority` slot is bound by the seeds `[b"vault_authority"]` and by the
    // bump from the config: pass an arbitrary address here and the "the withdrawal target
    // != the vaults' owner" condition would be checked against a decoy key.
    let ix = set_withdrawal_authority_ix_with_slots(
        admin.pubkey(),
        config_pda(),
        fresh,
        Pubkey::new_unique(),
    );
    let r = send(&mut env, ix, &admin);
    // The code is pinned by the first execution: ConstraintSeeds (2006) - Anchor derives
    // the slot's address from the seeds and the decoy does not match it.
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a non-canonical address in the vault_authority slot → ConstraintSeeds: {r:?}"
    );
    assert_target_unchanged(&env, before.withdrawal_authority, "a decoy vault_authority");

    // POSITIVE: the canonical slot passes with the same call.
    env.expire_blockhash();
    send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), fresh),
        &admin,
    )
    .expect("the canonical vault_authority is accepted");
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        fresh,
        "the withdrawal target was changed"
    );
}

#[test]
fn set_withdrawal_authority_rejects_forged_config() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let fresh = env.actors.beneficiary.pubkey();

    // A forge: a program-owned account with a genuine discriminator and a copy of
    // a live config's fields at a random address; there is no honest way to obtain one -
    // `initialize` only ever creates the config at the PDA of the seeds `[b"config"]`.
    // Were the substitution to pass, Admin's right would be checked against a foreign
    // config.
    let forged = Pubkey::new_unique();
    let snapshot = env.vesting_config();
    env.forge_config(&forged, &snapshot);
    assert_ne!(
        forged,
        config_pda(),
        "the forge sits outside the canonical address"
    );
    let forged_before = env
        .svm
        .get_account(&forged)
        .expect("the decoy config was written")
        .data;

    let ix = set_withdrawal_authority_ix_with_slots(
        admin.pubkey(),
        forged,
        fresh,
        vault_authority_pda(),
    );
    let r = send(&mut env, ix, &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a config outside the canonical PDA → ConstraintSeeds: Anchor derives the address from the seeds `[b\"config\"]` with the canonical bump: {r:?}"
    );
    assert_target_unchanged(&env, before.withdrawal_authority, "the decoy config");
    assert_eq!(
        env.svm
            .get_account(&forged)
            .expect("the decoy config is in place")
            .data,
        forged_before,
        "the rejected instruction did not touch the decoy account either"
    );

    // POSITIVE: the same call on the canonical config passes.
    env.expire_blockhash();
    send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), fresh),
        &admin,
    )
    .expect("the canonical config accepts set_withdrawal_authority");
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        fresh,
        "only the canonical config changed the withdrawal target"
    );
}
