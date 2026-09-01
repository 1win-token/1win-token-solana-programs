//! Releasing the unlocked $1WIN: `claim` (Arch §9.8, card I9).
//! Checking the five reference schedules of §10.5 on-chain lives in its own file,
//! `schedules_reference.rs`.
//!
//! Why the tests are laid out the way they are.
//!
//! 1. The expectations are literals from the `vesting-expected` crate rather
//!    than a recomputation of the formula next to the test: `vested()` is never
//!    reproduced in the tests, or the equality would be a tautology
//!    (tests-bind-to-real-code).
//! 2. Invariant I1 (`claimed == vested(now)`, with a `reference_amount` ceiling) is
//!    proven by BEHAVIOUR rather than by recomputation: a repeat of the claim at the
//!    same instant, a claim inside an already-paid period, and a claim long after the
//!    full release - all three must answer `NothingToClaim`, and the ATA must hold
//!    exactly `reference_amount`.
//! 3. The lamport assertions (who paid the ATA's rent) are taken with the
//!    environment's `payer` as the fee payer: were the actor to sign the transaction
//!    itself, the fee would mix into the rent and "paid exactly the rent" would become
//!    unverifiable.
//! 4. The ATA's rent is neither a literal nor a computation from the account's length
//!    but the created account's actual lamports: it is rent-exempt, so what sits on it
//!    is exactly what was debited from the payer.
//! 5. The I3/I4 negatives also assert the ABSENCE of a side effect: on a rejection the
//!    recipient's ATA must not come into being at all - `init_if_needed` runs in
//!    `try_accounts`, that is BEFORE the body's checks, and "there is no partial
//!    payout" rests solely on the whole transaction reverting.
//! 6. The authorization negatives submit a foreign key BOTH into the `signer` slot AND
//!    into the signers: pass it only into the slot and the rejection would come
//!    from the missing signature. The `recipient` slot stays CORRECT meanwhile, or the
//!    failure would come earlier, on the `address = vesting.recipient` binding.
//! 7. Activity isolation (I2) is checked on TWO funded activities with different
//!    reference amounts: on a single one substituting the vault is unobservable.
//! 8. The authorization negatives submit a foreign key and therefore check only the
//!    comparison in the body, not the `signer` slot's type. A separate case clears the
//!    signature from the CORRECT key: without it, changing the type to
//!    `UncheckedAccount` would let an outsider force a claim on the recipient's behalf.
//! 9. The period boundaries are checked to the second rather than to the day:
//!    `warp_to_day` only lands on the start of a day, so the last second of a period is
//!    set by the offset `tge_ts + MONTH − 1`.
//!
//! ===== DELIBERATELY DEFERRED =====
//! - The 10×5 role matrix and the 10×3 TGE matrix.
//! - Substituting `vault_authority` and the full slot substitution matrix.
//! - The claim's CU benchmark (a ceiling from the measurement).
//! - The "random claim moments" property lifecycle: pinpoint cases from
//!   the architecture's numeric examples stand here, the property sweep stands there.

use anchor_lang::error::ErrorCode;
use litesvm::types::TransactionResult;
use solana_instruction_error::InstructionError;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;
use vesting::errors::VestingError;
use vesting::events::Claimed;
use vesting_test_common::vesting_expected::vesting::{
    COMMUNITY_CLIFF_MONTHS, COMMUNITY_MONTHLY_BPS, COMMUNITY_TGE_BPS, ECOSYSTEM_FIRST_MONTHLY_DAY,
    ECOSYSTEM_FULL_RELEASE_DAY, MONTH, V2_MONTHLY_AMT, V2_REFERENCE, V2_TGE_AMT, V2_VESTED_DAY_1,
    V2_VESTED_DAY_151, V2_VESTED_DAY_31, V2_VESTED_DAY_61, V4_REFERENCE, V5_CLAIMABLE_DAY_541,
    V5_CLAIMED_AFTER_DAY_151, V5_CLAIMED_AFTER_DAY_541,
};
use vesting_test_common::{
    claim_ix, claim_ix_with_slots, config_pda, custom_error_code, decode_event, err, recipient_ata,
    set_operator_ix, set_recipient_ix, vault_authority_pda, vault_pda, vesting_pda, without_signer,
    ActivitySpec, TestEnv, DEFAULT_TGE_OFFSET, TOKEN_2022_ID,
};

/// The activity's reference amount: the V2 bucket of §10.5 (10⁸ $1WIN (100,000,000) in
/// token units) - the same one the architecture's numeric examples V2 and V5 are built
/// on.
const REFERENCE: u64 = V2_REFERENCE;

/// The day of the first claim in example V5: the recipient does not claim until day
/// 151, and a single call releases what accrued (TGE plus five tranches). The day
/// number comes from the text of the architecture's example V5 and the amount from
/// [`V2_VESTED_DAY_151`].
const DAY_151: i64 = 151;

/// A day inside the already-paid period 1 (`p = floor(44/30) = 1`, the same as day
/// 31's): a tranche does not leak second by second.
const DAY_45: i64 = 45;

/// A day long after the full release (`p` is capped by `min(·, M)`): a check that
/// nothing beyond the reference amount is released even years later.
const DAY_2000: i64 = 2000;

// The three reference values are taken from `vesting-expected` independently of one
// another, and the second-boundary case reads the increment as a separate literal
// `V2_MONTHLY_AMT`. A compile-time check: should the references drift apart, the file
// will not build instead of quietly checking something else.
const _: () = assert!(
    V2_VESTED_DAY_1 + V2_MONTHLY_AMT == V2_VESTED_DAY_31,
    "the references are inconsistent: the TGE share plus one monthly tranche must give what is unlocked on day 31"
);

/// The keys `claim` must reject: none of them is the activity's recipient or the
/// operator (Arch §8).
fn wrong_claimers(env: &TestEnv) -> Vec<(&'static str, Keypair)> {
    vec![
        ("owner", env.actors.owner.insecure_clone()),
        ("beneficiary", env.actors.beneficiary.insecure_clone()),
        ("rent_collector", env.actors.rent_collector.insecure_clone()),
        ("stranger", env.actors.stranger.insecure_clone()),
    ]
}

/// An environment with a config (TGE in a month), an activity on the "Ecosystem"
/// schedule for [`REFERENCE`], and a vault funded to exactly the reference amount.
/// Returns `(env, activity_id, tge_ts)`.
fn setup() -> (TestEnv, u64, i64) {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let activity_id = env.create_activity(&ActivitySpec::valid(REFERENCE));
    env.fund_vault(activity_id, REFERENCE);
    (env, activity_id, params.tge_ts)
}

