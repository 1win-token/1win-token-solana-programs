//! The role matrix "14 instructions × 6 actors" (Arch §8).
//!
//! The setup. Every instruction of the program is exercised by all six actors of the
//! environment: `owner` (the config's Owner), `admin` (the config's Admin),
//! `operator`, `recipient` (the scenario activity's recipient), `other_recipient`
//! (the recipient of ANOTHER activity) and `stranger` (a wallet with no role at all).
//! Every forbidden cell carries an executed negative with a specific code, and every
//! permitted one a positive with an observable effect: "the correct role
//! passes" is proven by execution rather than by the negatives having failed.
//!
//! Why the tests are laid out the way they are.
//!
//! 1. `initialize`'s privilege is NOT Owner. At that moment there is no config yet and
//!    the right belongs to the program's upgrade authority (Arch §9.1), so the row is
//!    built by substituting the ENVIRONMENT (`with_upgrade_authority` - the upgrade
//!    right belongs to an outside key) and all six actors, `owner` included, must
//!    receive `NotUpgradeAuthority`.
//! 2. Seven Owner instructions are held by one and the same guard
//!    `has_one = owner @ NotOwner` on the config, three Admin instructions
//!    (`set_tge`, `set_recipient`, `set_withdrawal_authority`) by the guard
//!    `has_one = admin @ NotAdmin`, and Anchor checks both during account RESOLUTION,
//!    before the handler's body. The negatives are nevertheless placed in a state
//!    where the CORRECT signer would pass (before TGE; for `close_vesting`, after the
//!    terminal claim): otherwise the failure would be ambiguous - a role one or a
//!    temporal one. In the Admin instructions' rows Owner stands among the wrong
//!    actors, and in the Owner instructions' rows Admin does: the permission sets do
//!    not overlap.
//!    `accept_owner` falls outside both groups: it is signed by the nominated
//!    candidate, so the context carries no `has_one` and the body checks that the
//!    signer matches `config.pending_owner` (`NotOwner`). Its row's candidate is a
//!    wallet OUTSIDE the matrix, so all six actors, the sitting Owner included, must
//!    be rejected. The negative stands with a candidate already nominated: without a
//!    nomination the rejection would come as `NoPendingOwner` and the role check would
//!    never run.
//! 3. The builder closure puts the actor's key BOTH into the signer slot's meta
//!    AND into the transaction's signers: pass it only into the slot and the rejection
//!    would come from the missing signature (`AccountNotSigner`), leaving the role
//!    guard unexecuted.
//! 4. `claim` is the only instruction with non-overlapping rights: either the
//!    recipient or the operator signs (Arch §8). A positive stands on each of the two
//!    permitted actors, while `owner` stands among the negatives: the row "Owner has
//!    no right to claim" (G-1) is verified right here and nowhere else.
//! 5. `assert_funded` has no signer at all (Arch §9.9): the actor acts as the fee
//!    payer, and all six cells of the row are positives. It has no role negative by
//! construction; a funding rejection lives in `funding.rs`.
//! 6. Every negative asserts the observable state is unchanged. The class "the
//!    rejection left no created ATA" is exercised on the SECOND activity, whose
//!    recipient still has no account: `init_if_needed` in `claim` runs in
//!    `try_accounts`, that is BEFORE the body's role check and at the signer's
//!    expense, so "there is no partial payout" rests solely on the whole transaction
//!    reverting - and that is observable only where the account did not exist before
//!    the call.
//!
//! ===== DELIBERATELY DEFERRED =====
//! - The temporal matrix of the same instructions against the three TGE moments -
//!   `tge_matrix.rs`.
//! - Slot substitutions (`vault_authority`, a direct SPL call on a vault).
//! - A multisig as the real Owner signer: the tests use a single key, as does the Sale
//!   suite.

