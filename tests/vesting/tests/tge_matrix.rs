//! The temporal matrix "instructions × 3 TGE moments" (Arch §4.2).
//!
//! The setup. Each moment lives in its OWN environment: `warp_to` only moves forward
//! (rolling time back is forbidden by construction), so "one second before
//! TGE", "exactly at TGE" and "one second after TGE" are three independent runs of
//! one scenario rather than three steps of one. The points are taken to the second:
//! the shared `require_before_tge` gate is a strict `now < tge_ts`, so the second
//! `tge − 1` must pass while the second `tge` itself must not.
//!
//! The matrix's rows.
//!
//! 1. `create_vesting` / `cancel_vesting` / `set_tge` - OK → `ScheduleStarted` →
//!    `ScheduleStarted`: the editing window of §4.2 closes exactly at the TGE
//!    instant. It holds the set of activities, their amounts and the date itself; the
//!    recipient is not part of it (row 5).
//! 2. `claim` - `ScheduleNotStarted` → OK or `NothingToClaim` → OK. At the TGE
//!    instant itself BOTH sub-cases are executed: what decides the answer there is the
//!    `tge_bps` share (a schedule with a share releases it at once, a schedule
//!    without one answers `NothingToClaim`) rather than time - otherwise "time has
//!    opened" would be verified on only one of the two kinds of schedule.
//! 3. `close_vesting` - `NotFullyClaimed` → `NotFullyClaimed` → OK. It has no time
//!    gate at all: in the first two moments the rejection comes from the
//!    draw-down rather than from time, and in the third the close passes - after a
//!    claim that creates that draw-down. A full draw-down in a single claim is
//!    reachable only on the "100% at TGE" schedule, so the row runs on it.
//! 4. `set_admin` / `set_operator` / `propose_owner` / `cancel_owner_proposal` /
//!    `accept_owner` / `set_withdrawal_authority` / `assert_funded` - OK at all three
//!    moments.
//!    A positive in each is mandatory: without an executed call the claim "TGE does
//!    not gate them" is verified by nothing, and a superfluous time gate would slip
//!    past the suite.
//! 5. `set_recipient` - OK at all three moments: the payout addressee is editable
//!    throughout an activity's life. A separate row rather than part of row 4: the
//!    instructions of row 4 do not touch an activity, while here it is also verified
//!    that the change moves neither the obligation's amount, nor the accounting of
//!    what was released, nor the vault's funds.
//!
//! `initialize` is not part of the matrix. Its time gate sits on the `tge_ts`
//! ARGUMENT (`now < tge_ts <= now + MAX_TGE_DELAY`, `InvalidTge`) rather than on the
//! instant of an already-set TGE: before `initialize` there is neither a config nor a
//! `config.tge_ts` field, so it has no "before / at / after TGE" cells at all. Its
//! argument's boundaries are covered by `initialize.rs`.
//!
//! Every negative carries an assertion that the observable state is unchanged: the
//! [`Snapshot`] before and after the rejection must coincide.
//!
//! ===== DELIBERATELY DEFERRED =====
//! - The role matrix of the same instructions - `role_matrix.rs`.
//! - The boundaries of `set_tge`'s argument (`now`, `now + MAX_TGE_DELAY`, a
//!   one-second step) (`config_setters.rs`): what is checked here is the
//!   moment of the call, not the value.
//! - Slot substitutions.

use litesvm::types::TransactionResult;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting::errors::VestingError;
use vesting_test_common::vesting_expected::vesting::{
    COMMUNITY_CLIFF_MONTHS, COMMUNITY_FULL_RELEASE_DAY, COMMUNITY_MONTHLY_BPS, COMMUNITY_TGE_BPS,
    MARKETING_CLIFF_MONTHS, MARKETING_MONTHLY_BPS, MARKETING_TGE_BPS, MONTH, V1_REFERENCE,
    V1_VESTED_FROM_TGE, V2_REFERENCE, V2_TGE_AMT, V2_VESTED_DAY_1, V4_REFERENCE,
    V4_VESTED_MONTHS_12,
};
use vesting_test_common::{
    accept_owner_ix, assert_funded_ix, cancel_owner_proposal_ix, cancel_vesting_ix,
    close_vesting_ix, create_vesting_ix, custom_error_code, err, propose_owner_ix, set_admin_ix,
    set_operator_ix, set_recipient_ix, set_tge_ix, set_withdrawal_authority_ix, ActivitySpec,
    TestEnv, DEFAULT_TGE_OFFSET,
};

