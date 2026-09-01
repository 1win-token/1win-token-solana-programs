//! The five reference schedules of §10.5 on-chain.
//!
//! Why the test is laid out the way it is.
//!
//! 1. All five buckets live in ONE environment on ONE TGE - as in production
//!    (Arch §4.2, a single TGE shared by every activity). Time moves forward through
//!    the landmark days 1 / 31 / 270 / 271 / 331 / 361 / 541 / 991, and on each of
//!    them the bucket that has a hand-written literal in the `vesting-expected` crate
//!    is claimed.
//! 2. The expectations are only literals from `vesting_expected::vesting`, derived by
//!    hand from §10.1-§10.3. Intermediate days with no literal are
//!    deliberately not claimed: computing an expectation with the program's formula
//!    would mean checking it against itself (tests-bind-to-real-code).
//! 3. Every bucket is checked both "from above" and "from below": on the days where
//!    the schedule unlocks zero (Treasury's cliff, Community's missing TGE share, an
//!    already fully drawn Marketing) a claim must answer `NothingToClaim` rather than
//!    hand out dust.
//! 4. Flooring is checked on Community with the degenerate reference of 1000 token units
//!    (example V4, `M = 12`): 11 tranches give 913 and the terminal period (day 361)
//!    tops it up to 1000, so the last tranche is 87 rather than the nominal 83. At
//!    production-scale amounts that loss is invisible in whole units.
//! 5. The second test proves accrual loses nothing: the same five buckets, no
//!    intermediate claims, and a single call on day 991 - each receives exactly its
//!    `reference_amount`.
//!
//! ===== DELIBERATELY DEFERRED =====
//! - Random (non-landmark) claim instants - the property lifecycle.
//! - The formula's own boundary values (`u64::MAX`, `M = 10000`) are covered by the
//!   unit tests in `programs/vesting/src/math/tests.rs`: there is no point
//!   running them through the chain, `vested()` is the same one there.

use litesvm::types::TransactionResult;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting::errors::VestingError;
use vesting_test_common::vesting_expected::vesting::{
    COMMUNITY_CLIFF_MONTHS, COMMUNITY_FIRST_MONTHLY_DAY, COMMUNITY_FULL_RELEASE_DAY,
    COMMUNITY_MONTHLY_BPS, COMMUNITY_TGE_BPS, ECOSYSTEM_CLIFF_MONTHS, ECOSYSTEM_FIRST_MONTHLY_DAY,
    ECOSYSTEM_FULL_RELEASE_DAY, ECOSYSTEM_MONTHLY_BPS, ECOSYSTEM_TGE_BPS, LIQUIDITY_CLIFF_MONTHS,
    LIQUIDITY_FULL_RELEASE_DAY, LIQUIDITY_MONTHLY_BPS, LIQUIDITY_REFERENCE, LIQUIDITY_TGE_AMT,
    LIQUIDITY_TGE_BPS, LIQUIDITY_VESTED_DAY_31, LIQUIDITY_VESTED_DAY_361, MARKETING_CLIFF_MONTHS,
    MARKETING_FULL_RELEASE_DAY, MARKETING_MONTHLY_BPS, MARKETING_TGE_BPS, TREASURY_CLIFF_MONTHS,
    TREASURY_FIRST_MONTHLY_DAY, TREASURY_FULL_RELEASE_DAY, TREASURY_MONTHLY_BPS, TREASURY_TGE_BPS,
    V1_REFERENCE, V1_VESTED_FROM_TGE, V2_REFERENCE, V2_VESTED_DAY_1, V2_VESTED_DAY_31,
    V2_VESTED_DAY_541, V3_REFERENCE, V3_VESTED_DAY_271, V3_VESTED_DAY_991, V4_LAST_TRANCHE,
    V4_MONTHLY_AMT, V4_REFERENCE, V4_VESTED_MONTHS_11, V4_VESTED_MONTHS_12,
};
use vesting_test_common::{
    custom_error_code, err, recipient_ata, ActivitySpec, TestEnv, DEFAULT_TGE_OFFSET,
};

/// The last day of Treasury's cliff: `p = floor(269/30) = 8 < s = 9`, so nothing is
/// unlocked yet (example V3 of the architecture). The "270 / 271" pair has no
/// constant of its own in `vesting-expected` - that crate records amounts, not the
/// number of a preceding day.
const TREASURY_LAST_CLIFF_DAY: i64 = TREASURY_FIRST_MONTHLY_DAY - 1;

/// Community's pre-terminal period: period 11, day `1 + 30·11 = 331`
/// (`COMMUNITY_FULL_RELEASE_DAY − 30`, exactly one month before the terminal one). On it
/// 11 tranches are unlocked = `V4_VESTED_MONTHS_11` = 913 (< 1000): the flooring has
/// not been topped up yet (example V4).
const COMMUNITY_PRE_TERMINAL_DAY: i64 = COMMUNITY_FULL_RELEASE_DAY - 30;

/// An emission bucket in the test: the activity, its recipient and the reference
/// amount.
struct Section {
    label: &'static str,
    id: u64,
    recipient: Keypair,
    reference: u64,
}

