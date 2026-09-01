//! Instructions over the config: `set_tge` under Admin (Arch §9.2), `set_admin` and
//! `set_operator` under Owner (§9.3), and the two-step ownership handover
//! `propose_owner` / `cancel_owner_proposal` / `accept_owner` (§9.4).
//!
//! Why the tests are laid out the way they are.
//!
//! 1. `set_admin`, `set_operator`, `propose_owner` and `cancel_owner_proposal` share
//!    one role guard - `has_one = owner @ NotOwner` on the config - and `set_tge` has
//!    `has_one = admin @ NotAdmin`. Anchor checks both during account RESOLUTION,
//!    before the handler's body, so the role negatives are placed BEFORE TGE and with
//!    valid arguments: at that point the correct signer would pass, so the failure is
//!    precisely a role one rather than temporal or argument-related. Owner stands
//!    among `set_tge`'s wrong signers on equal terms: only Admin moves the date. For
//!    `accept_owner` the signer is a different one - the nominated candidate - and the
//!    body checks the match against `config.pending_owner` (`NotOwner`).
//! 2. Only `set_tge` has a time gate, and it reads the OLD `config.tge_ts` value. So
//!    the "now == tge" and "now == tge + 1" cases first move the date with a positive
//!    call and only then warp the Clock onto it: `warp_to` only moves forward
//!    and the reverse order would be unreachable.
//! 3. The boundaries are checked in pairs to the second: `now` is rejected while
//!    `now + 1` is accepted; `now + MAX_TGE_DELAY + 1` is rejected while
//!    `now + MAX_TGE_DELAY` is accepted.
//! 4. Every negative carries an assertion that the config is unchanged (a rejection
//!    must leave no partial write) plus a positive in THE SAME config.
//! 5. The decoy config proves the singleton's binding is structural: Anchor
//!    derives the address from the seeds rather than a runtime check in the body
//!    verifying it. All six instructions over the config are swept: there are three
//!    contexts (`SetTge`, `OwnerUpdate`, `AcceptOwner`) with independent attributes.
//! 6. A separate case runs AFTER TGE has arrived: the role instructions are not gated
//!    by time (Arch §9.3, §9.4), and without it every check in this file would stand
//!    before TGE - a superfluous time gate in `set_operator` or in the ownership
//!    handover would be caught by none of them.
//!
//! ===== DELIBERATELY DEFERRED =====
//! - The full 12×3 TGE matrix (every instruction against three points in time):
//!   here three instructions are swept after TGE.
//! - The full 12×5 role matrix (all twelve instructions against five wrong actors):
//!   here three instructions are swept.
//! - `MathOverflow` at the `now + MAX_TGE_DELAY` ceiling in `set_tge`: it requires
//!   `now` within 365 days of `i64::MAX` and is reachable only by forging the Clock
//! directly - the substitution matrix.
//! - Substituting the `event_authority` / `program` service slots of the
//! `#[event_cpi]` context.
//! - A multisig as the real Owner signer: the tests use a single key, as does the Sale
//!   suite.

use anchor_lang::error::ErrorCode;
use litesvm::types::TransactionResult;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting::errors::VestingError;
use vesting::events::{
    AdminUpdated, OperatorUpdated, OwnerProposalCancelled, OwnerProposed, OwnerUpdated, TgeUpdated,
};
use vesting::state::MAX_TGE_DELAY;
use vesting_test_common::vesting_expected::vesting::MONTH;
use vesting_test_common::{
    accept_owner_ix, accept_owner_ix_with_config, cancel_owner_proposal_ix,
    cancel_owner_proposal_ix_with_config, config_pda, custom_error_code, decode_event, err,
    propose_owner_ix, propose_owner_ix_with_config, set_admin_ix, set_admin_ix_with_config,
    set_operator_ix, set_operator_ix_with_config, set_tge_ix, set_tge_ix_with_config,
    without_signer, TestEnv, DEFAULT_TGE_OFFSET,
};

// We return the native litesvm TransactionResult as is - just like `send_ixs`.
#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// The environment's six actors, none of which is the config's Owner: admin,
/// operator and the activity participants.
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

/// The environment's six actors, none of which is the config's Admin. Owner stands
/// here on equal terms: only Admin edits the TGE date and the recipient.
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

/// Sweeps every wrong actor in a loop. The closure puts the given pubkey into
/// the signer slot's meta AND signs with it - otherwise the transaction would fail on
/// the missing signature rather than on the role guard, and the test would be checking
/// the wrong thing.
fn assert_signers_rejected<F>(
    env: &mut TestEnv,
    wrong: Vec<(&'static str, Keypair)>,
    expected: VestingError,
    build_ix: F,
) where
    F: Fn(Pubkey) -> Instruction,
{
    for (label, actor) in wrong {
        env.expire_blockhash();
        let r = send(env, build_ix(actor.pubkey()), &actor);
        assert_eq!(
            custom_error_code(&r),
            Some(err(expected)),
            "{label} signed a privileged instruction → {expected:?} was expected: {r:?}"
        );
    }
}

/// [`assert_signers_rejected`] for the Owner instructions: the wrong signers are
/// [`wrong_owners`] and the expectation is `NotOwner`.
fn assert_wrong_signers_rejected<F>(env: &mut TestEnv, build_ix: F)
where
    F: Fn(Pubkey) -> Instruction,
{
    let wrong = wrong_owners(env);
    assert_signers_rejected(env, wrong, VestingError::NotOwner, build_ix);
}

/// [`assert_signers_rejected`] for the Admin instructions: the wrong signers are
/// [`wrong_admins`] and the expectation is `NotAdmin`.
fn assert_wrong_admins_rejected<F>(env: &mut TestEnv, build_ix: F)
where
    F: Fn(Pubkey) -> Instruction,
{
    let wrong = wrong_admins(env);
    assert_signers_rejected(env, wrong, VestingError::NotAdmin, build_ix);
}

// ===================================================================
// set_tge: the arrived-TGE gate and the new value's boundaries
// ===================================================================

#[test]
fn set_tge_moves_date_one_second_before_tge() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);

    // The last second at which the date is still editable: now == tge − 1.
    env.warp_to(p.tge_ts - 1);
    let moved = p.tge_ts + MONTH;

    let r = send(&mut env, set_tge_ix(admin.pubkey(), moved), &admin);
    r.as_ref()
        .expect("one second before TGE the date still moves");
    assert_eq!(
        env.vesting_config().tge_ts,
        moved,
        "the new value was written into the config"
    );

    let ev: TgeUpdated = decode_event(&r).expect("set_tge emits TgeUpdated");
    assert_eq!(
        ev.tge_ts, moved,
        "the event carries the value AFTER the change, not the previous one"
    );
}

