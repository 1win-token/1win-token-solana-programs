//! `set_recipient` (Arch §9.6, signed by Admin).
//!
//! Why the tests are laid out the way they are.
//!
//! 1. This is the second write path into `vesting.recipient` and the only way to fix a
//!    mistake in the recipient. So what is checked is not merely "the field was
//!    updated" but that the path accepts EXACTLY the same kind of address as
//!    `create_vesting`: should an address incompatible with `claim`'s `recipient` slot
//!    reach the state, the activity would become unclaimable - and after TGE
//!    uncancellable as well.
//! 2. The context is not `mut` on the config and creates no accounts, so a
//!    "rejection's trace" here is the `vesting.recipient` field and the activity's
//!    other fields: every negative asserts they are unchanged.
//! 3. The role and structural negatives are placed with a valid recipient: at that
//!    point the correct Admin would pass, so the failure is caused by the guard under
//!    test rather than by the argument.
//! 4. The positive runs in the same config and on the same activity.
//! 5. The role negative with a FOREIGN key checks `has_one` but not the slot's type:
//!    `has_one` compares addresses and requires no signature. So a separate case
//!    covers "the Admin key is correct, only the signature flag was cleared" - without
//!    it, changing the slot's type to `UncheckedAccount` would let an outsider
//!    reassign a bucket's recipient.
//! 6. The mismatch "the `activity_id` argument against the `vesting` slot's address"
//!    is checked with the record of ANOTHER existing activity: the canonical address
//!    of a non-existent activity fails with `AccountNotInitialized` and never touches
//!    the seed binding.
//! 7. The instruction carries no time restriction: the change passes before TGE,
//!    exactly at TGE and after it, including on an activity part of whose reference
//!    amount has already been released. So the three moments and the redirection of
//!    the remainder stand as positives. Alongside them it is checked that the
//!    neighbouring `cancel_vesting` kept its editing window closed once TGE arrived.
//!
//! ===== DELIBERATELY DEFERRED =====
//! - The full role and TGE matrices.
//! - Substituting the `#[event_cpi]` service slots - the substitution matrix.
//! - Changing the recipient BEFORE TGE followed by a claim (the addressee of the very
//!   first payout) (`claim.rs`): here the change stands AFTER TGE and after a
//!   partial payout.

use anchor_lang::error::ErrorCode;
use litesvm::types::TransactionResult;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting::errors::VestingError;
use vesting::events::RecipientUpdated;
use vesting_test_common::vesting_expected::vesting::{
    ECOSYSTEM_FULL_RELEASE_DAY, V2_REFERENCE, V2_VESTED_DAY_151, V5_CLAIMABLE_DAY_541,
    V5_CLAIMED_AFTER_DAY_541,
};
use vesting_test_common::{
    cancel_vesting_ix, close_vesting_ix, config_pda, custom_error_code, decode_event, err,
    event_authority_pda, foreign_program_pda, recipient_ata, set_recipient_ix,
    set_recipient_ix_with_slots, vault_authority_pda, vault_pda, vesting_pda, without_signer,
    ActivitySpec, TestEnv, DEFAULT_TGE_OFFSET,
};

/// The day of the first claim in the architecture's example V5: the recipient does not
/// claim until day 151, and a single call releases what accrued (the TGE share plus
/// five tranches, [`V2_VESTED_DAY_151`]).
const DAY_151: i64 = 151;

/// Sending an instruction with a single signer (which is also the fee payer).
/// We return the native litesvm TransactionResult as is - just like `send_ixs`.
#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// An environment with one activity: the config is created and `activity_id = 0` is
/// set up for `recipients[0]`. It returns the original recipient - every negative
/// asserts immutability against it.
fn env_with_activity() -> (TestEnv, Pubkey) {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let id = env.create_activity_for(&recipient, &ActivitySpec::valid(V2_REFERENCE));
    assert_eq!(id, 0, "the fixture: the first activity gets id 0");
    (env, recipient)
}

/// An environment with one activity on the "Ecosystem" schedule for
/// [`V2_REFERENCE`] belonging to `recipients[0]`, with its vault funded to exactly the
/// reference amount - the cases with real claims run on it. Returns
/// `(env, activity_id, tge_ts)`.
fn env_with_funded_activity() -> (TestEnv, u64, i64) {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let id = env.create_activity_for(&recipient, &ActivitySpec::valid(V2_REFERENCE));
    env.fund_vault(id, V2_REFERENCE);
    (env, id, params.tge_ts)
}