/// A claim by the activity's recipient (`actors.recipients[0]`) - the main scenario.
/// Sent through [`TestEnv::claim_as`]: the actor's key goes BOTH into the `signer` slot
/// AND into the signers, and the fee payer is the environment's `payer` (otherwise the
/// fee would mix into the ATA's rent in the lamport assertions).
#[allow(clippy::result_large_err)]
fn claim_by_recipient(env: &mut TestEnv, activity_id: u64) -> TransactionResult {
    let recipient = env.actors.recipients[0].insecure_clone();
    env.claim_as(&recipient, activity_id, recipient.pubkey())
}

/// The observable state around the activity: the accounting, the vault and the
/// recipient's account. Comparing the snapshots before and after is the "the rejection
/// left no trace" assertion in a single line.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    claimed: u64,
    vault_balance: u64,
    recipient_ata_exists: bool,
    recipient_ata_balance: u64,
}

fn snapshot(env: &TestEnv, activity_id: u64, recipient: &Pubkey) -> Snapshot {
    Snapshot {
        claimed: env.vesting(activity_id).claimed,
        vault_balance: env.vault_balance(activity_id),
        recipient_ata_exists: env.recipient_ata_exists(recipient),
        recipient_ata_balance: env.recipient_ata_balance(recipient),
    }
}

// ===================================================================
// the positive, signed by the recipient
// ===================================================================

#[test]
fn claim_by_recipient_pays_out_accumulated_amount() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].insecure_clone();
    let ata = recipient_ata(&recipient.pubkey(), &env.win_mint);
    assert!(
        !env.account_exists(&ata),
        "precondition: the recipient's ATA does not exist yet"
    );

    env.warp_to_day(tge_ts, DAY_151);
    let recipient_lamports_before = env.lamports_of(&recipient.pubkey());

    let r = claim_by_recipient(&mut env, id);
    r.as_ref().expect("the recipient claims its own activity");

    // The expectation is a literal from the reference crate: the TGE share plus five
    // monthly tranches (the architecture's example V5), computed by hand rather than by
    // the program's formula.
    assert_eq!(
        env.token_balance(&ata),
        V2_VESTED_DAY_151,
        "exactly what accrued by day 151 arrived at the recipient's ATA"
    );
    assert_eq!(
        env.vesting(id).claimed,
        V5_CLAIMED_AFTER_DAY_151,
        "claimed == vested(now) after the claim (I1)"
    );
    assert_eq!(
        env.vault_balance(id),
        REFERENCE - V2_VESTED_DAY_151,
        "the vault decreased by exactly what was released"
    );

    // The signer paid the ATA's rent: the account is rent-exempt, so the lamports sitting
    // on it are exactly what was debited. The fee does not mix in here - the fee payer is
    // a separate one (see `claim_as`).
    let ata_rent = env.lamports_of(&ata);
    assert!(ata_rent > 0, "the ATA was created and is rent-exempt");
    assert_eq!(
        recipient_lamports_before - env.lamports_of(&recipient.pubkey()),
        ata_rent,
        "the recipient itself paid the ATA's rent (payer = signer)"
    );

    let ev: Claimed = decode_event(&r).expect("claim emits Claimed");
    assert_eq!(
        (ev.activity_id, ev.recipient, ev.amount, ev.claimed),
        (
            id,
            recipient.pubkey(),
            V2_VESTED_DAY_151,
            V5_CLAIMED_AFTER_DAY_151
        ),
        "the event carries the released amount and the accumulated total AFTER the increment"
    );
}

// ===================================================================
// the positive, signed by the operator
// ===================================================================

#[test]
fn claim_by_operator_pays_recipient_and_costs_operator_the_ata_rent() {
    let (mut env, id, tge_ts) = setup();
    let operator = env.actors.operator.insecure_clone();
    let recipient = env.actors.recipients[0].pubkey();
    let recipient_ata_addr = recipient_ata(&recipient, &env.win_mint);
    let operator_ata = recipient_ata(&operator.pubkey(), &env.win_mint);

    env.warp_to_day(tge_ts, DAY_151);
    let operator_lamports_before = env.lamports_of(&operator.pubkey());
    let recipient_lamports_before = env.lamports_of(&recipient);

    let r = env.claim_as(&operator, id, recipient);
    r.as_ref()
        .expect("the operator has the right to trigger a claim (Arch §8)");

    // The key property of I4: the operator triggers the payout, but the funds go to the
    // recipient - to its ATA rather than to the operator's account.
    assert_eq!(
        env.token_balance(&recipient_ata_addr),
        V2_VESTED_DAY_151,
        "the tokens went to the RECIPIENT's ATA"
    );
    assert!(
        !env.account_exists(&operator_ata),
        "the operator's ATA was never created - the funds have nothing to do with it"
    );

    let ata_rent = env.lamports_of(&recipient_ata_addr);
    assert_eq!(
        operator_lamports_before - env.lamports_of(&operator.pubkey()),
        ata_rent,
        "the operator paid the rent of the recipient's ATA (the operator's operational cost, VE-12)"
    );
    assert_eq!(
        env.lamports_of(&recipient),
        recipient_lamports_before,
        "the recipient's lamports did not change: the operator gets no rent refund, but the recipient pays nothing either"
    );

    let ev: Claimed = decode_event(&r).expect("claim emits Claimed");
    assert_eq!(
        ev.recipient, recipient,
        "the event carries the activity's recipient rather than the signer"
    );
}

// ===================================================================
// the continuation of V5: the second claim takes the remainder
// ===================================================================