use litesvm::types::TransactionResult;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting::errors::VestingError;
use vesting::events::Claimed;
use vesting_test_common::vesting_expected::vesting::{
    ECOSYSTEM_FIRST_MONTHLY_DAY, ECOSYSTEM_FULL_RELEASE_DAY, MONTH, V2_REFERENCE, V2_VESTED_DAY_31,
    V2_VESTED_DAY_61,
};
use vesting_test_common::{
    accept_owner_ix, assert_funded_ix, cancel_owner_proposal_ix, cancel_vesting_ix, claim_ix,
    close_vesting_ix, create_vesting_ix, custom_error_code, decode_event, err, initialize_ix,
    propose_owner_ix, set_admin_ix, set_operator_ix, set_recipient_ix, set_tge_ix,
    set_withdrawal_authority_ix, ActivitySpec, TestEnv, ACTOR_LAMPORTS, DEFAULT_TGE_OFFSET,
};

/// The reference amount of the matrix's activities: the V2 bucket of §10.5
/// (100,000,000 $1WIN in token units) - the same one the architecture's V2/V5 numeric
/// examples are built on.
const REFERENCE: u64 = V2_REFERENCE;

/// The day of the first monthly tranche of the "Ecosystem" schedule - the point
/// of the `claim` row's first positive (signed by the recipient).
const DAY_31: i64 = ECOSYSTEM_FIRST_MONTHLY_DAY;

/// The day of the second monthly tranche (`p = 2`): the `claim` row's negatives and
/// the operator's positive stand there. By that moment only the day-31 tranche has
/// been released to the recipient, so claimable is non-zero - the rejection of a wrong
/// actor is precisely a role one, and the operator's positive right afterwards proves
/// it.
const DAY_61: i64 = 61;

// We return the native litesvm TransactionResult as is - just like `send_ixs`.
#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// The matrix's six actors. `recipient` is the scenario activity's recipient,
/// `other_recipient` is a key that in the `claim` row really becomes the recipient of
/// ANOTHER activity, and `stranger` is a wallet with no role at all.
fn matrix_actors(env: &TestEnv) -> Vec<(&'static str, Keypair)> {
    vec![
        ("owner", env.actors.owner.insecure_clone()),
        ("admin", env.actors.admin.insecure_clone()),
        ("operator", env.actors.operator.insecure_clone()),
        ("recipient", env.actors.recipients[0].insecure_clone()),
        ("other_recipient", env.actors.recipients[1].insecure_clone()),
        ("stranger", env.actors.stranger.insecure_clone()),
    ]
}