/// The rejection touched neither the recipient nor the activity's other fields.
fn assert_recipient_unchanged(env: &TestEnv, expected: Pubkey, case: &str) {
    let v = env.vesting(0);
    assert_eq!(
        v.recipient, expected,
        "{case}: the recipient was not changed"
    );
    assert_eq!(
        v.reference_amount, V2_REFERENCE,
        "{case}: the reference amount was not changed"
    );
    assert_eq!(v.claimed, 0, "{case}: the payout is untouched");
}

/// The environment's six actors, none of which is the config's Admin. Owner stands in
/// the list on equal terms: only Admin reassigns a recipient.
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

// ===================================================================
// POSITIVE
// ===================================================================

#[test]
fn set_recipient_updates_field_and_emits_event() {
    let (mut env, old_recipient) = env_with_activity();
    let admin = env.actors.admin.insecure_clone();
    let new_recipient = env.actors.recipients[1].pubkey();
    assert_ne!(
        new_recipient, old_recipient,
        "the fixture: the recipient really does change"
    );

    let r = send(
        &mut env,
        set_recipient_ix(admin.pubkey(), 0, new_recipient),
        &admin,
    );
    r.as_ref()
        .expect("admin reassigns the recipient before TGE");

    let v = env.vesting(0);
    assert_eq!(
        v.recipient, new_recipient,
        "the recipient was taken from the new_recipient slot"
    );
    assert_eq!(
        v.reference_amount, V2_REFERENCE,
        "changing the recipient does not touch the reference amount"
    );
    assert_eq!(
        v.claimed, 0,
        "changing the recipient does not touch the payout"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        1,
        "changing the recipient allocates no new id"
    );

    let ev: RecipientUpdated = decode_event(&r).expect("set_recipient emits RecipientUpdated");
    assert_eq!(ev.activity_id, 0, "the event: the activity's id");
    assert_eq!(
        ev.new_recipient, new_recipient,
        "the event carries the value AFTER the change"
    );
}

// ===================================================================
// the new_recipient slot: the type is itself the check
// ===================================================================

#[test]
fn set_recipient_rejects_non_system_new_recipient() {
    let (mut env, old_recipient) = env_with_activity();
    let admin = env.actors.admin.insecure_clone();

    let expected = u32::from(ErrorCode::AccountNotSystemOwned);
    assert_eq!(
        expected, 3011,
        "the nominal anchor code AccountNotSystemOwned (Anchor 1.0.2)"
    );

    let beneficiary = env.actors.beneficiary.pubkey();
    let mint = env.win_mint;
    let token_account = env.create_ata(&beneficiary, &mint);

    // Both write paths must reject the same class of addresses: let `set_recipient`
    // through what `create_vesting` does not, and the prohibition would be bypassed on
    // the second step.
    let substitutions: [(&str, Pubkey); 2] = [
        (
            "Pubkey::default() (the System Program's address)",
            Pubkey::default(),
        ),
        ("a token account", token_account),
    ];
    for (case, new_recipient) in substitutions {
        env.expire_blockhash();
        let r = send(
            &mut env,
            set_recipient_ix(admin.pubkey(), 0, new_recipient),
            &admin,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(expected),
            "{case} in the new_recipient slot → AccountNotSystemOwned during context resolution, before the body: {r:?}"
        );
        assert_recipient_unchanged(&env, old_recipient, case);
    }

    // POSITIVE: an ordinary wallet passes on the same activity.
    env.expire_blockhash();
    let new_recipient = env.actors.recipients[1].pubkey();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), 0, new_recipient),
        &admin,
    )
    .expect("a system owner in the new_recipient slot passes");
    assert_eq!(
        env.vesting(0).recipient,
        new_recipient,
        "the recipient changed only to a permitted address"
    );
}