#[test]
fn set_tge_rejects_when_now_equals_tge() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);

    // A POSITIVE before the gate: while now < tge the date moves (and at the same time
    // it sets the point to warp to - warp_to only moves forward).
    env.warp_to(p.tge_ts - 1);
    let moved = p.tge_ts + MONTH;
    send(&mut env, set_tge_ix(admin.pubkey(), moved), &admin).expect("before TGE the date moves");

    // now == tge: the TGE instant has arrived and the schedule is final.
    env.warp_to(moved);
    env.expire_blockhash();
    let r = send(&mut env, set_tge_ix(admin.pubkey(), moved + MONTH), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::ScheduleStarted)),
        "now == tge → ScheduleStarted (the gate is inclusive): {r:?}"
    );
    assert_eq!(
        env.vesting_config().tge_ts,
        moved,
        "the rejection did not move the TGE date"
    );
}

#[test]
fn set_tge_rejects_one_second_after_tge() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);

    env.warp_to(p.tge_ts - 1);
    let moved = p.tge_ts + MONTH;
    send(&mut env, set_tge_ix(admin.pubkey(), moved), &admin).expect("before TGE the date moves");

    env.warp_to(moved + 1);
    env.expire_blockhash();
    let r = send(&mut env, set_tge_ix(admin.pubkey(), moved + MONTH), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::ScheduleStarted)),
        "now == tge + 1 → ScheduleStarted: {r:?}"
    );
    assert_eq!(
        env.vesting_config().tge_ts,
        moved,
        "the rejection did not move the TGE date"
    );
}

#[test]
fn set_tge_rejects_new_value_at_now() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    let now = env.now();

    // Exactly `now` - the upper edge of the invalid range (the condition is `tge_ts > now`).
    let r = send(&mut env, set_tge_ix(admin.pubkey(), now), &admin);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidTge)),
        "a new tge_ts == now → InvalidTge: {r:?}"
    );
    assert_eq!(
        env.vesting_config().tge_ts,
        p.tge_ts,
        "the rejection did not touch the TGE date"
    );

    // A POSITIVE one second over: now + 1 is the minimum permitted value.
    env.expire_blockhash();
    send(&mut env, set_tge_ix(admin.pubkey(), now + 1), &admin)
        .expect("now + 1 is the minimum permitted value");
    assert_eq!(
        env.vesting_config().tge_ts,
        now + 1,
        "the boundary to the second: now was rejected and now + 1 accepted"
    );
}

#[test]
fn set_tge_rejects_new_value_beyond_max_delay() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    let now = env.now();

    let r = send(
        &mut env,
        set_tge_ix(admin.pubkey(), now + MAX_TGE_DELAY + 1),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidTge)),
        "a new tge_ts == now + MAX_TGE_DELAY + 1 → InvalidTge: {r:?}"
    );
    assert_eq!(
        env.vesting_config().tge_ts,
        p.tge_ts,
        "the rejection did not touch the TGE date"
    );

    // A POSITIVE: after the rejection the config remained editable.
    env.expire_blockhash();
    send(&mut env, set_tge_ix(admin.pubkey(), now + 1), &admin)
        .expect("after a rejection on the ceiling the config stays editable");
    assert_eq!(
        env.vesting_config().tge_ts,
        now + 1,
        "the date did move after all"
    );
}

#[test]
fn set_tge_accepts_new_value_exactly_at_max_delay() {
    // The ceiling is inclusive: `tge_ts <= now + MAX_TGE_DELAY`. The counterpart to the test above.
    assert_eq!(
        MAX_TGE_DELAY,
        365 * 86_400,
        "MAX_TGE_DELAY is fixed by agreement (365 days); changing it moves both boundaries"
    );

    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default(DEFAULT_TGE_OFFSET);
    let ceiling = env.now() + MAX_TGE_DELAY;

    let r = send(&mut env, set_tge_ix(admin.pubkey(), ceiling), &admin);
    r.as_ref()
        .expect("a value exactly on the ceiling is accepted");
    assert_eq!(
        env.vesting_config().tge_ts,
        ceiling,
        "the boundary value was written unaltered"
    );

    let ev: TgeUpdated = decode_event(&r).expect("set_tge emits TgeUpdated");
    assert_eq!(ev.tge_ts, ceiling, "the event carries the boundary value");
}

// ===================================================================
// set_admin: the positive, a zero key, overlap with the other two roles, a foreign
// signer and rotating the role
// ===================================================================

#[test]
fn set_admin_updates_field_and_emits_event() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let fresh = Pubkey::new_unique();

    let r = send(&mut env, set_admin_ix(owner.pubkey(), fresh), &owner);
    r.as_ref().expect("owner: set_admin passes");

    let c = env.vesting_config();
    assert_eq!(c.admin, fresh, "admin was updated");
    assert_eq!(
        c.owner, before.owner,
        "owner is untouched by the admin change"
    );
    assert_eq!(
        c.operator, before.operator,
        "operator is untouched by the admin change"
    );

    let ev: AdminUpdated = decode_event(&r).expect("set_admin emits AdminUpdated");
    assert_eq!(
        ev.admin, fresh,
        "the event carries the value after the change"
    );
}