/// The default activity's reference amount: the V2 bucket of §10.5 (100,000,000 $1WIN
/// in token units) on the "Ecosystem" schedule ([`ActivitySpec::valid`]) - which
/// has both a TGE share and monthly tranches.
const REFERENCE: u64 = V2_REFERENCE;

// We return the native litesvm TransactionResult as is - just like `send_ixs`.
#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

/// The moment of a call relative to TGE. Three points to the second - the whole domain
/// of the time gate: a strict "before", the boundary itself, and "after".
#[derive(Clone, Copy, Debug)]
enum Moment {
    OneSecondBefore,
    Exactly,
    OneSecondAfter,
}

impl Moment {
    const ALL: [Moment; 3] = [
        Moment::OneSecondBefore,
        Moment::Exactly,
        Moment::OneSecondAfter,
    ];

    /// The moment's offset from `tge_ts`, in seconds.
    fn offset(self) -> i64 {
        match self {
            Moment::OneSecondBefore => -1,
            Moment::Exactly => 0,
            Moment::OneSecondAfter => 1,
        }
    }

    /// A label for the assertion messages: it shows which cell of the row failed.
    fn label(self) -> &'static str {
        match self {
            Moment::OneSecondBefore => "tge − 1",
            Moment::Exactly => "tge",
            Moment::OneSecondAfter => "tge + 1",
        }
    }

    /// Whether the editing window of §4.2 is open at this moment (`now < tge_ts`).
    fn before_tge(self) -> bool {
        matches!(self, Moment::OneSecondBefore)
    }
}

/// An environment brought to the moment `moment`: the config (TGE in a month), an
/// activity on schedule `spec` for `recipients[0]`, its vault funded with exactly
/// `spec.reference_amount`, and the ATA of the single withdrawal recipient W
/// (`config.withdrawal_authority` = the `rent_collector` actor) - the addressee of the
/// remainder on a cancellation or a close.
struct Fixture {
    env: TestEnv,
    activity_id: u64,
    tge_ts: i64,
    beneficiary_token: Pubkey,
}

fn fixture_at(moment: Moment, spec: &ActivitySpec) -> Fixture {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let recipient = env.actors.recipients[0].pubkey();
    let activity_id = env.create_activity_for(&recipient, spec);
    env.fund_vault(activity_id, spec.reference_amount);

    let withdrawal = env.actors.rent_collector.pubkey();
    let mint = env.win_mint;
    let beneficiary_token = env.create_ata(&withdrawal, &mint);

    // The jump to the row's moment: the whole fixture is assembled before TGE (otherwise
    // `create_vesting` would itself be closed by the gate), and this is the last action.
    env.warp_to(params.tge_ts + moment.offset());

    Fixture {
        env,
        activity_id,
        tge_ts: params.tge_ts,
        beneficiary_token,
    }
}

/// The observable state around the activity and the config. Comparing the snapshots
/// before and after a rejection is the "the negative left no partial effect"
/// assertion in a single line.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    tge_ts: i64,
    owner: Pubkey,
    admin: Pubkey,
    operator: Pubkey,
    activity_count: u64,
    vesting_exists: bool,
    vault_exists: bool,
    vault_balance: u64,
    recipient: Pubkey,
    claimed: u64,
    beneficiary_balance: u64,
}