#[test]
fn set_recipient_rejects_vault_authority_as_new_recipient() {
    let (mut env, old_recipient) = env_with_activity();
    let admin = env.actors.admin.insecure_clone();

    // `vault_authority` is a data-less PDA (owner = the System Program), so the
    // `SystemAccount` type lets it through and the body's gate rejects it. Were the
    // substitution to pass, the claim's payout would go to `vault_authority`'s own ATA -
    // that is, into the program's custody, from which nothing can withdraw it.
    let r = send(
        &mut env,
        set_recipient_ix(admin.pubkey(), 0, vault_authority_pda()),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidAuthority)),
        "new_recipient == vault_authority → InvalidAuthority: the bucket would fall into the program's custody forever: {r:?}"
    );
    assert_recipient_unchanged(&env, old_recipient, "new_recipient == vault_authority");

    // A POSITIVE on the same activity: an ordinary wallet passes.
    env.expire_blockhash();
    let new_recipient = env.actors.recipients[1].pubkey();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), 0, new_recipient),
        &admin,
    )
    .expect("an ordinary wallet in the new_recipient slot passes");
    assert_eq!(
        env.vesting(0).recipient,
        new_recipient,
        "the recipient changed only to a permitted address"
    );
}

#[test]
fn set_recipient_rejects_off_curve_new_recipient() {
    let (mut env, old_recipient) = env_with_activity();
    let admin = env.actors.admin.insecure_clone();

    // The same class on the second write path: a data-less program-derived address
    // passes the slot's type and is stopped by the body's curve check. Letting it
    // through here would bypass the gate `create_vesting` holds.
    let free_id = env.next_activity_id();
    let substitutions: [(&str, Pubkey); 4] = [
        (
            "a foreign program's PDA",
            foreign_program_pda(b"multisig-vault"),
        ),
        (
            "this program's future activity record",
            vesting_pda(free_id),
        ),
        ("this program's future activity vault", vault_pda(free_id)),
        ("the event authority", event_authority_pda()),
    ];
    for (case, new_recipient) in substitutions {
        assert!(
            !new_recipient.is_on_curve(),
            "{case}: the fixture must be off-curve - otherwise the case checks the wrong thing"
        );
        env.expire_blockhash();
        let r = send(
            &mut env,
            set_recipient_ix(admin.pubkey(), 0, new_recipient),
            &admin,
        );
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::PayeeNotOnCurve)),
            "{case} in the new_recipient slot → PayeeNotOnCurve: {r:?}"
        );
        assert_recipient_unchanged(&env, old_recipient, case);
    }

    // A POSITIVE on the same activity: an ordinary wallet passes.
    env.expire_blockhash();
    let new_recipient = env.actors.recipients[1].pubkey();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), 0, new_recipient),
        &admin,
    )
    .expect("an on-curve wallet in the new_recipient slot passes");
    assert_eq!(
        env.vesting(0).recipient,
        new_recipient,
        "the recipient changed only to a permitted address"
    );
}

#[test]
fn set_recipient_rejects_non_canonical_vault_authority_slot() {
    let (mut env, old_recipient) = env_with_activity();
    let admin = env.actors.admin.insecure_clone();
    let new_recipient = env.actors.recipients[1].pubkey();

    // The `vault_authority` slot is bound by the seeds `[b"vault_authority"]` plus the
    // bump from the config: pass an arbitrary address here and the "recipient !=
    // vault_authority" gate would be checked against a decoy key, that is, disabled by
    // the submission.
    let ix = set_recipient_ix_with_slots(
        admin.pubkey(),
        config_pda(),
        vesting_pda(0),
        new_recipient,
        Pubkey::new_unique(),
        0,
    );
    let r = send(&mut env, ix, &admin);
    // The code is pinned by the first execution: ConstraintSeeds (2006) - Anchor derives
    // the slot's address from the seeds and the decoy does not match what was derived.
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a non-canonical address in the vault_authority slot → ConstraintSeeds: {r:?}"
    );
    assert_recipient_unchanged(&env, old_recipient, "a decoy vault_authority");

    // POSITIVE: the canonical slot passes with the same call.
    env.expire_blockhash();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), 0, new_recipient),
        &admin,
    )
    .expect("the canonical vault_authority is accepted");
    assert_eq!(
        env.vesting(0).recipient,
        new_recipient,
        "the recipient was changed"
    );
}

// ===================================================================
// the moments of the call, the role guard, a non-existent activity, the
// config
// ===================================================================