#[test]
fn set_admin_rejects_default_pubkey() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);

    let r = send(
        &mut env,
        set_admin_ix(owner.pubkey(), Pubkey::default()),
        &owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidAuthority)),
        "a zero admin → InvalidAuthority (a role nobody can sign with): {r:?}"
    );
    assert_eq!(
        env.vesting_config().admin,
        before.admin,
        "the rejection did not touch admin"
    );

    env.expire_blockhash();
    let fresh = Pubkey::new_unique();
    send(&mut env, set_admin_ix(owner.pubkey(), fresh), &owner)
        .expect("a non-zero admin passes in the same config");
    assert_eq!(env.vesting_config().admin, fresh, "admin was updated");
}

#[test]
fn set_admin_accepts_current_owner_and_operator() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);

    send(&mut env, set_admin_ix(owner.pubkey(), before.owner), &owner)
        .expect("admin == owner passes");
    assert_eq!(
        env.vesting_config().admin,
        before.owner,
        "admin became owner"
    );

    env.expire_blockhash();
    send(
        &mut env,
        set_admin_ix(owner.pubkey(), before.operator),
        &owner,
    )
    .expect("admin == operator passes");
    let c = env.vesting_config();
    assert_eq!(c.admin, before.operator, "admin became operator");
    assert_eq!(c.operator, before.operator, "operator is untouched");
}

#[test]
fn set_admin_rejects_wrong_signers() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let target = Pubkey::new_unique();

    // The most important in the list is the sitting admin: it has no right to reassign
    // itself, and only Owner hands out the role.
    assert_wrong_signers_rejected(&mut env, |a| set_admin_ix(a, target));
    assert_eq!(
        env.vesting_config().admin,
        before.admin,
        "no foreign signer changed admin"
    );

    env.expire_blockhash();
    send(&mut env, set_admin_ix(owner.pubkey(), target), &owner)
        .expect("the correct owner passes with the same call");
    assert_eq!(env.vesting_config().admin, target, "owner changed admin");
}

#[test]
fn admin_rotation_revokes_old_admin_on_set_tge() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let old_admin = env.actors.admin.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    let new_admin = env.new_wallet();

    send(
        &mut env,
        set_admin_ix(owner.pubkey(), new_admin.pubkey()),
        &owner,
    )
    .expect("owner reassigns admin");

    // The previous key loses the right to move the date immediately after the rotation.
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_tge_ix(old_admin.pubkey(), p.tge_ts + MONTH),
        &old_admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotAdmin)),
        "the previous admin after the rotation → NotAdmin: {r:?}"
    );
    assert_eq!(
        env.vesting_config().tge_ts,
        p.tge_ts,
        "the previous admin did not move the TGE date"
    );

    // A POSITIVE: the new key passes with the same instruction.
    env.expire_blockhash();
    let moved = p.tge_ts + MONTH;
    send(&mut env, set_tge_ix(new_admin.pubkey(), moved), &new_admin)
        .expect("the new admin moves the TGE date");
    assert_eq!(
        env.vesting_config().tge_ts,
        moved,
        "it was the new admin that moved the date"
    );
}

// ===================================================================
// set_operator and the ownership handover: the positive, a zero key, a missing
// nomination and acceptance by the wrong key
// ===================================================================

#[test]
fn set_operator_updates_field_and_emits_event() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let fresh = Pubkey::new_unique();

    let r = send(&mut env, set_operator_ix(owner.pubkey(), fresh), &owner);
    r.as_ref().expect("owner: set_operator passes");

    let c = env.vesting_config();
    assert_eq!(c.operator, fresh, "operator was updated");
    assert_eq!(
        c.owner, before.owner,
        "owner is untouched by the operator change"
    );

    let ev: OperatorUpdated = decode_event(&r).expect("set_operator emits OperatorUpdated");
    assert_eq!(
        ev.operator, fresh,
        "the event carries the value after the change"
    );
}

#[test]
fn set_operator_rejects_default_pubkey() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);

    let r = send(
        &mut env,
        set_operator_ix(owner.pubkey(), Pubkey::default()),
        &owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidAuthority)),
        "a zero operator → InvalidAuthority (a role nobody can sign with): {r:?}"
    );
    assert_eq!(
        env.vesting_config().operator,
        before.operator,
        "the rejection did not touch operator"
    );

    env.expire_blockhash();
    let fresh = Pubkey::new_unique();
    send(&mut env, set_operator_ix(owner.pubkey(), fresh), &owner)
        .expect("a non-zero operator passes in the same config");
    assert_eq!(env.vesting_config().operator, fresh, "operator was updated");
}

/// The sitting owner's key and the sitting admin's key are accepted as operator.
#[test]
fn set_operator_accepts_current_owner_and_admin() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);

    send(
        &mut env,
        set_operator_ix(owner.pubkey(), before.owner),
        &owner,
    )
    .expect("operator == owner passes");
    assert_eq!(
        env.vesting_config().operator,
        before.owner,
        "operator became owner"
    );

    env.expire_blockhash();
    send(
        &mut env,
        set_operator_ix(owner.pubkey(), before.admin),
        &owner,
    )
    .expect("operator == admin passes");
    let c = env.vesting_config();
    assert_eq!(c.operator, before.admin, "operator became admin");
    assert_eq!(c.admin, before.admin, "admin is untouched");
}