/// It panics if the record or the vault is absent: the snapshot is taken where the
/// activity must be alive, and "the account is gone" must never be read silently as
/// "empty" (fail-closed). The fact of closing is checked by separate assertions.
fn snapshot(f: &Fixture) -> Snapshot {
    let c = f.env.vesting_config();
    let v = f.env.vesting(f.activity_id);
    Snapshot {
        tge_ts: c.tge_ts,
        owner: c.owner,
        admin: c.admin,
        operator: c.operator,
        activity_count: c.activity_count,
        vesting_exists: f.env.vesting_exists(f.activity_id),
        vault_exists: f.env.vault_exists(f.activity_id),
        vault_balance: f.env.vault_balance(f.activity_id),
        recipient: v.recipient,
        claimed: v.claimed,
        beneficiary_balance: f.env.token_balance(&f.beneficiary_token),
    }
}

/// The "100% at TGE" schedule (Marketing, §10.5): the whole amount is
/// unlocked at the TGE instant and there are no monthly tranches. It is needed by the
/// `close_vesting` row: a full draw-down in a single claim is reachable only on it.
fn full_tge_spec() -> ActivitySpec {
    ActivitySpec {
        reference_amount: V1_REFERENCE,
        tge_bps: MARKETING_TGE_BPS,
        monthly_bps: MARKETING_MONTHLY_BPS,
        cliff_months: MARKETING_CLIFF_MONTHS,
        label: String::from("marketing"),
    }
}

/// The Community Rewards schedule (§10.5): `tge_bps = 0`, so at the TGE instant itself
/// nothing is unlocked. The reference is the degenerate 1000 token units of example
/// V4, so that the expectations come as literals from the reference crate.
fn no_tge_share_spec() -> ActivitySpec {
    ActivitySpec {
        reference_amount: V4_REFERENCE,
        tge_bps: COMMUNITY_TGE_BPS,
        monthly_bps: COMMUNITY_MONTHLY_BPS,
        cliff_months: COMMUNITY_CLIFF_MONTHS,
        label: String::from("community"),
    }
}

// ===================================================================
// The editing window of §4.2: create_vesting, cancel_vesting, set_tge
// ===================================================================

#[test]
fn create_vesting_is_open_only_before_tge() {
    for moment in Moment::ALL {
        let spec = ActivitySpec::valid(REFERENCE);
        let mut f = fixture_at(moment, &spec);
        let owner = f.env.actors.owner.insecure_clone();
        // The new activity's recipient is the second recipient actor: the fixture's activity
        // already occupies the first.
        let recipient = f.env.actors.recipients[1].pubkey();
        let mint = f.env.win_mint;
        let next_id = f.env.next_activity_id();
        let before = snapshot(&f);

        let ix = create_vesting_ix(owner.pubkey(), recipient, next_id, mint, &spec);
        let r = send(&mut f.env, ix, &owner);

        if moment.before_tge() {
            assert!(
                r.is_ok(),
                "{}: before TGE the set of activities is not final yet: {r:?}",
                moment.label()
            );
            assert_eq!(
                f.env.vesting(next_id).recipient,
                recipient,
                "{}: the activity was created for the given recipient",
                moment.label()
            );
            assert!(
                f.env.vault_exists(next_id),
                "{}: the new activity's vault was created",
                moment.label()
            );
            assert_eq!(
                f.env.vesting_config().activity_count,
                next_id + 1,
                "{}: the id allocator was incremented",
                moment.label()
            );
        } else {
            assert_eq!(
                custom_error_code(&r),
                Some(err(VestingError::ScheduleStarted)),
                "{}: once TGE arrives the set of activities is final: {r:?}",
                moment.label()
            );
            assert_eq!(
                snapshot(&f),
                before,
                "{}: the rejection touched neither the config nor the existing activity",
                moment.label()
            );
            assert!(
                !f.env.vesting_exists(next_id) && !f.env.vault_exists(next_id),
                "{}: the rejection created neither a record nor a vault for the new activity",
                moment.label()
            );
        }
    }
}