#[test]
fn second_claim_at_full_release_pays_the_remainder() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    let ata = recipient_ata(&recipient, &env.win_mint);

    env.warp_to_day(tge_ts, DAY_151);
    claim_by_recipient(&mut env, id).expect("the first claim passes");
    let after_first = env.token_balance(&ata);

    env.warp_to_day(tge_ts, ECOSYSTEM_FULL_RELEASE_DAY);
    env.expire_blockhash();
    let r = claim_by_recipient(&mut env, id);
    r.as_ref().expect("the second claim in the terminal period");

    let delta = env.token_balance(&ata) - after_first;
    assert_eq!(
        delta, V5_CLAIMABLE_DAY_541,
        "the second claim released exactly the obligation's remainder (example V5)"
    );
    assert_eq!(
        env.token_balance(&ata),
        V5_CLAIMED_AFTER_DAY_541,
        "the sum of the two transfers == reference_amount: the terminal branch topped up everything"
    );
    assert_eq!(
        env.vesting(id).claimed,
        REFERENCE,
        "claimed == reference_amount - the activity became closable (I9)"
    );
    assert_eq!(
        env.vault_balance(id),
        0,
        "the vault emptied out to exactly zero: as much was released as had been minted"
    );

    let ev: Claimed = decode_event(&r).expect("claim emits Claimed");
    assert_eq!(
        (ev.amount, ev.claimed),
        (V5_CLAIMABLE_DAY_541, REFERENCE),
        "the second claim's event: the payout is the remainder and the total is the reference amount"
    );
}

// ===================================================================
// [I1] - nothing beyond what is unlocked is released
// ===================================================================

#[test]
fn immediate_repeat_claim_yields_nothing() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    env.warp_to_day(tge_ts, DAY_151);
    claim_by_recipient(&mut env, id).expect("the first claim passes");
    let before = snapshot(&env, id, &recipient);

    // A fresh blockhash is mandatory: without it a byte-for-byte repeat would be filtered
    // out by the history as AlreadyProcessed BEFORE execution.
    env.expire_blockhash();
    let r = claim_by_recipient(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NothingToClaim)),
        "a repeat at the same instant: claimable == 0 → NothingToClaim: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the rejection did not release a single unit a second time"
    );
}

#[test]
fn two_claims_in_one_transaction_revert_whole_transaction_on_the_second() {
    // The test pins the persistence of `claimed` BETWEEN the instructions of a single
    // transaction: Anchor serializes the record back into the account's buffer after the
    // first instruction, and the second reads the already-updated value. A regression
    // (an AccountLoader without a commit, hand-rolled serialization, a lost &mut) would
    // turn a repeat within one tx into a double payout - the cheapest double-spend there
    // is.
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].insecure_clone();
    env.warp_to_day(tge_ts, DAY_151);
    let vault_before = env.vault_balance(id);

    let ix = claim_ix(recipient.pubkey(), id, recipient.pubkey(), env.win_mint);
    let r = env.send_ixs(&[ix.clone(), ix], &recipient.pubkey(), &[&recipient]);

    // It is precisely the SECOND instruction (index 1) that fails, and precisely with
    // NothingToClaim: the first released everything accrued and for the second
    // claimable == 0.
    match &r {
        Err(failed) => match failed.err {
            TransactionError::InstructionError(index, InstructionError::Custom(code)) => {
                assert_eq!(
                    index, 1,
                    "the second instruction must fail, not the first: {r:?}"
                );
                assert_eq!(
                    code,
                    err(VestingError::NothingToClaim),
                    "the second claim in the same tx sees the updated claimed → NothingToClaim: {r:?}"
                );
            }
            _ => panic!("a Custom code was expected on the second instruction: {r:?}"),
        },
        Ok(_) => panic!("two claims in one tx must not pass: the second must fail"),
    }

    // Atomicity: the second instruction's revert rolled back the successful first one too.
    assert_eq!(
        env.vesting(id).claimed,
        0,
        "the first instruction's payout was rolled back along with the transaction"
    );
    assert!(
        !env.recipient_ata_exists(&recipient.pubkey()),
        "the recipient's ATA does not exist: the first instruction's creation was rolled back"
    );
    assert_eq!(
        env.vault_balance(id),
        vault_before,
        "the vault lost not a single unit"
    );
}

#[test]
fn claim_inside_an_already_paid_period_yields_nothing() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    let ata = recipient_ata(&recipient, &env.win_mint);

    env.warp_to_day(tge_ts, ECOSYSTEM_FIRST_MONTHLY_DAY);
    claim_by_recipient(&mut env, id).expect("a claim on the day of the first monthly tranche");
    assert_eq!(
        env.token_balance(&ata),
        V2_VESTED_DAY_31,
        "TGE plus one tranche was released"
    );
    let before = snapshot(&env, id, &recipient);

    // Day 45 is the same period `p = 1` as day 31: a tranche accrues in full on the
    // period's boundary and does not leak second by second.
    env.warp_to_day(tge_ts, DAY_45);
    env.expire_blockhash();
    let r = claim_by_recipient(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NothingToClaim)),
        "inside an already-paid period there is nothing to claim: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the state inside the period did not change"
    );

    // A POSITIVE in the same environment: the next period brings exactly one
    // monthly tranche.
    env.warp_to_day(tge_ts, ECOSYSTEM_FIRST_MONTHLY_DAY + 30);
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("the next period opens a new tranche");
    assert_eq!(
        env.token_balance(&ata),
        V2_VESTED_DAY_61,
        "exactly one monthly tranche was added"
    );
}

#[test]
fn claim_long_after_full_release_yields_nothing_above_reference() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    let ata = recipient_ata(&recipient, &env.win_mint);

    env.warp_to_day(tge_ts, ECOSYSTEM_FULL_RELEASE_DAY);
    claim_by_recipient(&mut env, id).expect("the claim in the terminal period releases everything");
    assert_eq!(
        env.token_balance(&ata),
        REFERENCE,
        "the whole reference amount was released"
    );

    // The snapshot AFTER the setup (the terminal claim has already happened) - before the
    // call that gets rejected.
    env.warp_to_day(tge_ts, DAY_2000);
    env.expire_blockhash();
    let before = snapshot(&env, id, &recipient);
    let r = claim_by_recipient(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NothingToClaim)),
        "years after the full release there is nothing to claim: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the rejection touched neither the accounting, nor the vault, nor the recipient's account"
    );
    assert_eq!(
        env.token_balance(&ata),
        REFERENCE,
        "the ATA holds exactly reference_amount - the I1 ceiling is not broken by time"
    );
    assert_eq!(
        env.vesting(id).claimed,
        REFERENCE,
        "claimed hit the reference amount"
    );
}

// ===================================================================
// the time boundaries and different codes for different causes (P-14)
// ===================================================================