/// Sets up a bucket: creates an activity on the given schedule for its own recipient
/// and funds the vault with exactly the reference amount (the honest path of Arch §2 -
/// minting the emission straight into the vault).
fn add_section(
    env: &mut TestEnv,
    label: &'static str,
    reference: u64,
    tge_bps: u16,
    monthly_bps: u16,
    cliff_months: u16,
) -> Section {
    let recipient = env.new_wallet();
    let spec = ActivitySpec {
        reference_amount: reference,
        tge_bps,
        monthly_bps,
        cliff_months,
        label: String::from(label),
    };
    let id = env.create_activity_for(&recipient.pubkey(), &spec);
    env.fund_vault(id, reference);
    Section {
        label,
        id,
        recipient,
        reference,
    }
}

/// The five buckets of §10.5 on a shared TGE. Returns `(env, sections, tge_ts)`; the
/// buckets' order matches the table of §10.5.
fn setup_all_sections() -> (TestEnv, Vec<Section>, i64) {
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let sections = vec![
        add_section(
            &mut env,
            "marketing",
            V1_REFERENCE,
            MARKETING_TGE_BPS,
            MARKETING_MONTHLY_BPS,
            MARKETING_CLIFF_MONTHS,
        ),
        add_section(
            &mut env,
            "ecosystem",
            V2_REFERENCE,
            ECOSYSTEM_TGE_BPS,
            ECOSYSTEM_MONTHLY_BPS,
            ECOSYSTEM_CLIFF_MONTHS,
        ),
        add_section(
            &mut env,
            "community",
            V4_REFERENCE,
            COMMUNITY_TGE_BPS,
            COMMUNITY_MONTHLY_BPS,
            COMMUNITY_CLIFF_MONTHS,
        ),
        add_section(
            &mut env,
            "treasury",
            V3_REFERENCE,
            TREASURY_TGE_BPS,
            TREASURY_MONTHLY_BPS,
            TREASURY_CLIFF_MONTHS,
        ),
        add_section(
            &mut env,
            "liquidity",
            LIQUIDITY_REFERENCE,
            LIQUIDITY_TGE_BPS,
            LIQUIDITY_MONTHLY_BPS,
            LIQUIDITY_CLIFF_MONTHS,
        ),
    ];
    (env, sections, params.tge_ts)
}

/// A bucket's claim by its recipient (sent through [`TestEnv::claim_as`]).
#[allow(clippy::result_large_err)]
fn send_claim(env: &mut TestEnv, section: &Section) -> TransactionResult {
    let signer = section.recipient.insecure_clone();
    env.claim_as(&signer, section.id, signer.pubkey())
}

/// Claims a bucket and checks the ACCUMULATED total against the hand-written literal:
/// both on the recipient's ATA and in the record's `claimed` field.
fn claim_expecting_total(env: &mut TestEnv, section: &Section, expected_total: u64) {
    env.expire_blockhash();
    let r = send_claim(env, section);
    r.as_ref().unwrap_or_else(|e| {
        panic!(
            "{}: the claim on the landmark day must pass (a total of {expected_total} was expected): {e:?}",
            section.label
        )
    });
    let ata = recipient_ata(&section.recipient.pubkey(), &env.win_mint);
    assert_eq!(
        env.token_balance(&ata),
        expected_total,
        "{}: the accumulated payout diverged from the reference of §10.5",
        section.label
    );
    assert_eq!(
        env.vesting(section.id).claimed,
        expected_total,
        "{}: claimed == what was released (I1)",
        section.label
    );
}

/// Claims a bucket expecting the "nothing is unlocked beyond what was already
/// released" rejection.
fn claim_expecting_nothing(env: &mut TestEnv, section: &Section) {
    env.expire_blockhash();
    let claimed_before = env.vesting(section.id).claimed;
    let r = send_claim(env, section);
    assert_eq!(
        custom_error_code(&r),
        Some(err(VestingError::NothingToClaim)),
        "{}: on this day the schedule unlocks nothing beyond what was released: {r:?}",
        section.label
    );
    assert_eq!(
        env.vesting(section.id).claimed,
        claimed_before,
        "{}: the rejection did not move the accounting",
        section.label
    );
}

// ===================================================================
// the landmark days of the five schedules
// ===================================================================