#[test]
fn cancel_vesting_is_open_only_before_tge() {
    for moment in Moment::ALL {
        let mut f = fixture_at(moment, &ActivitySpec::valid(REFERENCE));
        let owner = f.env.actors.owner.insecure_clone();
        let withdrawal = f.env.actors.rent_collector.pubkey();
        let mint = f.env.win_mint;
        let id = f.activity_id;
        let beneficiary_token = f.beneficiary_token;
        let before = snapshot(&f);

        let ix = cancel_vesting_ix(owner.pubkey(), id, beneficiary_token, withdrawal, mint);
        let r = send(&mut f.env, ix, &owner);

        if moment.before_tge() {
            assert!(
                r.is_ok(),
                "{}: before TGE a mistaken activity can still be cancelled: {r:?}",
                moment.label()
            );
            assert!(
                !f.env.vesting_exists(id) && !f.env.vault_exists(id),
                "{}: both of the activity's accounts are closed",
                moment.label()
            );
            assert_eq!(
                f.env.token_balance(&beneficiary_token),
                REFERENCE,
                "{}: the whole vault remainder went to the single withdrawal recipient",
                moment.label()
            );
        } else {
            assert_eq!(
                custom_error_code(&r),
                Some(err(VestingError::ScheduleStarted)),
                "{}: after TGE the cancellation is closed - the obligation to the recipient has already arisen: {r:?}",
                moment.label()
            );
            assert_eq!(
                snapshot(&f),
                before,
                "{}: the rejection closed no accounts and moved no balance",
                moment.label()
            );
        }
    }
}

#[test]
fn set_tge_is_open_only_before_tge() {
    for moment in Moment::ALL {
        let mut f = fixture_at(moment, &ActivitySpec::valid(REFERENCE));
        let admin = f.env.actors.admin.insecure_clone();
        // A valid new value (`now < moved <= now + MAX_TGE_DELAY`): the gate under test is
        // the moment of the call, not the argument.
        let moved = f.env.now() + MONTH;
        let before = snapshot(&f);

        let ix = set_tge_ix(admin.pubkey(), moved);
        let r = send(&mut f.env, ix, &admin);

        if moment.before_tge() {
            assert!(
                r.is_ok(),
                "{}: before TGE the date can still be moved: {r:?}",
                moment.label()
            );
            assert_eq!(
                f.env.vesting_config().tge_ts,
                moved,
                "{}: the new date was written into the config",
                moment.label()
            );
        } else {
            assert_eq!(
                custom_error_code(&r),
                Some(err(VestingError::ScheduleStarted)),
                "{}: the gate reads the OLD value - an arrived TGE is not carried over: {r:?}",
                moment.label()
            );
            assert_eq!(
                snapshot(&f),
                before,
                "{}: the rejection did not move the TGE date",
                moment.label()
            );
        }
    }
}

// ===================================================================
// claim: closed before TGE, open from TGE itself
// ===================================================================

#[test]
fn claim_is_closed_before_tge_and_open_from_it() {
    for moment in Moment::ALL {
        let mut f = fixture_at(moment, &ActivitySpec::valid(REFERENCE));
        let recipient = f.env.actors.recipients[0].insecure_clone();
        let id = f.activity_id;
        let before = snapshot(&f);

        let r = f.env.claim_as(&recipient, id, recipient.pubkey());

        if moment.before_tge() {
            assert_eq!(
                custom_error_code(&r),
                Some(err(VestingError::ScheduleNotStarted)),
                "{}: before TGE nothing is unlocked and the claim is closed by time: {r:?}",
                moment.label()
            );
            assert_eq!(
                snapshot(&f),
                before,
                "{}: the rejection released not a single unit",
                moment.label()
            );
            assert!(
                !f.env.recipient_ata_exists(&recipient.pubkey()),
                "{}: the recipient's ATA was not created - `init_if_needed` runs in try_accounts, before the body's checks, and \"there is no partial payout\" rests on the whole transaction reverting",
                moment.label()
            );
        } else {
            // Both at TGE itself and one second later the period is p = 0, so exactly the TGE
            // share is unlocked (the expectations are literals from the reference crate).
            assert!(
                r.is_ok(),
                "{}: once TGE arrives the TGE share is released: {r:?}",
                moment.label()
            );
            assert_eq!(
                f.env.recipient_ata_balance(&recipient.pubkey()),
                V2_TGE_AMT,
                "{}: exactly the TGE share was released to the recipient",
                moment.label()
            );
            assert_eq!(
                f.env.vesting(id).claimed,
                V2_VESTED_DAY_1,
                "{}: claimed == vested(period 0)",
                moment.label()
            );
            assert_eq!(
                f.env.vault_balance(id),
                REFERENCE - V2_TGE_AMT,
                "{}: the vault decreased by exactly what was released",
                moment.label()
            );
        }
    }
}