#[test]
fn claim_one_second_before_tge_is_rejected() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    env.warp_to(tge_ts - 1);
    let before = snapshot(&env, id, &recipient);

    let r = claim_by_recipient(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::ScheduleNotStarted)),
        "now == tge − 1 → ScheduleNotStarted: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "before TGE there is no payout"
    );

    // A POSITIVE in the same environment: the gate is inclusive - exactly at TGE
    // the claim passes and releases the TGE share.
    env.warp_to(tge_ts);
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("at the very second of TGE the claim is open");
    assert_eq!(
        env.token_balance(&recipient_ata(&recipient, &env.win_mint)),
        V2_TGE_AMT,
        "at the TGE instant exactly the TGE share was released (day 1, period 0)"
    );
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_1);
}

#[test]
fn monthly_tranche_opens_exactly_at_the_period_boundary_second() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    let ata = recipient_ata(&recipient, &env.win_mint);

    // The TGE share is drawn immediately, so that afterwards exactly a tranche's
    // increment is observed.
    env.warp_to(tge_ts);
    claim_by_recipient(&mut env, id).expect("at the TGE instant the TGE share is released");
    assert_eq!(
        env.token_balance(&ata),
        V2_VESTED_DAY_1,
        "precondition: the account holds exactly the TGE share"
    );
    let before = snapshot(&env, id, &recipient);

    // A period's boundary is checked by a REAL claim and to the second: the nearest
    // day-based case (day 45) sits in the middle of an already-paid period and would not
    // notice the boundary shifting by a second. `p = (MONTH − 1) / MONTH = 0` - the same
    // period 0 as at the TGE instant.
    env.warp_to(tge_ts + MONTH - 1);
    env.expire_blockhash();
    let r = claim_by_recipient(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NothingToClaim)),
        "one second before the end of period 0 there is nothing to claim: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the last second of period 0 released not a single unit"
    );

    // One second later it is period 1: exactly one monthly tranche arrives, no more and no
    // less. The expectation is an independent literal from the reference crate.
    env.warp_to(tge_ts + MONTH);
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("the first second of period 1 opens a tranche");
    assert_eq!(
        env.token_balance(&ata) - before.recipient_ata_balance,
        V2_MONTHLY_AMT,
        "the payout's increment is exactly one monthly tranche"
    );
    assert_eq!(
        env.token_balance(&ata),
        V2_VESTED_DAY_31,
        "exactly what is unlocked on day 31 has accrued"
    );
    assert_eq!(
        env.vesting(id).claimed,
        V2_VESTED_DAY_31,
        "claimed == vested(now) after the claim (I1)"
    );
}

#[test]
fn claim_at_tge_without_tge_share_yields_nothing_to_claim() {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    // The Community Rewards schedule: `tge_bps = 0`, so at the TGE instant nothing is unlocked
    // even though the time gate is already satisfied.
    let spec = ActivitySpec {
        reference_amount: REFERENCE,
        tge_bps: COMMUNITY_TGE_BPS,
        monthly_bps: COMMUNITY_MONTHLY_BPS,
        cliff_months: COMMUNITY_CLIFF_MONTHS,
        label: String::from("community"),
    };
    let id = env.create_activity(&spec);
    env.fund_vault(id, REFERENCE);
    let recipient = env.actors.recipients[0].pubkey();

    env.warp_to(params.tge_ts);
    let before = snapshot(&env, id, &recipient);
    let r = claim_by_recipient(&mut env, id);
    // P-14: different codes for different causes. The time has come, so
    // `ScheduleNotStarted` would be the wrong diagnosis here - what is unlocked is zero.
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NothingToClaim)),
        "tge_bps == 0 at the TGE instant → NothingToClaim rather than ScheduleNotStarted: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the rejection touched neither the accounting, nor the vault, nor the recipient's account"
    );
    assert!(
        !env.account_exists(&recipient_ata(&recipient, &env.win_mint)),
        "the rejection rolled back the ATA's creation too"
    );

    // A POSITIVE in the same environment: the first monthly tranche opens the
    // payout.
    env.warp_to_day(params.tge_ts, ECOSYSTEM_FIRST_MONTHLY_DAY);
    env.expire_blockhash();
    claim_by_recipient(&mut env, id)
        .expect("on day 31 a schedule with no TGE share already has a tranche");
    assert!(
        env.vesting(id).claimed > 0,
        "the payout began with exactly the first monthly tranche"
    );
}

// ===================================================================
// a non-positive tge_ts does not open the unlock
// ===================================================================

#[test]
fn claim_rejects_non_positive_tge_in_config() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();

    // A forge: neither zero nor a negative value is reachable through the public
    // interface - `initialize` and `set_tge` accept only `tge_ts > now` (Arch §4.3). The
    // gate in `claim` is a safeguard against a refactor, and without this test it would
    // stay dead code.
    //
    // Both values are checked because the gate closes a CLASS rather than a point: zero
    // means "TGE is not set" and a negative value is an instant before the Unix epoch.
    // Rejecting zero alone would let `tge_ts = -1` through, where `now >= tge_ts` holds
    // and `vested()` would give a period far beyond the terminal one - that is, the whole
    // reference amount in a single call.
    //
    // In every case the time is deliberately "after" the forged TGE.
    env.warp_to_day(tge_ts, DAY_151);
    let before = snapshot(&env, id, &recipient);

    for (label, forged) in [
        ("zero means \"TGE is not set\", not \"1970\"", 0_i64),
        (
            "a negative value is an instant before the Unix epoch",
            -1_i64,
        ),
    ] {
        env.expire_blockhash();
        env.forge_tge_ts(forged);
        let r = claim_by_recipient(&mut env, id);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::ScheduleNotStarted)),
            "tge_ts == {forged} ({label}) → ScheduleNotStarted: {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &recipient),
            before,
            "tge_ts == {forged}: not a single unit was released"
        );
    }

    // A POSITIVE in the same environment: we restore the real date and the claim
    // goes through.
    env.forge_tge_ts(tge_ts);
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("with a TGE set the claim passes");
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_151);
}

// ===================================================================
// [I3] - an insufficient vault balance: a full revert
// ===================================================================