/// The matrix minus the `allowed` labels - a row's "wrong actors". It panics if a
/// label is absent from the matrix: a typo would otherwise silently narrow the sweep
/// (fail-closed).
fn actors_except(env: &TestEnv, allowed: &[&str]) -> Vec<(&'static str, Keypair)> {
    let all = matrix_actors(env);
    for label in allowed {
        assert!(
            all.iter().any(|(l, _)| l == label),
            "the label {label} is absent from the actor matrix - the row would sweep the wrong ones"
        );
    }
    let rest: Vec<(&'static str, Keypair)> = all
        .into_iter()
        .filter(|(l, _)| !allowed.contains(l))
        .collect();
    assert_eq!(
        rest.len(),
        6 - allowed.len(),
        "the sweep of wrong actors must cover the whole matrix minus the permitted ones"
    );
    rest
}

/// Sweeps the cells of a row where the actor has no right to invoke the instruction.
///
/// `build_ix` receives the actor's pubkey: that same key goes into the signer slot's
/// meta, signs the transaction, and is its fee payer. After every rejection
/// `assert_intact` checks that the observable state is unchanged (it receives the
/// actor's label - for the message - and its pubkey).
fn reject_all<F, C>(
    env: &mut TestEnv,
    wrong: &[(&'static str, Keypair)],
    what: &str,
    expected: VestingError,
    build_ix: F,
    mut assert_intact: C,
) where
    F: Fn(Pubkey) -> Instruction,
    C: FnMut(&TestEnv, &str, Pubkey),
{
    let expected_code = err(expected);
    for (label, actor) in wrong {
        env.expire_blockhash();
        let r = send(env, build_ix(actor.pubkey()), actor);
        assert_eq!(
            custom_error_code(&r),
            Some(expected_code),
            "{label} signed {what} → {expected:?} was expected: {r:?}"
        );
        assert_intact(env, label, actor.pubkey());
    }
}

// ===================================================================
// Row 1/14 - initialize: the privilege belongs to the upgrade authority, not to Owner
// ===================================================================

#[test]
fn initialize_rejects_every_actor_but_the_upgrade_authority() {
    // The upgrade right belongs to a key that is not among the actors. So no actor,
    // including the config's future `owner`, has the right to invoke `initialize`: the
    // Owner role gives nothing here - there is nobody to read it yet.
    let upgrade_authority = Keypair::new();
    let mut env = TestEnv::with_upgrade_authority(upgrade_authority.pubkey());
    env.svm
        .airdrop(&upgrade_authority.pubkey(), ACTOR_LAMPORTS)
        .expect("an airdrop to the real upgrade authority");

    let params = env.valid_init_params(DEFAULT_TGE_OFFSET);
    let wrong = matrix_actors(&env);
    reject_all(
        &mut env,
        &wrong,
        "initialize",
        VestingError::NotUpgradeAuthority,
        |a| initialize_ix(a, &params),
        |env, label, _| {
            assert!(
                !env.vesting_config_exists(),
                "{label}: the rejection leaves no singleton behind - otherwise the hijack would have partly succeeded (the config's address being occupied)"
            )
        },
    );

    // POSITIVE: the holder of the upgrade authority creates the singleton in the same
    // environment.
    env.expire_blockhash();
    let params = env.valid_init_params(DEFAULT_TGE_OFFSET);
    send(
        &mut env,
        initialize_ix(upgrade_authority.pubkey(), &params),
        &upgrade_authority,
    )
    .expect("the holder of the upgrade authority: initialize passes");
    assert_eq!(
        env.vesting_config().owner,
        params.owner,
        "the config was created by the upgrade authority itself, and its owner comes from the argument"
    );
}

// ===================================================================
// Rows 2-4/14 - Admin-only: set_tge, set_recipient, set_withdrawal_authority
// ===================================================================

#[test]
fn set_tge_rejects_every_actor_but_admin() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    // The negatives stand before TGE and with a valid argument: at that point the correct
    // Admin would pass, so the rejection is a role one rather than temporal or
    // argument-related.
    let moved = p.tge_ts + MONTH;
    let tge_before = p.tge_ts;

    let wrong = actors_except(&env, &["admin"]);
    reject_all(
        &mut env,
        &wrong,
        "set_tge",
        VestingError::NotAdmin,
        |a| set_tge_ix(a, moved),
        |env, label, _| {
            assert_eq!(
                env.vesting_config().tge_ts,
                tge_before,
                "{label}: the rejection did not move the TGE date"
            )
        },
    );

    env.expire_blockhash();
    send(&mut env, set_tge_ix(admin.pubkey(), moved), &admin)
        .expect("admin: set_tge before TGE passes");
    assert_eq!(
        env.vesting_config().tge_ts,
        moved,
        "admin really did move the TGE date"
    );
}

#[test]
fn set_recipient_rejects_every_actor_but_admin() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let new_recipient = env.actors.recipients[1].pubkey();
    let id = env.create_activity_for(&recipient, &ActivitySpec::valid(REFERENCE));

    let wrong = actors_except(&env, &["admin"]);
    reject_all(
        &mut env,
        &wrong,
        "set_recipient",
        VestingError::NotAdmin,
        |a| set_recipient_ix(a, id, new_recipient),
        |env, label, _| {
            assert_eq!(
                env.vesting(id).recipient,
                recipient,
                "{label}: the rejection did not reassign the recipient - otherwise a bucket could be taken over by a foreign signature"
            )
        },
    );

    env.expire_blockhash();
    send(
        &mut env,
        set_recipient_ix(admin.pubkey(), id, new_recipient),
        &admin,
    )
    .expect("admin: set_recipient before TGE passes");
    assert_eq!(
        env.vesting(id).recipient,
        new_recipient,
        "admin really did reassign the activity's recipient"
    );
}

#[test]
fn set_withdrawal_authority_rejects_every_actor_but_admin() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    // A wallet distinct from the current withdrawal target and not the vaults' PDA: the
    // address checks are satisfied and only the role one remains.
    let fresh_target = env.new_wallet().pubkey();

    let wrong = actors_except(&env, &["admin"]);
    reject_all(
        &mut env,
        &wrong,
        "set_withdrawal_authority",
        VestingError::NotAdmin,
        |a| set_withdrawal_authority_ix(a, fresh_target),
        |env, label, _| {
            assert_eq!(
                env.vesting_config().withdrawal_authority,
                p.withdrawal_authority,
                "{label}: the rejection did not redirect the remainder and the rent of closing activities to a foreign address"
            )
        },
    );

    env.expire_blockhash();
    send(
        &mut env,
        set_withdrawal_authority_ix(admin.pubkey(), fresh_target),
        &admin,
    )
    .expect("admin: set_withdrawal_authority passes");
    assert_eq!(
        env.vesting_config().withdrawal_authority,
        fresh_target,
        "admin really did change the withdrawal target"
    );
}