#[test]
fn set_recipient_works_before_at_and_after_tge() {
    let mut env = TestEnv::new();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    let admin = env.actors.admin.insecure_clone();
    let old_recipient = env.actors.recipients[0].pubkey();
    let id = env.create_activity_for(&old_recipient, &ActivitySpec::valid(V2_REFERENCE));

    // Three moments in a row within one environment: time only moves forward, and at each
    // step the recipient is a new one - otherwise "the field already held that value"
    // could not be told apart from "the instruction did nothing".
    let moments: [(&str, i64, Pubkey); 3] = [
        (
            "one second before TGE",
            p.tge_ts - 1,
            env.actors.recipients[1].pubkey(),
        ),
        (
            "exactly at the TGE instant",
            p.tge_ts,
            env.actors.recipients[2].pubkey(),
        ),
        (
            "one second after TGE",
            p.tge_ts + 1,
            env.actors.stranger.pubkey(),
        ),
    ];

    for (case, at, new_recipient) in moments {
        env.warp_to(at);
        env.expire_blockhash();
        let r = send(
            &mut env,
            set_recipient_ix(admin.pubkey(), id, new_recipient),
            &admin,
        );
        r.as_ref()
            .unwrap_or_else(|e| panic!("{case}: changing the recipient passes: {e:?}"));

        let v = env.vesting(id);
        assert_eq!(
            v.recipient, new_recipient,
            "{case}: the activity's recipient changed"
        );
        assert_eq!(
            v.reference_amount, V2_REFERENCE,
            "{case}: the reference amount is untouched"
        );
        assert_eq!(
            v.claimed, 0,
            "{case}: the accounting of what was released is untouched"
        );

        let ev: RecipientUpdated =
            decode_event(&r).unwrap_or_else(|| panic!("{case}: the RecipientUpdated event"));
        assert_eq!(ev.activity_id, id, "{case}: the event: the activity's id");
        assert_eq!(
            ev.new_recipient, new_recipient,
            "{case}: the event carries the value AFTER the change"
        );
    }
}

#[test]
fn set_recipient_after_tge_leaves_cancel_vesting_closed() {
    let mut env = TestEnv::new();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    let admin = env.actors.admin.insecure_clone();
    let owner = env.actors.owner.insecure_clone();
    let recipient = env.actors.recipients[0].pubkey();
    let id = env.create_activity_for(&recipient, &ActivitySpec::valid(V2_REFERENCE));
    // The second activity - the cancellation BEFORE TGE runs on it: without an executed
    // positive, "after TGE the cancellation is closed" would be only half verified.
    let spare_id = env.create_activity_for(&recipient, &ActivitySpec::valid(V2_REFERENCE));

    let withdrawal = env.actors.rent_collector.pubkey();
    let mint = env.win_mint;
    let beneficiary_token = env.create_ata(&withdrawal, &mint);

    let cancel = cancel_vesting_ix(
        owner.pubkey(),
        spare_id,
        beneficiary_token,
        withdrawal,
        mint,
    );
    send(&mut env, cancel, &owner).expect("before TGE the activity is cancelled");
    assert!(
        !env.vesting_exists(spare_id),
        "precondition: the cancellation really works in this config"
    );

    env.warp_to(p.tge_ts + 1);
    env.expire_blockhash();
    let new_recipient = env.actors.recipients[1].pubkey();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), id, new_recipient),
        &admin,
    )
    .expect("after TGE the recipient is reassigned");
    assert_eq!(
        env.vesting(id).recipient,
        new_recipient,
        "the recipient was changed"
    );

    // The neighbouring instruction kept its restriction: once TGE arrives the obligation
    // to the recipient has arisen and the activity may not be cancelled.
    env.expire_blockhash();
    let cancel = cancel_vesting_ix(owner.pubkey(), id, beneficiary_token, withdrawal, mint);
    let r = send(&mut env, cancel, &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::ScheduleStarted)),
        "after TGE cancel_vesting is rejected with ScheduleStarted: {r:?}"
    );
    assert!(
        env.vesting_exists(id) && env.vault_exists(id),
        "the rejection closed neither the activity's record nor its vault"
    );
    assert_eq!(
        env.vesting(id).recipient,
        new_recipient,
        "the cancellation's rejection did not touch the recipient"
    );
}