#[test]
fn claim_at_tge_without_tge_share_has_nothing_to_pay_out() {
    // The second sub-case of the `tge` moment (the first is in
    // `claim_is_closed_before_tge_and_open_from_it`): time is already open, but the
    // schedule has no TGE share, so the answer is `NothingToClaim` rather than
    // `ScheduleNotStarted`. The distinction is load-bearing: one code means "too early",
    // the other "nothing yet", and they must not be confused.
    let mut f = fixture_at(Moment::Exactly, &no_tge_share_spec());
    let recipient = f.env.actors.recipients[0].insecure_clone();
    let id = f.activity_id;
    let before = snapshot(&f);

    let r = f.env.claim_as(&recipient, id, recipient.pubkey());
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NothingToClaim)),
        "tge, a schedule with no TGE share: time is open but there is nothing to release: {r:?}"
    );
    assert_eq!(
        snapshot(&f),
        before,
        "the rejection released not a single unit"
    );
    assert!(
        !f.env.recipient_ata_exists(&recipient.pubkey()),
        "the recipient's ATA was not created: the rejection reverts the account creation too"
    );

    // A POSITIVE in the same environment: at the schedule's terminal period (day
    // 361) the whole reference amount is released - so the rejection above was about the
    // moment rather than a broken environment.
    f.env
        .claim_at_day(f.tge_ts, COMMUNITY_FULL_RELEASE_DAY, id, &recipient)
        .expect("the terminal period releases the whole reference amount");
    assert_eq!(
        f.env.recipient_ata_balance(&recipient.pubkey()),
        V4_VESTED_MONTHS_12,
        "on day 361 the bucket's whole reference amount was released"
    );
    assert_eq!(
        f.env.vesting(id).claimed,
        V4_VESTED_MONTHS_12,
        "claimed == reference_amount after the terminal claim"
    );
}

// ===================================================================
// close_vesting: the gate is the draw-down, not time
// ===================================================================

#[test]
fn close_vesting_is_gated_by_full_claim_not_by_tge() {
    for moment in Moment::ALL {
        // The "100% at TGE" schedule: right after TGE a single claim draws down the whole
        // reference amount, and the row's third cell becomes reachable.
        let mut f = fixture_at(moment, &full_tge_spec());
        let owner = f.env.actors.owner.insecure_clone();
        let recipient = f.env.actors.recipients[0].insecure_clone();
        let withdrawal = f.env.actors.rent_collector.pubkey();
        let mint = f.env.win_mint;
        let id = f.activity_id;
        let beneficiary_token = f.beneficiary_token;

        if matches!(moment, Moment::OneSecondAfter) {
            // One second after TGE: the claim draws down the whole reference amount and the
            // close passes - time does not stand in the close's way in either direction.
            f.env
                .claim_as(&recipient, id, recipient.pubkey())
                .expect("the \"100% at TGE\" schedule: a claim after TGE releases the whole reference amount");
            assert_eq!(
                f.env.vesting(id).claimed,
                V1_VESTED_FROM_TGE,
                "{}: the close's precondition - the activity is fully drawn down",
                moment.label()
            );

            f.env.expire_blockhash();
            let ix = close_vesting_ix(owner.pubkey(), id, beneficiary_token, withdrawal, mint);
            let r = send(&mut f.env, ix, &owner);
            assert!(
                r.is_ok(),
                "{}: a fully drawn activity closes after TGE: {r:?}",
                moment.label()
            );
            assert!(
                !f.env.vesting_exists(id) && !f.env.vault_exists(id),
                "{}: both of the activity's accounts are closed",
                moment.label()
            );
            assert_eq!(
                f.env.token_balance(&beneficiary_token),
                0,
                "{}: it was funded to exactly the reference amount and all of it was released - there was no remainder to hand out",
                moment.label()
            );
        } else {
            // Nothing has been claimed: the rejection must come from the DRAW-DOWN rather than
            // from time - `close_vesting` has no time gate.
            let before = snapshot(&f);
            let ix = close_vesting_ix(owner.pubkey(), id, beneficiary_token, withdrawal, mint);
            let r = send(&mut f.env, ix, &owner);
            assert_eq!(
                custom_error_code(&r),
                Some(err(VestingError::NotFullyClaimed)),
                "{}: claimed == 0 → NotFullyClaimed (neither ScheduleStarted nor ScheduleNotStarted): {r:?}",
                moment.label()
            );
            assert_eq!(
                snapshot(&f),
                before,
                "{}: the rejection closed no accounts and handed out no remainder",
                moment.label()
            );
        }
    }
}