// ===================================================================
// Rows 5-11/14 - Owner-only: set_admin, set_operator, propose_owner,
// cancel_owner_proposal, create_vesting, cancel_vesting, close_vesting;
// row 12/14 - accept_owner (signed by the candidate)
// ===================================================================

#[test]
fn set_admin_rejects_every_actor_but_owner() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    // A non-zero key distinct from Owner and from the sitting operator: `set_admin`'s
    // argument checks are satisfied and only the role one remains.
    let fresh_admin = Pubkey::new_unique();

    let wrong = actors_except(&env, &["owner"]);
    reject_all(
        &mut env,
        &wrong,
        "set_admin",
        VestingError::NotOwner,
        |a| set_admin_ix(a, fresh_admin),
        |env, label, _| {
            assert_eq!(
                env.vesting_config().admin,
                p.admin,
                "{label}: the rejection did not reassign admin - otherwise the right to configure would be taken over by a foreign signature"
            )
        },
    );

    env.expire_blockhash();
    send(&mut env, set_admin_ix(owner.pubkey(), fresh_admin), &owner)
        .expect("owner: set_admin passes");
    assert_eq!(
        env.vesting_config().admin,
        fresh_admin,
        "owner really did reassign admin"
    );
}

#[test]
fn set_operator_rejects_every_actor_but_owner() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    // A non-zero key distinct from Owner: `set_operator`'s argument gates
    // (`InvalidAuthority`) are satisfied and only the role one remains.
    let fresh_operator = Pubkey::new_unique();

    let wrong = actors_except(&env, &["owner"]);
    reject_all(
        &mut env,
        &wrong,
        "set_operator",
        VestingError::NotOwner,
        |a| set_operator_ix(a, fresh_operator),
        |env, label, _| {
            assert_eq!(
                env.vesting_config().operator,
                p.operator,
                "{label}: the rejection did not reassign operator"
            )
        },
    );

    env.expire_blockhash();
    send(
        &mut env,
        set_operator_ix(owner.pubkey(), fresh_operator),
        &owner,
    )
    .expect("owner: set_operator passes");
    assert_eq!(
        env.vesting_config().operator,
        fresh_operator,
        "owner really did reassign operator"
    );
}

#[test]
fn propose_owner_rejects_every_actor_but_owner() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    // A key distinct from zero and from the sitting operator: `propose_owner`'s argument
    // gates are satisfied and only the role one remains.
    let fresh_owner = Pubkey::new_unique();

    let wrong = actors_except(&env, &["owner"]);
    reject_all(
        &mut env,
        &wrong,
        "propose_owner",
        VestingError::NotOwner,
        |a| propose_owner_ix(a, fresh_owner),
        |env, label, _| {
            assert_eq!(
                env.vesting_config().pending_owner,
                None,
                "{label}: the rejection nominated no candidate - otherwise a foreign key would queue up for ownership"
            )
        },
    );

    env.expire_blockhash();
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), fresh_owner),
        &owner,
    )
    .expect("owner: propose_owner passes");
    let c = env.vesting_config();
    assert_eq!(
        c.pending_owner,
        Some(fresh_owner),
        "owner really did nominate a candidate"
    );
    assert_eq!(
        c.owner, p.owner,
        "the nomination has not handed ownership over yet"
    );
}