#[test]
fn set_recipient_rejects_wrong_signers() {
    let (mut env, old_recipient) = env_with_activity();
    let admin = env.actors.admin.insecure_clone();
    let new_recipient = env.actors.recipients[1].pubkey();

    // The most dangerous case in the list is the activity's current recipient: it signs
    // and reassigns the payout to another self. Owner stands next to it: it has no rights
    // over this instruction. The closure puts the key both into the admin slot and into
    // the signers.
    for (label, actor) in wrong_admins(&env) {
        env.expire_blockhash();
        let ix = set_recipient_ix(actor.pubkey(), 0, new_recipient);
        let r = send(&mut env, ix, &actor);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::NotAdmin)),
            "{label} signed set_recipient → NotAdmin was expected: changing the recipient is Admin's exclusive right: {r:?}"
        );
        assert_recipient_unchanged(&env, old_recipient, label);
    }

    // POSITIVE: the correct admin passes with the same call.
    env.expire_blockhash();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), 0, new_recipient),
        &admin,
    )
    .expect("the correct admin passes");
    assert_eq!(
        env.vesting(0).recipient,
        new_recipient,
        "only Admin changed the recipient"
    );
}

#[test]
fn set_recipient_rejects_unsigned_admin_slot() {
    let (mut env, old_recipient) = env_with_activity();
    let admin = env.actors.admin.insecure_clone();
    let stranger = env.actors.stranger.insecure_clone();
    let new_recipient = env.actors.recipients[1].pubkey();

    let expected = u32::from(ErrorCode::AccountNotSigner);
    assert_eq!(
        expected, 3010,
        "the nominal anchor code AccountNotSigner (Anchor 1.0.2)"
    );

    // The key in the `admin` slot is CORRECT and only the signature flag was cleared.
    // `has_one = admin` compares addresses and would let such a call through - it is the
    // `Signer<'info>` type that rejects it. The fee is paid by an outsider: were admin to
    // sign the transaction itself, the slot would get its signature back.
    //
    // The case is load-bearing: should anyone change the slot's type to
    // `UncheckedAccount`, any wallet could reassign a bucket's recipient to itself before
    // TGE - and every other negative in the file would stay green, since they all submit
    // a FOREIGN key.
    let ix = without_signer(
        set_recipient_ix(admin.pubkey(), 0, new_recipient),
        &admin.pubkey(),
    );
    let r = env.send_ixs(&[ix], &stranger.pubkey(), &[&stranger]);
    assert_eq!(
        custom_error_code(&r),
        Some(expected),
        "admin's signature was cleared → AccountNotSigner during account resolution, before the body: {r:?}"
    );
    assert_recipient_unchanged(&env, old_recipient, "admin's signature was cleared");

    // A POSITIVE in the same environment: only the signature flag differs.
    env.expire_blockhash();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), 0, new_recipient),
        &admin,
    )
    .expect("the same call with admin's signature passes");
    assert_eq!(
        env.vesting(0).recipient,
        new_recipient,
        "the recipient was changed by the signed call"
    );
}

#[test]
fn set_recipient_rejects_unknown_activity() {
    let (mut env, old_recipient) = env_with_activity();
    let admin = env.actors.admin.insecure_clone();
    let new_recipient = env.actors.recipients[1].pubkey();

    let unknown_id = 7;
    assert!(
        !env.vesting_exists(unknown_id),
        "the fixture: nobody created an activity with that id"
    );
    let expected = u32::from(ErrorCode::AccountNotInitialized);
    assert_eq!(
        expected, 3012,
        "the nominal anchor code AccountNotInitialized (Anchor 1.0.2)"
    );

    // The address is derived by the canonical builder - it exists as a point on the
    // seeds' curve, but there is no account at it. The instruction must fail during
    // resolution rather than create a record "along the way" (creation exists only in
    // `create_vesting`, and the id is not an argument there).
    let r = send(
        &mut env,
        set_recipient_ix(admin.pubkey(), unknown_id, new_recipient),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(expected),
        "a non-existent activity → AccountNotInitialized: {r:?}"
    );
    assert!(
        !env.vesting_exists(unknown_id),
        "the rejection created no record at the non-existent activity's address"
    );
    assert_recipient_unchanged(&env, old_recipient, "a non-existent activity");

    // POSITIVE: an existing id passes in the same environment.
    env.expire_blockhash();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), 0, new_recipient),
        &admin,
    )
    .expect("an existing activity accepts the recipient change");
    assert_eq!(
        env.vesting(0).recipient,
        new_recipient,
        "the recipient was changed"
    );
}