#[test]
fn claim_rejects_vault_short_by_one_unit() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    let ata = recipient_ata(&recipient, &env.win_mint);
    env.warp_to_day(tge_ts, DAY_151);

    // The under-mint is produced by a forge: the vault's balance is only ever
    // moved down by `claim` and by a cancellation or close, and both stay consistent with
    // the accounting.
    env.set_vault_amount(id, V2_VESTED_DAY_151 - 1);
    let before = snapshot(&env, id, &recipient);

    let r = claim_by_recipient(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "a vault one unit below what is due → InsufficientVaultBalance: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the accounting and the vault are untouched: there is no partial payout"
    );
    assert!(
        !env.account_exists(&ata),
        "the recipient's ATA was NOT created: init_if_needed runs in try_accounts, but the body's revert rolls it back too"
    );

    // A POSITIVE in the same environment: the missing unit lifts the rejection.
    let win_mint = env.win_mint;
    env.mint_to(&win_mint, &vault_pda(id), 1);
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("topping up by one token unit opens the payout");
    assert_eq!(env.token_balance(&ata), V2_VESTED_DAY_151);
    assert_eq!(
        env.vault_balance(id),
        0,
        "everything that was in the vault was released"
    );
}

#[test]
fn claim_rejects_underfunded_vault_on_the_terminal_tranche() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    env.warp_to_day(tge_ts, DAY_151);
    claim_by_recipient(&mut env, id).expect("the first claim passes");

    // The terminal period demands the remainder in full; we understate the vault by one
    // unit.
    env.warp_to_day(tge_ts, ECOSYSTEM_FULL_RELEASE_DAY);
    env.set_vault_amount(id, V5_CLAIMABLE_DAY_541 - 1);
    let before = snapshot(&env, id, &recipient);

    env.expire_blockhash();
    let r = claim_by_recipient(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::InsufficientVaultBalance)),
        "the terminal tranche is all-or-nothing too: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "an underfunded terminal claim did not release a part"
    );
    assert_eq!(
        env.vesting(id).claimed,
        V5_CLAIMED_AFTER_DAY_151,
        "claimed stayed at the first claim's value"
    );
}

// ===================================================================
// [I4] - the funds go to the recipient only
// ===================================================================

#[test]
fn operator_cannot_redirect_payout_to_own_ata() {
    let (mut env, id, tge_ts) = setup();
    let operator = env.actors.operator.insecure_clone();
    let recipient = env.actors.recipients[0].pubkey();
    let win_mint = env.win_mint;
    // The operator's ATA is created the honest way - the ATA program does not ask for the
    // owner's signature, so such an account is created in advance without Vesting's
    // involvement.
    let operator_ata = env.create_ata(&operator.pubkey(), &win_mint);
    env.warp_to_day(tge_ts, DAY_151);
    let before = snapshot(&env, id, &recipient);

    // The `recipient` slot is correct and only the ATA addressee was substituted: without
    // the `associated_token::authority = recipient` binding the transfer would go to the
    // operator.
    let ix = claim_ix_with_slots(
        operator.pubkey(),
        config_pda(),
        vesting_pda(id),
        vault_pda(id),
        recipient,
        operator_ata,
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id,
    );
    let payer = env.payer.insecure_clone();
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &operator]);
    // The code is pinned by the first execution: on an existing account `init_if_needed`
    // takes the "already created" branch and checks the authority -
    // `ConstraintTokenOwner` (2015) rather than the address derivation
    // (`ConstraintAssociated`, 2009). The observable property does not change: the
    // rejection happens in `try_accounts`, before the body and before the transfer.
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintTokenOwner)),
        "a foreign ATA in the addressee slot → a binding error BEFORE the transfer: without it a compromise of the operator would become theft: {r:?}"
    );
    assert_eq!(
        env.token_balance(&operator_ata),
        0,
        "not a single unit reached the operator's account"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the vault and the accounting are untouched"
    );

    // A POSITIVE in the same environment: the same operator with the recipient's
    // canonical ATA passes - the rejection was about the addressee, not about the role.
    env.expire_blockhash();
    env.claim_as(&operator, id, recipient)
        .expect("the operator claims into the recipient's ATA");
    assert_eq!(
        env.token_balance(&recipient_ata(&recipient, &win_mint)),
        V2_VESTED_DAY_151
    );
    assert_eq!(env.token_balance(&operator_ata), 0);
}

#[test]
fn claim_rejects_foreign_key_in_recipient_slot() {
    let (mut env, id, tge_ts) = setup();
    let operator = env.actors.operator.insecure_clone();
    let recipient = env.actors.recipients[0].pubkey();
    env.warp_to_day(tge_ts, DAY_151);
    let before = snapshot(&env, id, &recipient);

    // A consistent substitution: both the `recipient` slot and the ATA belong to the
    // operator. Such a pair would satisfy the `associated_token::authority` binding; what
    // rejects it is `address = vesting.recipient` - the address comes from the STORED
    // field.
    let ix = claim_ix(operator.pubkey(), id, operator.pubkey(), env.win_mint);
    let payer = env.payer.insecure_clone();
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &operator]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintAddress)),
        "a foreign key in the recipient slot → ConstraintAddress (2012): {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "substituting the recipient did not touch the state"
    );
    assert!(
        !env.account_exists(&recipient_ata(&operator.pubkey(), &env.win_mint)),
        "the operator's ATA was not even created"
    );
}

// ===================================================================
// [I2] - activity isolation
// ===================================================================

/// Two funded activities with DIFFERENT recipients and amounts. Returns
/// `(env, id_a, id_b, tge_ts)`.
fn setup_two() -> (TestEnv, u64, u64, i64) {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let first = env.actors.recipients[0].pubkey();
    let second = env.actors.recipients[1].pubkey();
    let id_a = env.create_activity_for(&first, &ActivitySpec::valid(REFERENCE));
    let id_b = env.create_activity_for(&second, &ActivitySpec::valid(REFERENCE));
    env.fund_vault(id_a, REFERENCE);
    env.fund_vault(id_b, REFERENCE);
    env.warp_to_day(params.tge_ts, DAY_151);
    (env, id_a, id_b, params.tge_ts)
}

#[test]
fn claim_rejects_vault_of_another_activity() {
    let (mut env, id_a, id_b, _tge) = setup_two();
    let recipient = env.actors.recipients[0].insecure_clone();
    let recipient_b = env.actors.recipients[1].pubkey();
    let win_mint = env.win_mint;
    let before_a = snapshot(&env, id_a, &recipient.pubkey());
    let before_b = snapshot(&env, id_b, &recipient_b);

    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        vesting_pda(id_a),
        vault_pda(id_b),
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id_a,
    );
    let payer = env.payer.insecure_clone();
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &recipient]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "another activity's vault → ConstraintSeeds: the address is derived from the same activity_id as the record (I2): {r:?}"
    );
    assert_eq!(
        snapshot(&env, id_a, &recipient.pubkey()),
        before_a,
        "activity A is intact: the rejection touched neither the accounting, nor the vault, nor the recipient's account"
    );
    assert_eq!(
        snapshot(&env, id_b, &recipient_b),
        before_b,
        "activity B is intact"
    );
    assert_eq!(
        env.vault_balance(id_b),
        REFERENCE,
        "the neighbouring activity's vault is untouched"
    );
    assert_eq!(env.vesting(id_a).claimed, 0, "the accounting did not move");
}