#[test]
fn cancel_owner_proposal_rejects_every_actor_but_owner() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default(DEFAULT_TGE_OFFSET);
    let candidate = Pubkey::new_unique();

    // A nomination already exists: at that point the correct Owner would pass, so the
    // rejection is a role one rather than "there is no candidate".
    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate),
        &owner,
    )
    .expect("owner: propose_owner passes");

    let wrong = actors_except(&env, &["owner"]);
    reject_all(
        &mut env,
        &wrong,
        "cancel_owner_proposal",
        VestingError::NotOwner,
        cancel_owner_proposal_ix,
        |env, label, _| {
            assert_eq!(
                env.vesting_config().pending_owner,
                Some(candidate),
                "{label}: the rejection did not withdraw the candidate"
            )
        },
    );

    env.expire_blockhash();
    send(&mut env, cancel_owner_proposal_ix(owner.pubkey()), &owner)
        .expect("owner: cancel_owner_proposal passes");
    assert_eq!(
        env.vesting_config().pending_owner,
        None,
        "owner really did withdraw the candidate"
    );
}

#[test]
fn accept_owner_rejects_every_actor_but_the_candidate() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    // The candidate is a wallet outside the actor matrix: so no actor of it, the sitting
    // Owner included, has the right to accept.
    let candidate = env.new_wallet();

    send(
        &mut env,
        propose_owner_ix(owner.pubkey(), candidate.pubkey()),
        &owner,
    )
    .expect("owner: propose_owner passes");

    let wrong = matrix_actors(&env);
    reject_all(
        &mut env,
        &wrong,
        "accept_owner",
        VestingError::NotOwner,
        accept_owner_ix,
        |env, label, _| {
            let c = env.vesting_config();
            assert_eq!(
                c.owner, p.owner,
                "{label}: the rejection did not hand ownership over - only the nominated candidate accepts"
            );
            assert_eq!(
                c.pending_owner,
                Some(candidate.pubkey()),
                "{label}: the rejection did not clear the candidate"
            );
        },
    );

    // The POSITIVE is the test's last action: after it actors.owner is no longer Owner.
    env.expire_blockhash();
    send(&mut env, accept_owner_ix(candidate.pubkey()), &candidate)
        .expect("the candidate: accept_owner passes");
    let c = env.vesting_config();
    assert_eq!(
        c.owner,
        candidate.pubkey(),
        "ownership really was handed over"
    );
    assert_eq!(
        c.pending_owner, None,
        "the candidate was cleared by the acceptance"
    );
}

#[test]
fn create_vesting_rejects_every_actor_but_owner() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let mint = env.win_mint;
    let spec = ActivitySpec::valid(REFERENCE);
    // The identifier the next successful call will assign: the negatives do not move it,
    // so it is the same one for the whole row.
    let next_id = env.next_activity_id();

    let wrong = actors_except(&env, &["owner"]);
    reject_all(
        &mut env,
        &wrong,
        "create_vesting",
        VestingError::NotOwner,
        |a| create_vesting_ix(a, recipient, next_id, mint, &spec),
        |env, label, _| {
            assert_eq!(
                env.vesting_config().activity_count,
                next_id,
                "{label}: the id allocator did not move"
            );
            assert!(
                !env.vesting_exists(next_id) && !env.vault_exists(next_id),
                "{label}: the rejection created neither an activity record nor its vault"
            );
        },
    );

    env.expire_blockhash();
    send(
        &mut env,
        create_vesting_ix(owner.pubkey(), recipient, next_id, mint, &spec),
        &owner,
    )
    .expect("owner: create_vesting before TGE passes");
    assert_eq!(
        env.vesting(next_id).recipient,
        recipient,
        "owner really did create an activity for the given recipient"
    );
    assert!(
        env.vault_exists(next_id),
        "the activity's vault was created by the same call"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        next_id + 1,
        "the id allocator was incremented by exactly one activity"
    );
}