#[test]
fn set_recipient_rejects_record_of_another_activity() {
    let (mut env, recipient_a) = env_with_activity();
    let admin = env.actors.admin.insecure_clone();
    let recipient_b = env.actors.recipients[1].pubkey();
    let id_b = env.create_activity_for(&recipient_b, &ActivitySpec::valid(V2_REFERENCE));
    assert_ne!(id_b, 0, "the fixture: the second activity got its own id");
    let new_recipient = env.actors.recipients[2].pubkey();

    // The argument is activity 0 while the `vesting` slot holds activity B's record. The
    // argument goes into the slot's seeds, so the mismatch must fail: were it to pass,
    // Owner would be reassigning the recipient of an activity other than the one it
    // named - and the addressee of a FOREIGN bucket would change silently.
    let ix = set_recipient_ix_with_slots(
        admin.pubkey(),
        config_pda(),
        vesting_pda(id_b),
        new_recipient,
        vault_authority_pda(),
        0,
    );
    let r = send(&mut env, ix, &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "another activity's record with the argument 0 → ConstraintSeeds: the record's address is derived from the activity_id argument: {r:?}"
    );
    assert_recipient_unchanged(&env, recipient_a, "another activity's record");
    assert_eq!(
        env.vesting(id_b).recipient,
        recipient_b,
        "the substituted activity B's recipient is untouched as well"
    );

    // A POSITIVE in the same environment: a consistent "argument + record" pair
    // passes - the rejection was about the mismatch rather than about the address or the
    // role.
    env.expire_blockhash();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), id_b, new_recipient),
        &admin,
    )
    .expect("a consistent \"argument + record\" pair passes");
    assert_eq!(
        env.vesting(id_b).recipient,
        new_recipient,
        "the recipient changed for the activity the argument named"
    );
    assert_eq!(
        env.vesting(0).recipient,
        recipient_a,
        "activity 0 kept its own recipient"
    );
}

#[test]
fn set_recipient_rejects_forged_config() {
    let (mut env, old_recipient) = env_with_activity();
    let admin = env.actors.admin.insecure_clone();
    let new_recipient = env.actors.recipients[1].pubkey();

    // A forge: a program-owned account with a genuine discriminator and a
    // copy of a live config's fields at a random address; there is no honest way to
    // obtain one - `initialize` only ever creates the config at the PDA of the seeds
    // `[b"config"]`. Were the substitution to pass, the TGE gate and the role guard would
    // be checked against a foreign config, that is, disabled together.
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

    let ix = set_recipient_ix_with_slots(
        admin.pubkey(),
        forged,
        vesting_pda(0),
        new_recipient,
        vault_authority_pda(),
        0,
    );
    let r = send(&mut env, ix, &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a config outside the canonical PDA → ConstraintSeeds: Anchor derives the address from the seeds `[b\"config\"]` with the canonical bump: {r:?}"
    );
    assert_recipient_unchanged(&env, old_recipient, "the decoy config");
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
        set_recipient_ix(admin.pubkey(), 0, new_recipient),
        &admin,
    )
    .expect("the canonical config accepts set_recipient");
    assert_eq!(
        env.vesting(0).recipient,
        new_recipient,
        "only the canonical config changed the recipient"
    );
}

// ===================================================================
// changing the recipient after TGE: the addressee of the undelivered
// remainder
// ===================================================================