#[test]
fn owner_handover_transfers_role_in_two_steps() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let candidate = env.new_wallet();

    let r = send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    );
    r.as_ref().expect("owner: propose_owner passes");

    let c = env.vesting_config();
    assert_eq!(
        c.pending_owner,
        Some(candidate.pubkey()),
        "the candidate was recorded"
    );
    assert_eq!(
        c.owner, before.owner,
        "until the candidate accepts, ownership stays with the sitting owner"
    );

    let ev: OwnerProposed = decode_event(&r).expect("propose_owner emits OwnerProposed");
    assert_eq!(
        ev.pending_owner,
        candidate.pubkey(),
        "the event carries the candidate"
    );

    env.expire_blockhash();
    let r = send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate);
    r.as_ref().expect("the candidate: accept_owner passes");

    let c = env.vesting_config();
    assert_eq!(
        c.owner,
        candidate.pubkey(),
        "ownership was handed to the candidate"
    );
    assert_eq!(
        c.pending_owner, None,
        "the candidate was cleared by the acceptance"
    );
    assert_eq!(
        c.operator, before.operator,
        "operator is untouched by the owner change - a rotation does not touch the second role"
    );

    let ev: OwnerUpdated = decode_event(&r).expect("accept_owner emits OwnerUpdated");
    assert_eq!(
        ev.new_owner,
        candidate.pubkey(),
        "the event carries the value after the change"
    );

    // The rights moved in full: the previous key is rejected and the new one governs the
    // config.
    env.expire_blockhash();
    let r = send(
        &mut env,
        set_operator_ix(owner.pubkey(), Pubkey::new_unique()),
        &owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotOwner)),
        "the previous owner after the handover → NotOwner: {r:?}"
    );

    env.expire_blockhash();
    let fresh_operator = Pubkey::new_unique();
    send(
        &mut env,
        set_operator_ix(candidate.pubkey(), fresh_operator),
        &candidate,
    )
    .expect("the new owner: set_operator passes");
    assert_eq!(
        env.vesting_config().operator,
        fresh_operator,
        "the new owner really did change operator"
    );
}

#[test]
fn accept_owner_rejects_when_no_candidate_proposed() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let candidate = env.new_wallet();

    assert_eq!(
        env.vesting_config().pending_owner,
        None,
        "precondition: right after initialize there is no candidate"
    );
    let r = send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NoPendingOwner)),
        "acceptance with no nomination → NoPendingOwner: {r:?}"
    );
    assert_eq!(
        env.vesting_config().owner,
        before.owner,
        "the rejection did not touch owner"
    );

    // A POSITIVE in the same config: with a nomination the same candidate accepts.
    env.expire_blockhash();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("owner nominates a candidate");
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate)
        .expect("after the nomination the acceptance passes");
    assert_eq!(
        env.vesting_config().owner,
        candidate.pubkey(),
        "ownership was handed over"
    );
}

#[test]
fn accept_owner_rejects_signer_other_than_candidate() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let stranger = env.actors.stranger.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let candidate = env.new_wallet();

    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("owner nominates a candidate");

    // The sitting owner matters most in this list: only the nominated key has the right
    // to accept, and "whoever nominated" does not have it.
    let wrong: [(&str, &Keypair); 3] = [
        ("the previous owner", &owner),
        ("operator", &operator),
        ("an outsider", &stranger),
    ];
    for (label, actor) in wrong {
        env.expire_blockhash();
        let r = send(&mut env, accept_owner_ix(actor.pubkey()), actor);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::NotOwner)),
            "{label} signed accept_owner → NotOwner: {r:?}"
        );
        let c = env.vesting_config();
        assert_eq!(
            c.owner, before.owner,
            "{label}: the rejection did not touch owner"
        );
        assert_eq!(
            c.pending_owner,
            Some(candidate.pubkey()),
            "{label}: the rejection did not clear the candidate"
        );
    }

    // A POSITIVE in the same config: the nominated candidate accepts.
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate)
        .expect("the nominated candidate accepts");
    assert_eq!(
        env.vesting_config().owner,
        candidate.pubkey(),
        "ownership was handed over"
    );
}

#[test]
fn propose_owner_rejects_default_pubkey() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default(DEFAULT_TGE_OFFSET);

    let r = send(
        &mut env,
        propose_owner_ix(owner.pubkey(), Pubkey::default()),
        &owner,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InvalidAuthority)),
        "a zero candidate → InvalidAuthority (there is nothing to accept with): {r:?}"
    );
    assert_eq!(
        env.vesting_config().pending_owner,
        None,
        "the rejection recorded no candidate"
    );

    env.expire_blockhash();
    let fresh = Pubkey::new_unique();
    send(&mut env, propose_owner_ix(owner.pubkey(), fresh), &owner)
        .expect("a non-zero candidate passes in the same config");
    assert_eq!(
        env.vesting_config().pending_owner,
        Some(fresh),
        "the candidate was recorded"
    );
}

/// The sitting admin and the sitting operator are accepted as owner candidates.
#[test]
fn propose_owner_accepts_current_admin_and_operator() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);

    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), before.operator),
        &owner,
    )
    .expect("a candidate == operator passes");
    assert_eq!(
        env.vesting_config().pending_owner,
        Some(before.operator),
        "operator was recorded as the candidate"
    );

    env.expire_blockhash();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), before.admin),
        &owner,
    )
    .expect("a candidate == admin passes");
    assert_eq!(
        env.vesting_config().pending_owner,
        Some(before.admin),
        "admin was recorded as the candidate"
    );
}

/// A candidate that became admin between the nomination and the acceptance does take
/// ownership: the trio of roles is not checked at acceptance.
#[test]
fn accept_owner_succeeds_for_candidate_who_became_admin() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let candidate = env.new_wallet();

    env.init_default(DEFAULT_TGE_OFFSET);

    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("owner nominates a candidate");

    env.expire_blockhash();
    send(
        &mut env,
        set_admin_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("owner makes the candidate an admin");

    env.expire_blockhash();
    send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate)
        .expect("the candidate accepts while being an admin");

    let c = env.vesting_config();
    assert_eq!(c.owner, candidate.pubkey(), "ownership was handed over");
    assert_eq!(c.admin, candidate.pubkey(), "that same key stayed an admin");
    assert_eq!(
        c.pending_owner, None,
        "the candidate was cleared by the acceptance"
    );
}