#[test]
fn cancel_vesting_rejects_every_actor_but_owner() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    env.init_default(DEFAULT_TGE_OFFSET);
    let id = env.create_activity(&ActivitySpec::valid(REFERENCE));
    env.fund_vault(id, REFERENCE);

    // Both withdrawal addressees are pinned by `config.withdrawal_authority` (the
    // `rent_collector` actor): pass different ones and the rejection would come as
    // `InvalidBeneficiary` or `InvalidRentCollector`, and the row would be checking
    // something other than the role.
    let withdrawal = env.actors.rent_collector.pubkey();
    let mint = env.win_mint;
    let beneficiary_token = env.create_ata(&withdrawal, &mint);

    let wrong = actors_except(&env, &["owner"]);
    reject_all(
        &mut env,
        &wrong,
        "cancel_vesting",
        VestingError::NotOwner,
        |a| cancel_vesting_ix(a, id, beneficiary_token, withdrawal, mint),
        |env, label, _| {
            assert!(
                env.vesting_exists(id) && env.vault_exists(id),
                "{label}: the rejection closed neither the record nor the vault"
            );
            assert_eq!(
                env.vault_balance(id),
                REFERENCE,
                "{label}: not a single unit left the vault"
            );
            assert_eq!(
                env.token_balance(&beneficiary_token),
                0,
                "{label}: not a single unit arrived at the withdrawal account"
            );
        },
    );

    env.expire_blockhash();
    send(
        &mut env,
        cancel_vesting_ix(owner.pubkey(), id, beneficiary_token, withdrawal, mint),
        &owner,
    )
    .expect("owner: cancel_vesting before TGE passes");
    assert!(
        !env.vesting_exists(id) && !env.vault_exists(id),
        "owner really did close both of the activity's accounts"
    );
    assert_eq!(
        env.token_balance(&beneficiary_token),
        REFERENCE,
        "the whole vault remainder went to the single withdrawal recipient's account"
    );
}

#[test]
fn close_vesting_rejects_every_actor_but_owner() {
    let mut env = TestEnv::new();
    let owner = env.actors.owner.insecure_clone();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].insecure_clone();
    let id = env.create_activity_for(&recipient.pubkey(), &ActivitySpec::valid(REFERENCE));
    env.fund_vault(id, REFERENCE);

    let withdrawal = env.actors.rent_collector.pubkey();
    let mint = env.win_mint;
    let beneficiary_token = env.create_ata(&withdrawal, &mint);

    // The negatives are placed in a state where the correct Owner would pass:
    // `close_vesting`'s gate is the full draw-down (`claimed >= reference_amount`),
    // so the terminal claim comes first.
    env.claim_at_day(p.tge_ts, ECOSYSTEM_FULL_RELEASE_DAY, id, &recipient)
        .expect("the terminal claim releases the whole reference amount");
    assert_eq!(
        env.vesting(id).claimed,
        REFERENCE,
        "precondition: the activity is fully drawn down and the close gate is open"
    );

    let wrong = actors_except(&env, &["owner"]);
    reject_all(
        &mut env,
        &wrong,
        "close_vesting",
        VestingError::NotOwner,
        |a| close_vesting_ix(a, id, beneficiary_token, withdrawal, mint),
        |env, label, _| {
            assert!(
                env.vesting_exists(id) && env.vault_exists(id),
                "{label}: the rejection did not close the drawn-down activity's accounts"
            );
            assert_eq!(
                env.vesting(id).claimed,
                REFERENCE,
                "{label}: the activity's accounting is untouched"
            );
        },
    );

    env.expire_blockhash();
    send(
        &mut env,
        close_vesting_ix(owner.pubkey(), id, beneficiary_token, withdrawal, mint),
        &owner,
    )
    .expect("owner: close_vesting of a fully drawn activity passes");
    assert!(
        !env.vesting_exists(id) && !env.vault_exists(id),
        "owner really did close both of the activity's accounts"
    );
}