#[test]
fn five_reference_sections_pay_exactly_the_manual_literals() {
    let (mut env, sections, tge_ts) = setup_all_sections();
    let [marketing, ecosystem, community, treasury, liquidity] = &sections[..] else {
        panic!("exactly five buckets of §10.5 were expected");
    };

    // --- Day 1 (p = 0, the TGE instant) ---
    env.warp_to(tge_ts);
    // "100% at TGE": the whole amount is available at once, the `monthly_bps == 0`
    // branch.
    claim_expecting_total(&mut env, marketing, V1_VESTED_FROM_TGE);
    claim_expecting_total(&mut env, ecosystem, V2_VESTED_DAY_1);
    claim_expecting_total(&mut env, liquidity, LIQUIDITY_TGE_AMT);
    // `tge_bps = 0` - nothing is released at the TGE instant.
    claim_expecting_nothing(&mut env, community);
    // A 9-month cliff with `tge_bps = 0` - all the more so, nothing.
    claim_expecting_nothing(&mut env, treasury);

    // --- Day 31 (p = 1, the first monthly tranche of the cliff-less schedules) ---
    env.warp_to_day(tge_ts, ECOSYSTEM_FIRST_MONTHLY_DAY);
    assert_eq!(
        ECOSYSTEM_FIRST_MONTHLY_DAY, COMMUNITY_FIRST_MONTHLY_DAY,
        "both cliff-less schedules have their first tranche on the same day"
    );
    claim_expecting_total(&mut env, ecosystem, V2_VESTED_DAY_31);
    claim_expecting_total(&mut env, community, V4_MONTHLY_AMT);
    claim_expecting_total(&mut env, liquidity, LIQUIDITY_VESTED_DAY_31);
    claim_expecting_nothing(&mut env, treasury);
    // A fully drawn bucket hands out nothing on a repeat (the I1 ceiling).
    claim_expecting_nothing(&mut env, marketing);

    // --- Days 270 / 271: the boundary of Treasury's cliff (example V3) ---
    env.warp_to_day(tge_ts, TREASURY_LAST_CLIFF_DAY);
    claim_expecting_nothing(&mut env, treasury);
    env.warp_to_day(tge_ts, TREASURY_FIRST_MONTHLY_DAY);
    claim_expecting_total(&mut env, treasury, V3_VESTED_DAY_271);

    // --- Day 331: Community's pre-terminal period (11 tranches = 913, the flooring is not
    // topped up) ---
    env.warp_to_day(tge_ts, COMMUNITY_PRE_TERMINAL_DAY);
    claim_expecting_total(&mut env, community, V4_VESTED_MONTHS_11);

    // --- Day 361: the terminal period of Liquidity and Community ---
    assert_eq!(
        LIQUIDITY_FULL_RELEASE_DAY, COMMUNITY_FULL_RELEASE_DAY,
        "Liquidity and Community (both cliff-less, M = 12) share the same full-release day"
    );
    env.warp_to_day(tge_ts, LIQUIDITY_FULL_RELEASE_DAY);
    claim_expecting_total(&mut env, liquidity, LIQUIDITY_VESTED_DAY_361);
    claim_expecting_total(&mut env, community, V4_VESTED_MONTHS_12);
    assert_eq!(
        V4_VESTED_MONTHS_12 - V4_VESTED_MONTHS_11,
        V4_LAST_TRANCHE,
        "Community's last tranche is not the nominal 83 but 87 units (1000 − 913): the terminal branch hands out everything the floor dropped along with it (example V4)"
    );

    // --- Day 541: the terminal period of Ecosystem ---
    env.warp_to_day(tge_ts, ECOSYSTEM_FULL_RELEASE_DAY);
    claim_expecting_total(&mut env, ecosystem, V2_VESTED_DAY_541);

    // --- Day 991: the terminal period of Treasury ---
    env.warp_to_day(tge_ts, TREASURY_FULL_RELEASE_DAY);
    claim_expecting_total(&mut env, treasury, V3_VESTED_DAY_991);

    // The total: every bucket was drawn to exactly its reference amount, and none
    // overshot.
    for section in &sections {
        let record = env.vesting(section.id);
        assert_eq!(
            record.claimed, section.reference,
            "{}: after the full-release day claimed == reference_amount",
            section.label
        );
        assert_eq!(
            env.vault_balance(section.id),
            0,
            "{}: the vault emptied out to exactly zero - as much was released as was minted",
            section.label
        );
    }
    assert_eq!(
        MARKETING_FULL_RELEASE_DAY, 1,
        "for \"100% at TGE\" the full-release day is TGE itself"
    );
}

// ===================================================================
// accrual across skipped periods (the property of example V5)
// ===================================================================

#[test]
fn single_claim_after_full_release_pays_each_section_its_whole_reference() {
    let (mut env, sections, tge_ts) = setup_all_sections();

    // No intermediate claims at all: straight to the terminal period of the longest
    // schedule (Treasury, day 991).
    env.warp_to_day(tge_ts, TREASURY_FULL_RELEASE_DAY);

    for section in &sections {
        claim_expecting_total(&mut env, section, section.reference);
        // A repeat right after the full draw-down: the claim is repeatable but there is
        // nothing to release.
        claim_expecting_nothing(&mut env, section);
    }

    // Vault isolation (I2) across a chain of five activities: every bucket has its own
    // "record + vault" pair and there are no cross debits.
    let total_paid: u64 = sections
        .iter()
        .map(|s| {
            let ata: Pubkey = recipient_ata(&s.recipient.pubkey(), &env.win_mint);
            env.token_balance(&ata)
        })
        .sum();
    let total_reference: u64 = sections.iter().map(|s| s.reference).sum();
    assert_eq!(
        total_paid, total_reference,
        "in total exactly as much was released as was minted into all five vaults"
    );
}