#[test]
fn owner_survives_proposal_of_address_without_a_key() {
    // Exactly the case the handover is two-step for: a typo into an address that cannot
    // sign. Nobody can accept such a nomination, ownership stays with the sitting owner,
    // and it nominates a candidate again.
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    // An address with no private key: a PDA of the Vesting program.
    let dead_address = config_pda();

    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), dead_address),
        &owner,
    )
    .expect(
        "the nomination was accepted: the address is valid and the argument gates are satisfied",
    );
    assert_eq!(
        env.vesting_config().owner,
        before.owner,
        "ownership did not move on the nomination alone"
    );

    // The sitting owner still governs the config.
    env.expire_blockhash();
    let fresh_operator = Pubkey::new_unique();
    send(
        &mut env,
        set_operator_ix(owner.pubkey(), fresh_operator),
        &owner,
    )
    .expect("the previous owner did not lose its rights");

    // A repeated nomination overwrites the candidate - with a live key.
    env.expire_blockhash();
    let candidate = env.new_wallet();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("owner nominates a candidate afresh");
    assert_eq!(
        env.vesting_config().pending_owner,
        Some(candidate.pubkey()),
        "the candidate was replaced"
    );

    env.expire_blockhash();
    send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate).expect("a live key accepts");
    assert_eq!(
        env.vesting_config().owner,
        candidate.pubkey(),
        "ownership was handed over on the second attempt"
    );
}

#[test]
fn repeated_proposal_replaces_the_previous_candidate() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default(DEFAULT_TGE_OFFSET);
    let first = env.new_wallet();
    let second = env.new_wallet();

    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), first.pubkey()),
        &owner,
    )
    .expect("the first candidate was nominated");
    env.expire_blockhash();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), second.pubkey()),
        &owner,
    )
    .expect("the second candidate was nominated");
    assert_eq!(
        env.vesting_config().pending_owner,
        Some(second.pubkey()),
        "the config holds exactly one candidate - the last nominated"
    );

    env.expire_blockhash();
    let r = send(&mut env, accept_owner_ix(first.pubkey()), &first);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotOwner)),
        "the previous candidate cannot accept: its nomination was overwritten: {r:?}"
    );

    env.expire_blockhash();
    send(&mut env, accept_owner_ix(second.pubkey()), &second)
        .expect("the standing candidate accepts");
    assert_eq!(
        env.vesting_config().owner,
        second.pubkey(),
        "ownership is with the last nominated candidate"
    );
}

#[test]
fn cancel_owner_proposal_drops_the_candidate() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let candidate = env.new_wallet();

    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("owner nominates a candidate");

    env.expire_blockhash();
    let r = send(&mut env, cancel_owner_proposal_ix(owner.pubkey()), &owner);
    r.as_ref().expect("owner: cancel_owner_proposal passes");
    assert_eq!(
        env.vesting_config().pending_owner,
        None,
        "the candidate was withdrawn"
    );
    decode_event::<OwnerProposalCancelled>(&r)
        .expect("cancel_owner_proposal emits OwnerProposalCancelled");

    // A withdrawn nomination cannot be executed.
    env.expire_blockhash();
    let r = send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NoPendingOwner)),
        "after the withdrawal there is nothing to accept → NoPendingOwner: {r:?}"
    );
    assert_eq!(
        env.vesting_config().owner,
        before.owner,
        "ownership stayed with the sitting owner"
    );

    // A POSITIVE in the same config: a new nomination is executable again.
    env.expire_blockhash();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("owner nominates a candidate afresh");
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate)
        .expect("the candidate accepts after the new nomination");
    assert_eq!(
        env.vesting_config().owner,
        candidate.pubkey(),
        "ownership was handed over"
    );
}

#[test]
fn cancel_owner_proposal_rejects_when_nothing_is_proposed() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default(DEFAULT_TGE_OFFSET);

    assert_eq!(
        env.vesting_config().pending_owner,
        None,
        "precondition: there is no candidate"
    );
    let r = send(&mut env, cancel_owner_proposal_ix(owner.pubkey()), &owner);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NoPendingOwner)),
        "a withdrawal with no nomination → NoPendingOwner rather than a silent success: {r:?}"
    );

    // A POSITIVE in the same config: with a nomination the withdrawal passes.
    env.expire_blockhash();
    let candidate = Pubkey::new_unique();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate),
        &owner,
    )
    .expect("owner nominates a candidate");
    env.expire_blockhash();
    send(&mut env, cancel_owner_proposal_ix(owner.pubkey()), &owner)
        .expect("withdrawing the nominated candidate passes");
    assert_eq!(
        env.vesting_config().pending_owner,
        None,
        "the candidate was withdrawn"
    );
}

// ===================================================================
// the Owner handover: the rights move in full
// ===================================================================

#[test]
fn owner_rotation_revokes_old_owner_on_every_instruction() {
    let mut env = TestEnv::new();
    let old_owner = env.actors.owner.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    let new_owner = env.new_wallet();

    send(
        &mut env,
        propose_owner_ix(old_owner.pubkey(), new_owner.pubkey()),
        &old_owner,
    )
    .expect("the sitting owner nominates a candidate");
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(new_owner.pubkey()), &new_owner)
        .expect("the candidate accepts and takes the role");

    // The old key loses its rights across ALL of the config's Owner instructions at
    // once, not only the one it was revoked with.
    let attempts: [(&str, Instruction); 4] = [
        (
            "set_admin",
            set_admin_ix(old_owner.pubkey(), Pubkey::new_unique()),
        ),
        (
            "set_operator",
            set_operator_ix(old_owner.pubkey(), Pubkey::new_unique()),
        ),
        (
            "propose_owner",
            propose_owner_ix(old_owner.pubkey(), old_owner.pubkey()),
        ),
        (
            "cancel_owner_proposal",
            cancel_owner_proposal_ix(old_owner.pubkey()),
        ),
    ];
    for (label, ix) in attempts {
        env.expire_blockhash();
        let r = send(&mut env, ix, &old_owner);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::NotOwner)),
            "the previous owner invoked {label} after the rotation → NotOwner was expected: {r:?}"
        );
    }

    let c = env.vesting_config();
    assert_eq!(
        c.owner,
        new_owner.pubkey(),
        "the previous owner did not take the role back"
    );
    assert_eq!(
        c.tge_ts, p.tge_ts,
        "the previous owner did not move the TGE date"
    );
    assert_eq!(c.admin, p.admin, "the previous owner did not change admin");
    assert_eq!(
        c.operator, p.operator,
        "the previous owner did not change operator"
    );
    assert_eq!(
        c.pending_owner, None,
        "the previous owner did not nominate itself as the candidate"
    );
}