#[test]
fn set_recipient_after_partial_claim_redirects_the_remainder() {
    let (mut env, id, tge_ts) = env_with_funded_activity();
    let admin = env.actors.admin.insecure_clone();
    let old = env.actors.recipients[0].insecure_clone();
    let new = env.actors.recipients[1].insecure_clone();

    // The first payout goes to the original recipient - day 151 of the architecture's
    // example V5.
    env.claim_at_day(tge_ts, DAY_151, id, &old)
        .expect("the original recipient claims what accrued by day 151");
    assert_eq!(
        env.vesting(id).claimed,
        V2_VESTED_DAY_151,
        "precondition: part of the reference amount was released and the activity is not fully drawn down"
    );

    env.expire_blockhash();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), id, new.pubkey()),
        &admin,
    )
    .expect("after TGE and a partial payout the recipient is reassigned");
    let v = env.vesting(id);
    assert_eq!(
        v.recipient,
        new.pubkey(),
        "the activity's recipient changed"
    );
    assert_eq!(
        v.claimed, V2_VESTED_DAY_151,
        "the accounting of what was released continues from its previous value rather than being zeroed"
    );

    // The schedule's terminal day: the undelivered remainder goes to the NEW wallet.
    env.claim_at_day(tge_ts, ECOSYSTEM_FULL_RELEASE_DAY, id, &new)
        .expect("the new recipient claims the remainder");

    assert_eq!(
        env.recipient_ata_balance(&new.pubkey()),
        V5_CLAIMABLE_DAY_541,
        "the new wallet was paid exactly the undelivered remainder"
    );
    assert_eq!(
        env.recipient_ata_balance(&old.pubkey()),
        V2_VESTED_DAY_151,
        "since the change the previous wallet received not a single unit"
    );
    assert_eq!(
        env.vesting(id).claimed,
        V5_CLAIMED_AFTER_DAY_541,
        "the released counter reached the reference amount, adding both payouts"
    );
    // The total released for the activity does not exceed its reference amount: the
    // addends are independent literals from the reference crate and the ceiling is a
    // field of the record itself.
    assert_eq!(
        env.recipient_ata_balance(&old.pubkey()) + env.recipient_ata_balance(&new.pubkey()),
        env.vesting(id).reference_amount,
        "the two wallets together received exactly the reference amount - not a unit more"
    );
    assert_eq!(env.vault_balance(id), 0, "the vault emptied out exactly");

    // The previous wallet also lost the right to claim: it follows the record's field.
    env.expire_blockhash();
    let r = env.claim_as(&old, id, new.pubkey());
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotAuthorized)),
        "the previous recipient no longer has the right to claim the activity: {r:?}"
    );
}

#[test]
fn set_recipient_on_fully_claimed_activity_passes_and_leaves_close_working() {
    let (mut env, id, tge_ts) = env_with_funded_activity();
    let admin = env.actors.admin.insecure_clone();
    let owner = env.actors.owner.insecure_clone();
    let old = env.actors.recipients[0].insecure_clone();
    let new = env.actors.recipients[1].insecure_clone();
    let withdrawal = env.actors.rent_collector.pubkey();
    let mint = env.win_mint;
    let beneficiary_token = env.create_ata(&withdrawal, &mint);

    env.claim_at_day(tge_ts, ECOSYSTEM_FULL_RELEASE_DAY, id, &old)
        .expect("the terminal claim releases the whole reference amount");
    assert_eq!(
        env.vesting(id).claimed,
        V2_REFERENCE,
        "precondition: the activity is fully drawn down"
    );

    // Changing the recipient on a drawn-down activity is not forbidden - but there is
    // nothing left to release to the new wallet.
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_recipient_ix(admin.pubkey(), id, new.pubkey()),
        &admin,
    );
    r.as_ref()
        .expect("changing the recipient on a fully drawn activity passes");
    assert_eq!(
        env.vesting(id).recipient,
        new.pubkey(),
        "the recipient was changed"
    );
    let ev: RecipientUpdated = decode_event(&r).expect("the RecipientUpdated event");
    assert_eq!(
        ev.new_recipient,
        new.pubkey(),
        "the event carries the new recipient"
    );

    env.expire_blockhash();
    let r = env.claim_as(&new, id, new.pubkey());
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NothingToClaim)),
        "there is nothing to release to the new recipient - the reference amount was released in full: {r:?}"
    );
    assert!(
        !env.account_exists(&recipient_ata(&new.pubkey(), &mint)),
        "the rejection left the new recipient not even a $1WIN account"
    );
    assert_eq!(
        env.recipient_ata_balance(&old.pubkey()),
        V2_REFERENCE,
        "what was released to the previous recipient stayed with it"
    );

    // Changing the recipient does not affect closing the activity: a fully drawn one can
    // be closed, and the addressee of the remainder and the rent comes from the config.
    env.expire_blockhash();
    let close = close_vesting_ix(owner.pubkey(), id, beneficiary_token, withdrawal, mint);
    send(&mut env, close, &owner)
        .expect("the drawn-down activity closes after the recipient change");
    assert!(
        !env.vesting_exists(id) && !env.vault_exists(id),
        "both of the activity's accounts are closed"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        0,
        "the vault was empty - the withdrawal target got zero"
    );
}