// ===================================================================
// Row 13/14 - claim: the recipient OR the operator, nobody else
// ===================================================================

#[test]
fn claim_accepts_only_recipient_and_operator() {
    let mut env = TestEnv::new();
    let p = env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].insecure_clone();
    let other_recipient = env.actors.recipients[1].insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let mint = env.win_mint;

    let id = env.create_activity_for(&recipient.pubkey(), &ActivitySpec::valid(REFERENCE));
    env.fund_vault(id, REFERENCE);
    // `other_recipient` is the recipient of ANOTHER activity: rejecting it proves the
    // right is bound to the activity's record rather than to "being a recipient in
    // general".
    let other_id =
        env.create_activity_for(&other_recipient.pubkey(), &ActivitySpec::valid(REFERENCE));
    env.fund_vault(other_id, REFERENCE);

    // POSITIVE 1: the recipient claims what accrued by day 31 (the expectation is a
    // literal from the reference crate, not a recomputation of the program's formula).
    env.warp_to_day(p.tge_ts, DAY_31);
    env.claim_as(&recipient, id, recipient.pubkey())
        .expect("the recipient has the right to claim its own activity");
    assert_eq!(
        env.recipient_ata_balance(&recipient.pubkey()),
        V2_VESTED_DAY_31,
        "exactly the amount accrued by day 31 was released to the recipient"
    );

    // The negatives sit on day 61, where claimable is non-zero (the operator's positive
    // below proves it): the rejection is a role one rather than "there is nothing to
    // release".
    env.warp_to_day(p.tge_ts, DAY_61);
    let claimed_before = env.vesting(id).claimed;
    let vault_before = env.vault_balance(id);
    let paid_before = env.recipient_ata_balance(&recipient.pubkey());

    let wrong = actors_except(&env, &["recipient", "operator"]);
    reject_all(
        &mut env,
        &wrong,
        "claim",
        VestingError::NotAuthorized,
        // The `recipient` slot stays CORRECT: substitute it and the failure would come
        // earlier, on the `address = vesting.recipient` binding, leaving the signer's role
        // unchecked.
        |a| claim_ix(a, id, recipient.pubkey(), mint),
        |env, label, _| {
            assert_eq!(
                env.vesting(id).claimed,
                claimed_before,
                "{label}: the activity's accounting did not move"
            );
            assert_eq!(
                env.vault_balance(id),
                vault_before,
                "{label}: not a single unit left the vault"
            );
            assert_eq!(
                env.recipient_ata_balance(&recipient.pubkey()),
                paid_before,
                "{label}: nothing was released to the recipient"
            );
        },
    );

    // The same role rejection on an activity whose recipient still has NO ATA: the class
    // "the rejection left no created ATA" is exercised only here. `init_if_needed`
    // creates the account in `try_accounts`, that is BEFORE the body's role check and at
    // the signer's expense, so the only thing holding "there is no partial payout" is the
    // whole transaction reverting. On activity `id` that assertion is unverifiable: its
    // ATA was created by positive 1.
    assert!(
        !env.recipient_ata_exists(&other_recipient.pubkey()),
        "precondition: the second activity's recipient has no ATA yet - otherwise the case would be checking the wrong thing"
    );
    let other_claimed_before = env.vesting(other_id).claimed;
    let other_vault_before = env.vault_balance(other_id);

    let wrong_for_other = actors_except(&env, &["other_recipient", "operator"]);
    reject_all(
        &mut env,
        &wrong_for_other,
        "the claim of the second activity",
        VestingError::NotAuthorized,
        |a| claim_ix(a, other_id, other_recipient.pubkey(), mint),
        |env, label, _| {
            assert!(
                !env.recipient_ata_exists(&other_recipient.pubkey()),
                "{label}: the recipient's ATA was not created - the revert rolled back init_if_needed too, otherwise the rejection would leave an account paid for by a foreign signature"
            );
            assert_eq!(
                env.vesting(other_id).claimed,
                other_claimed_before,
                "{label}: the second activity's accounting did not move"
            );
            assert_eq!(
                env.vault_balance(other_id),
                other_vault_before,
                "{label}: not a single unit left its vault"
            );
        },
    );

    // POSITIVE 2: the operator triggers the claim and the funds go to the RECIPIENT.
    env.expire_blockhash();
    let r = env.claim_as(&operator, id, recipient.pubkey());
    r.as_ref()
        .expect("the operator has the right to trigger a claim (Arch §8)");
    assert_eq!(
        env.recipient_ata_balance(&recipient.pubkey()),
        V2_VESTED_DAY_61,
        "the total accrued by day 61 sits in the recipient's ATA"
    );
    assert_eq!(
        env.vesting(id).claimed,
        V2_VESTED_DAY_61,
        "claimed == vested(day 61) after the operator's claim"
    );
    assert_eq!(
        env.vault_balance(id),
        REFERENCE - V2_VESTED_DAY_61,
        "the vault decreased by exactly what was released"
    );
    let ev: Claimed = decode_event(&r).expect("claim emits Claimed");
    assert_eq!(
        ev.recipient,
        recipient.pubkey(),
        "the event carries the activity's recipient, not the signer"
    );

    // POSITIVE 3: the same `other_recipient` that was rejected above claims ITS OWN
    // activity - the rejection was bound to the record, not to the key. It is also the
    // counterpart to the negatives with a cold ATA: the recipient's account is
    // created by this very call, so the rejections above left none behind.
    env.expire_blockhash();
    env.claim_as(&other_recipient, other_id, other_recipient.pubkey())
        .expect("the other activity's recipient claims exactly that activity");
    assert!(
        env.recipient_ata_exists(&other_recipient.pubkey()),
        "the recipient's ATA appeared on the successful claim - before it there was none"
    );
    assert_eq!(
        env.recipient_ata_balance(&other_recipient.pubkey()),
        V2_VESTED_DAY_61,
        "the other activity was paid its own accrued amount"
    );
}