// ===================================================================
// The roles change after TGE (Arch §9.3, §9.4): they are not subject to time
// ===================================================================

#[test]
fn roles_rotate_after_tge_while_schedule_is_frozen() {
    // Every other case in this file runs BEFORE TGE, so a time gate mistakenly placed on
    // the role instructions would not be caught by them. The price of such a defect is a
    // compromised operator that cannot be revoked after TGE.
    //
    // The mutation check was performed: with `require!(now < config.tge_ts,
    // ScheduleStarted)` in `set_operator` only this test turns red; the gate was
    // removed.
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);

    env.warp_to(p.tge_ts + 1);

    // The contrast at the same point in time: the schedule is already running and the
    // date is immutable even for the Admin it belongs to...
    let r = send(
        &mut env,
        set_tge_ix(admin.pubkey(), p.tge_ts + MONTH),
        &admin,
    );
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::ScheduleStarted)),
        "after TGE the date does not move - otherwise the case below would be checking a different invariant: {r:?}"
    );

    // ...while the roles are reassigned.
    env.expire_blockhash();
    let fresh_admin = Pubkey::new_unique();
    let r = send(&mut env, set_admin_ix(owner.pubkey(), fresh_admin), &owner);
    r.as_ref()
        .expect("after TGE admin rotates: revoking a compromised key has no deadline");
    assert_eq!(
        env.vesting_config().admin,
        fresh_admin,
        "admin was updated after TGE"
    );
    let ev: AdminUpdated = decode_event(&r).expect("set_admin emits AdminUpdated");
    assert_eq!(ev.admin, fresh_admin, "the event: admin after TGE");

    env.expire_blockhash();
    let fresh_operator = Pubkey::new_unique();
    let r = send(
        &mut env,
        set_operator_ix(owner.pubkey(), fresh_operator),
        &owner,
    );
    r.as_ref()
        .expect("after TGE operator rotates: revoking a compromised key has no deadline");
    assert_eq!(
        env.vesting_config().operator,
        fresh_operator,
        "operator was updated after TGE"
    );
    let ev: OperatorUpdated = decode_event(&r).expect("set_operator emits OperatorUpdated");
    assert_eq!(ev.operator, fresh_operator, "the event: operator after TGE");

    env.expire_blockhash();
    let new_owner = env.new_wallet();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), new_owner.pubkey()),
        &owner,
    )
    .expect("after TGE owner nominates a candidate");
    env.expire_blockhash();
    let r = send(&mut env, accept_owner_ix(new_owner.pubkey()), &new_owner);
    r.as_ref().expect("after TGE the candidate accepts");
    let c = env.vesting_config();
    assert_eq!(c.owner, new_owner.pubkey(), "owner was updated after TGE");
    assert_eq!(
        c.pending_owner, None,
        "the candidate was cleared by the acceptance"
    );
    assert_eq!(
        c.operator, fresh_operator,
        "the owner handover did not touch operator"
    );
    assert_eq!(
        c.tge_ts, p.tge_ts,
        "the role rotations did not move the TGE date"
    );
    let ev: OwnerUpdated = decode_event(&r).expect("accept_owner emits OwnerUpdated");
    assert_eq!(
        ev.new_owner,
        new_owner.pubkey(),
        "the event: the new owner after TGE"
    );

    // The role moved in full: the new key governs the config after TGE.
    env.expire_blockhash();
    let second_operator = Pubkey::new_unique();
    send(
        &mut env,
        set_operator_ix(new_owner.pubkey(), second_operator),
        &new_owner,
    )
    .expect("the new owner governs the config after TGE");
    assert_eq!(
        env.vesting_config().operator,
        second_operator,
        "the new owner really did change operator after TGE"
    );
}

// ===================================================================
// a foreign signer and a signature cleared from the owner slot
// ===================================================================

#[test]
fn set_tge_rejects_wrong_signers() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    // Before TGE and with a valid value: for the correct admin the call would pass, so
    // the failure is purely a role one. Owner is among the wrong signers: it has no
    // rights over this instruction.
    let moved = p.tge_ts + MONTH;

    assert_wrong_admins_rejected(&mut env, |a| set_tge_ix(a, moved));
    assert_eq!(
        env.vesting_config().tge_ts,
        p.tge_ts,
        "no foreign signer, Owner included, moved the TGE date"
    );

    env.expire_blockhash();
    send(&mut env, set_tge_ix(admin.pubkey(), moved), &admin)
        .expect("the correct admin passes with the same call");
    assert_eq!(
        env.vesting_config().tge_ts,
        moved,
        "admin moved the TGE date"
    );
}

#[test]
fn set_tge_rejects_unsigned_admin_slot() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let payer = env.payer.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    let moved = p.tge_ts + MONTH;

    let expected = u32::from(ErrorCode::AccountNotSigner);

    // The key in the admin slot is CORRECT and only the signature flag was cleared:
    // has_one would pass and the `Signer<'info>` type rejects it. The fee is paid by an
    // outsider, otherwise admin would sign the transaction as the payer.
    let ix = without_signer(set_tge_ix(admin.pubkey(), moved), &admin.pubkey());
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer]);
    assert_eq!(
        custom_error_code(&r),
        Some(expected),
        "set_tge with admin's signature cleared → AccountNotSigner: {r:?}"
    );
    assert_eq!(
        env.vesting_config().tge_ts,
        p.tge_ts,
        "the unsigned call did not move the TGE date"
    );

    // A POSITIVE: the same call with admin's signature passes.
    env.expire_blockhash();
    send(&mut env, set_tge_ix(admin.pubkey(), moved), &admin).expect("a signed admin passes");
    assert_eq!(env.vesting_config().tge_ts, moved, "the TGE date was moved");
}