// ===================================================================
// Outside the time gate: set_recipient, set_admin, set_operator, the ownership
// handover, set_withdrawal_authority, assert_funded
// ===================================================================

#[test]
fn set_recipient_is_not_gated_by_tge() {
    for moment in Moment::ALL {
        let mut f = fixture_at(moment, &ActivitySpec::valid(REFERENCE));
        let admin = f.env.actors.admin.insecure_clone();
        let new_recipient = f.env.actors.recipients[1].pubkey();
        let id = f.activity_id;
        let claimed_before = f.env.vesting(id).claimed;

        let ix = set_recipient_ix(admin.pubkey(), id, new_recipient);
        let r = send(&mut f.env, ix, &admin);
        assert!(
            r.is_ok(),
            "{}: the payout addressee is editable throughout the activity's life - otherwise a mistake in the recipient could not be fixed after TGE: {r:?}",
            moment.label()
        );

        let v = f.env.vesting(id);
        assert_eq!(
            v.recipient,
            new_recipient,
            "{}: the activity's recipient really did change",
            moment.label()
        );
        assert_eq!(
            v.reference_amount,
            REFERENCE,
            "{}: the obligation's amount is untouched - no instruction changes it",
            moment.label()
        );
        assert_eq!(
            v.claimed,
            claimed_before,
            "{}: the accounting of what was released is untouched",
            moment.label()
        );
        assert_eq!(
            f.env.vault_balance(id),
            REFERENCE,
            "{}: changing the recipient moves no funds",
            moment.label()
        );
    }
}

#[test]
fn set_admin_is_not_gated_by_tge() {
    for moment in Moment::ALL {
        let mut f = fixture_at(moment, &ActivitySpec::valid(REFERENCE));
        let owner = f.env.actors.owner.insecure_clone();
        // A non-zero key distinct from Owner and operator: the argument checks are satisfied.
        let fresh_admin = Pubkey::new_unique();

        let ix = set_admin_ix(owner.pubkey(), fresh_admin);
        let r = send(&mut f.env, ix, &owner);
        assert!(
            r.is_ok(),
            "{}: rotating the admin does not depend on time - otherwise a compromised key could not be replaced after TGE: {r:?}",
            moment.label()
        );
        assert_eq!(
            f.env.vesting_config().admin,
            fresh_admin,
            "{}: admin really was reassigned",
            moment.label()
        );
    }
}

#[test]
fn set_operator_is_not_gated_by_tge() {
    for moment in Moment::ALL {
        let mut f = fixture_at(moment, &ActivitySpec::valid(REFERENCE));
        let owner = f.env.actors.owner.insecure_clone();
        // A non-zero key distinct from Owner: the argument gates are satisfied.
        let fresh_operator = Pubkey::new_unique();

        let ix = set_operator_ix(owner.pubkey(), fresh_operator);
        let r = send(&mut f.env, ix, &owner);
        assert!(
            r.is_ok(),
            "{}: rotating the operator is not gated by time - otherwise a compromised key could not be replaced after TGE: {r:?}",
            moment.label()
        );
        assert_eq!(
            f.env.vesting_config().operator,
            fresh_operator,
            "{}: operator really was reassigned",
            moment.label()
        );
    }
}