// ===================================================================
// Row 14/14 - assert_funded: permissionless, all six actors pass
// ===================================================================

/// The observable state `assert_funded` must not touch: the activity's accounting, its
/// vault, the id allocator and the config's roles.
#[allow(clippy::type_complexity)]
fn state_digest(
    env: &TestEnv,
    activity_id: u64,
) -> (u64, u64, u64, Pubkey, Pubkey, Pubkey, Pubkey, i64) {
    let c = env.vesting_config();
    let v = env.vesting(activity_id);
    (
        v.claimed,
        v.reference_amount,
        env.vault_balance(activity_id),
        v.recipient,
        c.owner,
        c.admin,
        c.operator,
        c.tge_ts,
    )
}

#[test]
fn assert_funded_accepts_all_six_actors() {
    let mut env = TestEnv::new();
    env.init_default(DEFAULT_TGE_OFFSET);
    let id = env.create_activity(&ActivitySpec::valid(REFERENCE));
    env.fund_vault(id, REFERENCE);
    let mint = env.win_mint;
    let count_before = env.vesting_config().activity_count;
    let before = state_digest(&env, id);

    // The instruction has no Signer slot at all (Arch §9.9): the actor acts only as the
    // fee payer, and that is precisely the check of permissionless access.
    for (label, actor) in matrix_actors(&env) {
        env.expire_blockhash();
        let r = env.send_ixs(&[assert_funded_ix(id, mint)], &actor.pubkey(), &[&actor]);
        assert!(
            r.is_ok(),
            "{label}: assert_funded is permissionless - it does not check the signer's role: {r:?}"
        );
        assert_eq!(
            state_digest(&env, id),
            before,
            "{label}: assert_funded changes no state - it is a check, not an operation"
        );
        assert_eq!(
            env.vesting_config().activity_count,
            count_before,
            "{label}: the id allocator is untouched"
        );
    }
}