#[test]
fn set_operator_rejects_wrong_signers() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let target = Pubkey::new_unique();

    assert_wrong_signers_rejected(&mut env, |a| set_operator_ix(a, target));
    assert_eq!(
        env.vesting_config().operator,
        before.operator,
        "no foreign signer changed operator"
    );

    env.expire_blockhash();
    send(&mut env, set_operator_ix(owner.pubkey(), target), &owner)
        .expect("the correct owner passes with the same call");
    assert_eq!(
        env.vesting_config().operator,
        target,
        "owner changed operator"
    );
}

#[test]
fn propose_owner_rejects_wrong_signers() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let before = env.init_default(DEFAULT_TGE_OFFSET);
    let target = Pubkey::new_unique();

    // The most dangerous case: a foreign signer nominates itself as the candidate.
    assert_wrong_signers_rejected(&mut env, |a| propose_owner_ix(a, a));
    // And the harmless form of the same call - so that the rejection does not depend on
    // the argument.
    assert_wrong_signers_rejected(&mut env, |a| propose_owner_ix(a, target));
    let c = env.vesting_config();
    assert_eq!(
        c.owner, before.owner,
        "no foreign signer hijacked the owner role"
    );
    assert_eq!(
        c.pending_owner, None,
        "no foreign signer queued up for ownership"
    );

    env.expire_blockhash();
    send(&mut env, propose_owner_ix(owner.pubkey(), target), &owner)
        .expect("the correct owner passes with the same call");
    assert_eq!(
        env.vesting_config().pending_owner,
        Some(target),
        "the candidate was recorded"
    );
}

#[test]
fn cancel_owner_proposal_rejects_wrong_signers() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default(DEFAULT_TGE_OFFSET);
    let target = Pubkey::new_unique();

    // A candidate is already nominated: for the correct owner the withdrawal would pass,
    // so the failure is a role one rather than a missing nomination.
    send(&mut env, propose_owner_ix(owner.pubkey(), target), &owner)
        .expect("owner nominates a candidate");

    assert_wrong_signers_rejected(&mut env, cancel_owner_proposal_ix);
    assert_eq!(
        env.vesting_config().pending_owner,
        Some(target),
        "no foreign signer withdrew the candidate"
    );

    env.expire_blockhash();
    send(&mut env, cancel_owner_proposal_ix(owner.pubkey()), &owner)
        .expect("the correct owner passes with the same call");
    assert_eq!(
        env.vesting_config().pending_owner,
        None,
        "the candidate was withdrawn"
    );
}

#[test]
fn owner_instructions_reject_unsigned_owner_slot() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let payer = env.payer.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);

    let expected = u32::from(ErrorCode::AccountNotSigner);
    assert_eq!(
        expected, 3010,
        "the nominal anchor code AccountNotSigner (Anchor 1.0.2)"
    );

    // The key in the owner slot is CORRECT and only the signature flag was cleared:
    // has_one would pass and the `Signer<'info>` type rejects it. The fee payer is an
    // outside payer, otherwise owner would sign the transaction as the payer.
    let unsigned: [(&str, Instruction, Pubkey); 4] = [
        (
            "set_admin",
            set_admin_ix(owner.pubkey(), Pubkey::new_unique()),
            owner.pubkey(),
        ),
        (
            "set_operator",
            set_operator_ix(owner.pubkey(), Pubkey::new_unique()),
            owner.pubkey(),
        ),
        (
            "propose_owner",
            propose_owner_ix(owner.pubkey(), Pubkey::new_unique()),
            owner.pubkey(),
        ),
        (
            "cancel_owner_proposal",
            cancel_owner_proposal_ix(owner.pubkey()),
            owner.pubkey(),
        ),
    ];
    for (label, ix, slot) in unsigned {
        env.expire_blockhash();
        let ix = without_signer(ix, &slot);
        let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer]);
        assert_eq!(
            custom_error_code(&r),
            Some(expected),
            "{label} with owner's signature cleared → AccountNotSigner: {r:?}"
        );
    }

    // `accept_owner` is signed by the candidate: its slot is a different one, so clearing
    // the signature is checked by a separate call with a candidate already nominated.
    let candidate = Pubkey::new_unique();
    env.expire_blockhash();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate),
        &owner,
    )
    .expect("owner nominates a candidate");
    env.expire_blockhash();
    let ix = without_signer(accept_owner_ix(candidate), &candidate);
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer]);
    assert_eq!(
        custom_error_code(&r),
        Some(expected),
        "accept_owner with the candidate's signature cleared → AccountNotSigner: {r:?}"
    );

    let c = env.vesting_config();
    assert_eq!(c.owner, p.owner, "the unsigned calls did not touch owner");
    assert_eq!(c.admin, p.admin, "they did not touch admin");
    assert_eq!(c.operator, p.operator, "they did not touch operator");
    assert_eq!(c.tge_ts, p.tge_ts, "they did not touch the TGE date");
    assert_eq!(
        c.pending_owner,
        Some(candidate),
        "the unsigned accept_owner did not clear the candidate"
    );

    // A POSITIVE: the same call with owner's signature passes.
    env.expire_blockhash();
    let fresh = Pubkey::new_unique();
    send(&mut env, set_operator_ix(owner.pubkey(), fresh), &owner).expect("a signed owner passes");
    assert_eq!(env.vesting_config().operator, fresh, "operator was updated");
}

// ===================================================================
// a decoy config outside the canonical PDA
// ===================================================================