#[test]
fn owner_handover_is_not_gated_by_tge() {
    // All three steps of the ownership handover - the nomination, its withdrawal and the
    // candidate's acceptance - are executed at each of the three moments.
    for moment in Moment::ALL {
        let mut f = fixture_at(moment, &ActivitySpec::valid(REFERENCE));
        let owner = f.env.actors.owner.insecure_clone();
        let operator = f.env.vesting_config().operator;
        // The candidate must be able to sign the acceptance - we take a wallet with a key.
        let candidate = f.env.new_wallet();
        assert_ne!(
            candidate.pubkey(),
            operator,
            "the argument gate's precondition: the candidate ≠ operator"
        );

        let ix = propose_owner_ix(owner.pubkey(), candidate.pubkey());
        let r = send(&mut f.env, ix, &owner);
        assert!(
            r.is_ok(),
            "{}: nominating an owner is not gated by time: {r:?}",
            moment.label()
        );
        assert_eq!(
            f.env.vesting_config().pending_owner,
            Some(candidate.pubkey()),
            "{}: the candidate was recorded",
            moment.label()
        );

        f.env.expire_blockhash();
        let r = send(&mut f.env, cancel_owner_proposal_ix(owner.pubkey()), &owner);
        assert!(
            r.is_ok(),
            "{}: withdrawing the nomination is not gated by time: {r:?}",
            moment.label()
        );
        assert_eq!(
            f.env.vesting_config().pending_owner,
            None,
            "{}: the candidate was withdrawn",
            moment.label()
        );

        f.env.expire_blockhash();
        let ix = propose_owner_ix(owner.pubkey(), candidate.pubkey());
        send(&mut f.env, ix, &owner).expect("a repeated nomination of the same candidate");

        f.env.expire_blockhash();
        let r = send(&mut f.env, accept_owner_ix(candidate.pubkey()), &candidate);
        assert!(
            r.is_ok(),
            "{}: the candidate's acceptance is not gated by time: {r:?}",
            moment.label()
        );
        let c = f.env.vesting_config();
        assert_eq!(
            c.owner,
            candidate.pubkey(),
            "{}: ownership really was handed over",
            moment.label()
        );
        assert_eq!(
            c.pending_owner,
            None,
            "{}: the candidate was cleared by the acceptance",
            moment.label()
        );
    }
}

#[test]
fn set_withdrawal_authority_is_not_gated_by_tge() {
    for moment in Moment::ALL {
        let mut f = fixture_at(moment, &ActivitySpec::valid(REFERENCE));
        let admin = f.env.actors.admin.insecure_clone();
        // A wallet outside the vaults' PDA: the address check is satisfied.
        let fresh_target = f.env.new_wallet().pubkey();

        let ix = set_withdrawal_authority_ix(admin.pubkey(), fresh_target);
        let r = send(&mut f.env, ix, &admin);
        assert!(
            r.is_ok(),
            "{}: changing the withdrawal target is not restricted by time - otherwise, with a lost key, the remainder and rent of closing activities would go nowhere: {r:?}",
            moment.label()
        );
        assert_eq!(
            f.env.vesting_config().withdrawal_authority,
            fresh_target,
            "{}: the withdrawal target really did change",
            moment.label()
        );
    }
}

#[test]
fn assert_funded_is_not_gated_by_tge() {
    for moment in Moment::ALL {
        let mut f = fixture_at(moment, &ActivitySpec::valid(REFERENCE));
        let payer = f.env.payer.insecure_clone();
        let mint = f.env.win_mint;
        let id = f.activity_id;
        let before = snapshot(&f);

        // The instruction has no Signer slot (Arch §9.9): any key can send it as the fee
        // payer, at any point in the program's life.
        let r = f
            .env
            .send_ixs(&[assert_funded_ix(id, mint)], &payer.pubkey(), &[&payer]);
        assert!(
            r.is_ok(),
            "{}: the funding check is not gated by time - it is needed both before TGE (in the minting transaction) and after: {r:?}",
            moment.label()
        );
        assert_eq!(
            snapshot(&f),
            before,
            "{}: assert_funded changes no state - it is a check, not an operation",
            moment.label()
        );
    }
}