#[test]
fn claim_rejects_record_of_another_activity() {
    let (mut env, id_a, id_b, _tge) = setup_two();
    let recipient = env.actors.recipients[0].insecure_clone();
    let recipient_b = env.actors.recipients[1].pubkey();
    let win_mint = env.win_mint;
    let before_a = snapshot(&env, id_a, &recipient.pubkey());
    let before_b = snapshot(&env, id_b, &recipient_b);

    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        vesting_pda(id_b),
        vault_pda(id_a),
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id_a,
    );
    let payer = env.payer.insecure_clone();
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &recipient]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "another activity's record with its own vault → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id_a, &recipient.pubkey()),
        before_a,
        "activity A is intact: the rejection touched neither the accounting, nor the vault, nor the recipient's account"
    );
    assert_eq!(
        snapshot(&env, id_b, &recipient_b),
        before_b,
        "activity B is intact"
    );
    assert_eq!(
        env.vesting(id_b).claimed,
        0,
        "the foreign accounting did not move"
    );
}

#[test]
fn claim_rejects_non_canonical_token_account_in_vault_slot() {
    let (mut env, id_a, id_b, _tge) = setup_two();
    let recipient = env.actors.recipients[0].insecure_clone();
    let recipient_b = env.actors.recipients[1].pubkey();
    let win_mint = env.win_mint;

    // A token account with the CORRECT mint and authority but not at the canonical PDA:
    // `vault_authority`'s own ATA. It satisfies the `token::mint` / `token::authority`
    // bindings - what rejects it is precisely the seeds.
    let authority_ata = env.create_ata(&vault_authority_pda(), &win_mint);
    env.mint_to(&win_mint, &authority_ata, REFERENCE);
    assert_eq!(
        env.token_account_mint_owner(&authority_ata),
        (win_mint, vault_authority_pda()),
        "precondition: the mint and the authority match the activity's vault"
    );
    // The snapshot AFTER the setup (the decoy account is created and funded).
    let before_a = snapshot(&env, id_a, &recipient.pubkey());
    let before_b = snapshot(&env, id_b, &recipient_b);

    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        vesting_pda(id_a),
        authority_ata,
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id_a,
    );
    let payer = env.payer.insecure_clone();
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &recipient]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "matching the mint and the authority is not enough: the vault is also bound by seeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id_a, &recipient.pubkey()),
        before_a,
        "activity A is intact: the rejection touched neither the accounting, nor the vault, nor the recipient's account"
    );
    assert_eq!(
        snapshot(&env, id_b, &recipient_b),
        before_b,
        "activity B is intact"
    );
    assert_eq!(
        env.token_balance(&authority_ata),
        REFERENCE,
        "the decoy account is untouched"
    );
}

#[test]
fn claim_rejects_activity_id_argument_mismatching_the_accounts() {
    let (mut env, id_a, id_b, _tge) = setup_two();
    let recipient = env.actors.recipients[0].insecure_clone();
    let recipient_b = env.actors.recipients[1].pubkey();
    let win_mint = env.win_mint;
    let before_a = snapshot(&env, id_a, &recipient.pubkey());
    let before_b = snapshot(&env, id_b, &recipient_b);

    // The argument comes from activity B while the accounts come from A. The argument
    // goes into the seeds of both PDAs, so the mismatch must fail rather than silently
    // edit someone else's record.
    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        vesting_pda(id_a),
        vault_pda(id_a),
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id_b,
    );
    let payer = env.payer.insecure_clone();
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &recipient]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "the activity_id argument against the submitted accounts → ConstraintSeeds: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id_a, &recipient.pubkey()),
        before_a,
        "activity A is intact: the rejection touched neither the accounting, nor the vault, nor the recipient's account"
    );
    assert_eq!(
        snapshot(&env, id_b, &recipient_b),
        before_b,
        "activity B is intact"
    );
    assert_eq!(env.vesting(id_a).claimed, 0);
    assert_eq!(env.vesting(id_b).claimed, 0);
}

// ===================================================================
// the claim's authorization
// ===================================================================

#[test]
fn claim_rejects_everyone_but_recipient_and_operator() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    env.warp_to_day(tge_ts, DAY_151);
    let before = snapshot(&env, id, &recipient);

    // Owner in this list is the only direct check of the non-overlap of rights (G-1): it
    // has everything except the claim.
    for (label, actor) in wrong_claimers(&env) {
        env.expire_blockhash();
        let r = env.claim_as(&actor, id, recipient);
        assert_eq!(
            custom_error_code(&r),
            Some(err(VestingError::NotAuthorized)),
            "{label} signed the claim → NotAuthorized was expected: {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &recipient),
            before,
            "{label}: the rejection touched neither the vault, nor the accounting, nor the recipient's account"
        );
    }

    // A POSITIVE with the same call: only the key in the `signer` slot differs.
    let recipient_kp = env.actors.recipients[0].insecure_clone();
    env.expire_blockhash();
    env.claim_as(&recipient_kp, id, recipient)
        .expect("the recipient passes");
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_151);
}