#[test]
fn forged_config_outside_canonical_pda_rejected_by_seeds() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let real = env.init_default(DEFAULT_TGE_OFFSET);

    // A forge: the account belongs to the Vesting program, the discriminator is
    // genuine and the fields are a copy of a live config, but the address is random.
    // There is no honest way to obtain that: `initialize` only ever creates the config at
    // the PDA of the seeds `[b"config"]`, and there is no other source of such accounts.
    //
    // ALL of the instructions over the config are swept: the singleton's binding by seeds
    // is a property of each context individually (`SetTge` and `OwnerUpdate` are declared
    // in different files), and checking one of them would leave a neighbour's removed
    // attributes unnoticed.
    //
    // The mutation check was performed across both contexts: with `seeds` + `bump`
    // removed the test turns red on the corresponding line of the loop (the instruction
    // passes and writes into the decoy account); the attributes were put back.
    let forged_address = Pubkey::new_unique();
    let snapshot = env.vesting_config();
    env.forge_config(&forged_address, &snapshot);
    let forged_before = env
        .svm
        .get_account(&forged_address)
        .expect("the decoy config was written")
        .data;
    assert_ne!(
        forged_address,
        config_pda(),
        "the forge sits precisely outside the canonical address - otherwise the test would be checking an ordinary positive"
    );

    // The arguments are valid: for the canonical config every call would pass, so the
    // rejection is caused by the config's address rather than by an argument.
    // `accept_owner` is signed by the candidate, so its attempt goes with that
    // signature, while the candidate itself is nominated in the REAL config - otherwise
    // the rejection would be ambiguous (`NoPendingOwner` instead of the config's
    // address).
    let moved_tge = real.tge_ts + MONTH;
    let hijacked_admin = Pubkey::new_unique();
    let hijacked_operator = Pubkey::new_unique();
    let hijacked_owner = Pubkey::new_unique();
    let candidate = env.new_wallet();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("the candidate was nominated in the real config");

    let attempts: [(&str, Instruction, &Keypair); 6] = [
        (
            "set_tge",
            set_tge_ix_with_config(admin.pubkey(), forged_address, moved_tge),
            &admin,
        ),
        (
            "set_admin",
            set_admin_ix_with_config(owner.pubkey(), forged_address, hijacked_admin),
            &owner,
        ),
        (
            "set_operator",
            set_operator_ix_with_config(owner.pubkey(), forged_address, hijacked_operator),
            &owner,
        ),
        (
            "propose_owner",
            propose_owner_ix_with_config(owner.pubkey(), forged_address, hijacked_owner),
            &owner,
        ),
        (
            "cancel_owner_proposal",
            cancel_owner_proposal_ix_with_config(owner.pubkey(), forged_address),
            &owner,
        ),
        (
            "accept_owner",
            accept_owner_ix_with_config(candidate.pubkey(), forged_address),
            &candidate,
        ),
    ];
    for (label, ix, signer) in attempts {
        env.expire_blockhash();
        let r = send(&mut env, ix, signer);
        assert_eq!(
            custom_error_code(&r),
            Some(u32::from(ErrorCode::ConstraintSeeds)),
            "{label} with a config outside the canonical PDA → ConstraintSeeds: Anchor derives the address from the seeds `[b\\\"config\\\"]` with the canonical bump and there is nothing to substitute it with: {r:?}"
        );

        let forged_after = env
            .svm
            .get_account(&forged_address)
            .expect("the decoy config is in place")
            .data;
        assert_eq!(
            forged_after, forged_before,
            "{label}: the decoy config was not changed by the rejected instruction"
        );
        let c = env.vesting_config();
        assert_eq!(
            c.tge_ts, real.tge_ts,
            "{label}: the real config is untouched"
        );
        assert_eq!(c.admin, real.admin, "{label}: admin is untouched");
        assert_eq!(c.operator, real.operator, "{label}: operator is untouched");
        assert_eq!(c.owner, real.owner, "{label}: owner is untouched");
        assert_eq!(
            c.pending_owner,
            Some(candidate.pubkey()),
            "{label}: the candidate in the real config is untouched"
        );
    }

    // A POSITIVE: the same six calls on the canonical config pass. `accept_owner` comes
    // last - it takes the role away from the previous owner.
    env.expire_blockhash();
    send(&mut env, set_tge_ix(admin.pubkey(), moved_tge), &admin)
        .expect("the canonical config accepts set_tge");
    assert_eq!(
        env.vesting_config().tge_ts,
        moved_tge,
        "the TGE date was moved"
    );

    env.expire_blockhash();
    send(
        &mut env,
        set_admin_ix(owner.pubkey(), hijacked_admin),
        &owner,
    )
    .expect("the canonical config accepts set_admin");
    assert_eq!(
        env.vesting_config().admin,
        hijacked_admin,
        "admin changed only through the canonical config"
    );

    env.expire_blockhash();
    send(
        &mut env,
        set_operator_ix(owner.pubkey(), hijacked_operator),
        &owner,
    )
    .expect("the canonical config accepts set_operator");
    assert_eq!(
        env.vesting_config().operator,
        hijacked_operator,
        "operator changed only through the canonical config"
    );

    env.expire_blockhash();
    send(&mut env, cancel_owner_proposal_ix(owner.pubkey()), &owner)
        .expect("the canonical config accepts cancel_owner_proposal");
    assert_eq!(
        env.vesting_config().pending_owner,
        None,
        "the candidate was withdrawn only through the canonical config"
    );

    env.expire_blockhash();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("the canonical config accepts propose_owner");
    assert_eq!(
        env.vesting_config().pending_owner,
        Some(candidate.pubkey()),
        "the candidate was nominated only through the canonical config"
    );

    env.expire_blockhash();
    send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate)
        .expect("the canonical config accepts accept_owner");
    let c = env.vesting_config();
    assert_eq!(
        c.owner,
        candidate.pubkey(),
        "owner changed only through the canonical config"
    );
    assert_eq!(
        c.pending_owner, None,
        "the candidate was cleared by the acceptance"
    );
}