#[test]
fn claim_rejects_unsigned_signer_slot() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    let operator = env.actors.operator.pubkey();
    let stranger = env.actors.stranger.insecure_clone();
    let win_mint = env.win_mint;

    let expected = u32::from(ErrorCode::AccountNotSigner);
    assert_eq!(
        expected, 3010,
        "the nominal anchor code AccountNotSigner (Anchor 1.0.2)"
    );

    // The recipient's ATA is created in advance: `init_if_needed` then takes the "already
    // created" branch, there is no system CPI at the payer's expense, and the signature is
    // demanded by exactly the `signer` slot's type. This is also the most dangerous
    // configuration in production - a working bucket's recipient already has an account.
    env.create_ata(&recipient, &win_mint);
    env.warp_to_day(tge_ts, DAY_151);
    let before = snapshot(&env, id, &recipient);

    // Both permitted signers (Arch §8) are checked separately: the body compares the key
    // against `vesting.recipient` OR against `config.operator`, and both comparisons would
    // pass - the signature here is held only by the slot's type. The fee is paid by an
    // outsider, otherwise the key would sign the transaction as the payer.
    for (label, key) in [("the recipient", recipient), ("the operator", operator)] {
        env.expire_blockhash();
        let ix = without_signer(claim_ix(key, id, recipient, win_mint), &key);
        let r = env.send_ixs(&[ix], &stranger.pubkey(), &[&stranger]);
        assert_eq!(
            custom_error_code(&r),
            Some(expected),
            "{label} in the signer slot with no signature → AccountNotSigner: otherwise an outsider would claim on the recipient's behalf: {r:?}"
        );
        assert_eq!(
            snapshot(&env, id, &recipient),
            before,
            "{label}: the rejection released not a single unit"
        );
    }

    // A POSITIVE in the same environment: only the signature flag differs.
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("the same call with the recipient's signature passes");
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_151);
    assert_eq!(
        env.token_balance(&recipient_ata(&recipient, &win_mint)),
        V2_VESTED_DAY_151,
        "it was precisely the signature that opened the payout"
    );
}

#[test]
fn claim_rejects_recipient_of_another_activity() {
    let (mut env, id_a, id_b, _tge) = setup_two();
    let recipient_a = env.actors.recipients[0].pubkey();
    let recipient_b = env.actors.recipients[1].insecure_clone();

    // The key is a genuine recipient, but of ANOTHER activity: a recipient's right is
    // bound to the record rather than to the role "a recipient in general".
    let r = env.claim_as(&recipient_b, id_a, recipient_a);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotAuthorized)),
        "activity B's recipient does not claim activity A: {r:?}"
    );
    assert_eq!(
        env.vesting(id_a).claimed,
        0,
        "activity A's accounting did not move"
    );

    // A POSITIVE in the same environment: it does claim its own activity.
    env.expire_blockhash();
    env.claim_as(&recipient_b, id_b, recipient_b.pubkey())
        .expect("recipient B does claim its own activity");
    assert_eq!(env.vesting(id_b).claimed, V2_VESTED_DAY_151);
    assert_eq!(env.vesting(id_a).claimed, 0);
}

#[test]
fn claim_follows_operator_rotation_in_config() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    let owner = env.actors.owner.insecure_clone();
    let old_operator = env.actors.operator.insecure_clone();
    let new_operator = env.new_wallet();

    // Rotating the operator: the right to claim follows the config's FIELD rather than a
    // key that used to be the operator.
    env.send_ixs(
        &[set_operator_ix(owner.pubkey(), new_operator.pubkey())],
        &owner.pubkey(),
        &[&owner],
    )
    .expect("owner rotates the operator");

    env.warp_to_day(tge_ts, DAY_151);
    let before = snapshot(&env, id, &recipient);

    // The previous operator lost the right along with the field.
    let r = env.claim_as(&old_operator, id, recipient);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotAuthorized)),
        "the previous operator after the rotation → NotAuthorized: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the previous operator's rejection touched neither the accounting, nor the vault, nor the recipient's account"
    );

    // The new operator passes with the same call; the tokens go to the recipient and the
    // new operator pays the ATA's rent itself (payer = signer in the claim context).
    env.expire_blockhash();
    let new_operator_lamports_before = env.lamports_of(&new_operator.pubkey());
    env.claim_as(&new_operator, id, recipient)
        .expect("the new operator claims after the rotation");
    let ata = recipient_ata(&recipient, &env.win_mint);
    assert_eq!(
        env.token_balance(&ata),
        V2_VESTED_DAY_151,
        "the tokens went to the recipient's ATA rather than to the operator"
    );
    // The reference is the minimum rent of an SPL token account (165 bytes) taken from the
    // environment rather than a value from the program: the operator's expense is checked
    // against an independent number.
    let ata_rent = env.svm.minimum_balance_for_rent_exemption(165);
    assert_eq!(
        env.lamports_of(&ata),
        ata_rent,
        "the ATA was created and is rent-exempt: exactly a token account's minimum rent"
    );
    assert_eq!(
        new_operator_lamports_before - env.lamports_of(&new_operator.pubkey()),
        ata_rent,
        "the NEW operator paid the ATA's rent"
    );
}

// ===================================================================
// changing the recipient before TGE changes the payout's addressee
// ===================================================================

#[test]
fn claim_after_set_recipient_pays_the_new_recipient() {
    let (mut env, id, tge_ts) = setup();
    let old_recipient = env.actors.recipients[0].pubkey();
    let new_recipient = env.actors.recipients[1].insecure_clone();
    let win_mint = env.win_mint;
    let admin = env.actors.admin.insecure_clone();

    let ix = set_recipient_ix(admin.pubkey(), id, new_recipient.pubkey());
    env.send_ixs(&[ix], &admin.pubkey(), &[&admin])
        .expect("changing the recipient before TGE passes");
    assert_eq!(env.vesting(id).recipient, new_recipient.pubkey());

    env.warp_to_day(tge_ts, DAY_151);
    env.claim_as(&new_recipient, id, new_recipient.pubkey())
        .expect("the new recipient claims the activity");

    assert_eq!(
        env.token_balance(&recipient_ata(&new_recipient.pubkey(), &win_mint)),
        V2_VESTED_DAY_151,
        "the payout went to the NEW recipient"
    );
    assert!(
        !env.account_exists(&recipient_ata(&old_recipient, &win_mint)),
        "the previous recipient's ATA was never created"
    );

    // The previous key is no longer authorized: the right follows the stored field.
    env.expire_blockhash();
    let old = env.actors.recipients[0].insecure_clone();
    let r = env.claim_as(&old, id, new_recipient.pubkey());
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NotAuthorized)),
        "the previous recipient lost the right along with the record's field: {r:?}"
    );
}

// ===================================================================
// an existing ATA and substitutions of the service slots
// ===================================================================

#[test]
fn claim_into_existing_ata_tops_it_up_without_reinit() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    let win_mint = env.win_mint;

    // The ATA exists in advance and is not empty: `init_if_needed` must skip the creation
    // branch and not zero out what accrued.
    let ata = env.create_ata(&recipient, &win_mint);
    env.mint_to(&win_mint, &ata, V4_REFERENCE);
    let ata_lamports_before = env.lamports_of(&ata);

    env.warp_to_day(tge_ts, DAY_151);
    claim_by_recipient(&mut env, id).expect("a claim into an existing ATA passes");

    assert_eq!(
        env.token_balance(&ata),
        V4_REFERENCE + V2_VESTED_DAY_151,
        "the ATA's balance was topped up rather than re-initialized"
    );
    assert_eq!(
        env.lamports_of(&ata),
        ata_lamports_before,
        "the rent was not paid a second time - the account already existed"
    );
}

#[test]
fn claim_rejects_substituted_token_program() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].insecure_clone();
    let win_mint = env.win_mint;
    env.warp_to_day(tge_ts, DAY_151);
    let before = snapshot(&env, id, &recipient.pubkey());

    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        vesting_pda(id),
        vault_pda(id),
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        vault_authority_pda(),
        win_mint,
        TOKEN_2022_ID,
        id,
    );
    let payer = env.payer.insecure_clone();
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &recipient]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::InvalidProgramId)),
        "Token-2022 in the token_program slot → InvalidProgramId (3008): the typed Program<Token> checks the program id: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient.pubkey()),
        before,
        "substituting the token program did not touch the state"
    );
}

#[test]
fn claim_rejects_foreign_token_mint() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].insecure_clone();
    let foreign_mint = env.foreign_mint;
    env.warp_to_day(tge_ts, DAY_151);
    let before = snapshot(&env, id, &recipient.pubkey());

    // The mint is "foreign" but has the same 8 decimals: the rejection must come from the
    // address binding rather than from a decimals mismatch.
    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        config_pda(),
        vesting_pda(id),
        vault_pda(id),
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &foreign_mint),
        vault_authority_pda(),
        foreign_mint,
        spl_token_interface::id(),
        id,
    );
    let payer = env.payer.insecure_clone();
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &recipient]);
    // The code is pinned by the first execution: `ConstraintTokenMint` (2014) rather than
    // `ConstraintAddress` (2012). Anchor resolves the slots in declaration order and
    // `vault` is declared before `token_mint`, so its `token::mint = token_mint` binding
    // fires first (the vault holds $1WIN while a foreign mint was submitted into the
    // slot). The mint slot's own `address = config.token_mint` is therefore unreachable
    // through the public interface and remains the second layer: should anyone remove
    // `token::mint` from the vault, the mint's binding to the config would hold the
    // invariant on its own.
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintTokenMint)),
        "a foreign mint in the token_mint slot → ConstraintTokenMint (2014), fail-fast before the transfer: {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient.pubkey()),
        before,
        "substituting the mint did not touch the state"
    );
}

#[test]
fn claim_rejects_forged_config() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].insecure_clone();
    let win_mint = env.win_mint;
    env.warp_to_day(tge_ts, DAY_151);

    // A forge: a program-owned account with a genuine discriminator and a
    // copy of a live config's fields at a random address. Were the substitution to pass,
    // the decoy config would define both the `operator` (that is, who may claim) and the
    // `vault_authority_bump` (that is, whose signature the transfer goes out under).
    let forged = Pubkey::new_unique();
    let real = env.vesting_config();
    env.forge_config(&forged, &real);
    let before = snapshot(&env, id, &recipient.pubkey());

    let ix = claim_ix_with_slots(
        recipient.pubkey(),
        forged,
        vesting_pda(id),
        vault_pda(id),
        recipient.pubkey(),
        recipient_ata(&recipient.pubkey(), &win_mint),
        vault_authority_pda(),
        win_mint,
        spl_token_interface::id(),
        id,
    );
    let payer = env.payer.insecure_clone();
    let r = env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &recipient]);
    assert_eq!(
        custom_error_code(&r),
        Some(u32::from(ErrorCode::ConstraintSeeds)),
        "a config outside the canonical PDA → ConstraintSeeds (2006): {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient.pubkey()),
        before,
        "the decoy config did not touch the state"
    );

    // A POSITIVE in the same environment: the canonical config accepts the same
    // call.
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("the canonical config accepts the claim");
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_151);
}

// ===================================================================
// C [the checked_sub → saturating_sub mutation] - claimed > vested →
// MathOverflow
//
// Before this test the `amount = vested.checked_sub(claimed)` gate was uncovered
// (HANDOFF Low#4): honestly `claimed` never exceeds `vested` (the ceiling of
// Arch §10.3 - `vested` never decreases and a claim writes `claimed = vested`), so the
// state is produced by a forge. The `checked_sub → saturating_sub` mutation
// would return 0 instead of an error and the claim would quietly answer
// `NothingToClaim` instead of reverting - which is exactly what this test turns red.
// ===================================================================

#[test]
fn claim_reverts_when_forged_claimed_exceeds_vested() {
    let (mut env, id, tge_ts) = setup();
    let recipient = env.actors.recipients[0].pubkey();
    // Day 151: `vested = V2_VESTED_DAY_151` = 3.5×10¹⁵ - strictly below the reference
    // amount.
    env.warp_to_day(tge_ts, DAY_151);

    // A forge: `claimed = REFERENCE` (10¹⁶) is greater than what is unlocked by
    // day 151. Unreachable through the public interface - the sole writer of `claimed`
    // (`claim`) keeps `claimed = vested <= reference` (Arch §10.3).
    env.forge_claimed(id, REFERENCE);
    // A precondition on the constants: the forged claimed is deliberately greater than
    // vested(151).
    const {
        assert!(REFERENCE > V2_VESTED_DAY_151);
    }
    let before = snapshot(&env, id, &recipient);

    let r = claim_by_recipient(&mut env, id);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::MathOverflow)),
        "claimed > vested → checked_sub underflows → MathOverflow (and NOT NothingToClaim, as saturating_sub would return): {r:?}"
    );
    assert_eq!(
        snapshot(&env, id, &recipient),
        before,
        "the revert moved neither the accounting, nor the vault, nor the recipient's account"
    );
    assert!(
        !env.recipient_ata_exists(&recipient),
        "the recipient's ATA was not created: the revert rolled back init_if_needed too"
    );

    // A POSITIVE in the same environment: we restore a consistent `claimed = 0`,
    // and the same call on day 151 releases exactly what accrued.
    env.forge_claimed(id, 0);
    env.expire_blockhash();
    claim_by_recipient(&mut env, id).expect("with claimed == 0 the claim on day 151 passes");
    assert_eq!(env.vesting(id).claimed, V2_VESTED_DAY_151);
    assert_eq!(
        env.token_balance(&recipient_ata(&recipient, &env.win_mint)),
        V2_VESTED_DAY_151,
    );
}
