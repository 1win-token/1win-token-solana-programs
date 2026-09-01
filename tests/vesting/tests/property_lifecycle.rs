//! A property test of the Vesting lifecycle: random sequences of
//! `create_vesting` → funding → `cancel_vesting` → claims at random moments →
//! `close_vesting` across three or more activities with DIFFERENT schedules (every
//! scenario is guaranteed to contain a schedule with `tge_bps = 10000` and one with
//! `monthly_bps = 834` - this is asserted against the set's parameters rather than its
//! order). Every step is a real instruction; the assertions run against the observable
//! state (tests-bind-to-real-code).
//!
//! What is checked after every step:
//! - `vesting.claimed` never decreases and never exceeds `reference_amount`;
//! - the recipient's ATA balance == the sum of `vesting.claimed` across ALL of its
//!   activities (the recipient is never changed here, so the equality is exact rather
//!   than "no greater than"; one wallet may hold two activities - the `Dense`
//!   profile);
//! - the vault's balance == `reference_amount − claimed` (funding is exactly the
//!   reference amount): as much was released as left the account;
//! - an immediate repeat of a successful claim → `NothingToClaim`, with the state
//!   unmoved;
//! - the vaults and records of the OTHER activities did not change by a single unit;
//! - `config.activity_count` is monotonic: neither a cancellation nor a close
//!   decrements it (the counter is an id allocator, and rolling it back would make a
//!   closed activity's seeds reusable);
//! - `assert_funded` passes after every payout: `vault.amount + claimed` covers the
//!   reference amount (the accounting agrees with the balance).
//!
//! How the expectations about AMOUNTS are set. The `vested()` formula is never
//! reproduced in the test (or the equality would be a tautology). On random days only
//! the inequality invariants above are checked. Exact amounts are checked on the
//! schedules' LANDMARK days: every bucket in [`SECTIONS`] carries a list of
//! `(day, reference)` - literals from the `vesting-expected` crate derived by hand from
//! §10.1-§10.3. A landmark day always falls into the sweep, and after a claim attempt
//! on it `claimed` must equal the literal - regardless of whether that call released
//! anything or answered `NothingToClaim` (the value having been reached earlier).
//!
//! Determinism: SplitMix64 seeded from [`BASE_SEED`]; the seed of a failing scenario is
//! printed in the panic. To reproduce:
//! `VESTING_LIFECYCLE_SEED=<seed> cargo test <test_name>` - the batch then runs exactly
//! one scenario with that seed.

// DUP: the SplitMix64 generator, the batch runner and the env-seed parsing are a copy
// of the approach in tests/sale/tests/property_lifecycle.rs (a shared
// svm-core crate is a follow-up).

use std::collections::BTreeSet;
use std::panic::{catch_unwind, AssertUnwindSafe};

use litesvm::types::TransactionResult;
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use vesting::errors::VestingError;
use vesting_test_common::vesting_expected::vesting::{
    COMMUNITY_CLIFF_MONTHS, COMMUNITY_FULL_RELEASE_DAY, COMMUNITY_MONTHLY_BPS, COMMUNITY_TGE_BPS,
    ECOSYSTEM_CLIFF_MONTHS, ECOSYSTEM_FULL_RELEASE_DAY, ECOSYSTEM_MONTHLY_BPS, ECOSYSTEM_TGE_BPS,
    LIQUIDITY_CLIFF_MONTHS, LIQUIDITY_FULL_RELEASE_DAY, LIQUIDITY_MONTHLY_BPS, LIQUIDITY_REFERENCE,
    LIQUIDITY_TGE_BPS, LIQUIDITY_VESTED_DAY_31, LIQUIDITY_VESTED_DAY_331, LIQUIDITY_VESTED_DAY_361,
    MARKETING_CLIFF_MONTHS, MARKETING_FULL_RELEASE_DAY, MARKETING_MONTHLY_BPS, MARKETING_TGE_BPS,
    TREASURY_CLIFF_MONTHS, TREASURY_FULL_RELEASE_DAY, TREASURY_MONTHLY_BPS, TREASURY_TGE_BPS,
    V1_REFERENCE, V1_VESTED_FROM_TGE, V2_REFERENCE, V2_VESTED_DAY_1, V2_VESTED_DAY_151,
    V2_VESTED_DAY_31, V2_VESTED_DAY_511, V2_VESTED_DAY_541, V2_VESTED_DAY_61, V3_REFERENCE,
    V3_VESTED_DAY_270, V3_VESTED_DAY_271, V3_VESTED_DAY_961, V3_VESTED_DAY_991, V4_REFERENCE,
    V4_VESTED_MONTHS_11, V4_VESTED_MONTHS_12,
};
use vesting_test_common::{
    assert_funded_ix, cancel_vesting_ix, close_vesting_ix, custom_error_code, err, vault_pda,
    vesting_pda, ActivitySpec, TestEnv, DEFAULT_TGE_OFFSET,
};

/// The determinism base; overridden by the `VESTING_LIFECYCLE_SEED` env var (decimal
/// or 0x-hex).
const BASE_SEED: u64 = 0x1B16_5EED_2026_0728;

// ---------- A deterministic PRNG (SplitMix64) ----------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// Uniformly from [lo, hi], inclusive.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn shuffle<T>(&mut self, xs: &mut [T]) {
        for i in (1..xs.len()).rev() {
            let j = self.below((i + 1) as u64) as usize;
            xs.swap(i, j);
        }
    }
}

// ---------- The buckets' schedules and their landmark points ----------

/// A tokenomics bucket: the schedule of §10.5, the reference amount and the landmark
/// unlock points. All the numbers are literals from the `vesting-expected` crate
///; the program's formula is not reproduced here.
struct Section {
    label: &'static str,
    reference: u64,
    tge_bps: u16,
    monthly_bps: u16,
    cliff_months: u16,
    /// The full-release day (§10.5): from it onwards `vested == reference_amount`.
    full_release_day: i64,
    /// `(day, the reference vested on that day)` - the sweep must pass through every one
    /// of them.
    anchors: &'static [(i64, u64)],
}

impl Section {
    fn spec(&self) -> ActivitySpec {
        ActivitySpec {
            reference_amount: self.reference,
            tge_bps: self.tge_bps,
            monthly_bps: self.monthly_bps,
            cliff_months: self.cliff_months,
            label: String::from(self.label),
        }
    }
}

/// The five reference buckets of §10.5. The first [`FIXED_SECTIONS`] go to the first
/// activities of EVERY scenario (that set's properties are asserted in
/// [`assert_schedule_coverage`]); the third activity is pinned to one of the rest by
/// the scenario's number within the batch, and the remaining ones are drawn at random.
static SECTIONS: &[Section] = &[
    // Marketing: the whole amount at the TGE instant, with no monthly
    // tranches.
    Section {
        label: "marketing",
        reference: V1_REFERENCE,
        tge_bps: MARKETING_TGE_BPS,
        monthly_bps: MARKETING_MONTHLY_BPS,
        cliff_months: MARKETING_CLIFF_MONTHS,
        full_release_day: MARKETING_FULL_RELEASE_DAY,
        anchors: &[(MARKETING_FULL_RELEASE_DAY, V1_VESTED_FROM_TGE)],
    },
    // Community Rewards: 8.34% per month, 12 tranches, and the terminal one tops up the
    // flooring (the reference amount is degenerately small - 1000 units - so that the
    // loss is visible in whole units).
    Section {
        label: "community",
        reference: V4_REFERENCE,
        tge_bps: COMMUNITY_TGE_BPS,
        monthly_bps: COMMUNITY_MONTHLY_BPS,
        cliff_months: COMMUNITY_CLIFF_MONTHS,
        full_release_day: COMMUNITY_FULL_RELEASE_DAY,
        anchors: &[
            (331, V4_VESTED_MONTHS_11),
            (COMMUNITY_FULL_RELEASE_DAY, V4_VESTED_MONTHS_12),
        ],
    },
    // Ecosystem: 10% at TGE plus 5% per month, 18 tranches.
    Section {
        label: "ecosystem",
        reference: V2_REFERENCE,
        tge_bps: ECOSYSTEM_TGE_BPS,
        monthly_bps: ECOSYSTEM_MONTHLY_BPS,
        cliff_months: ECOSYSTEM_CLIFF_MONTHS,
        full_release_day: ECOSYSTEM_FULL_RELEASE_DAY,
        anchors: &[
            (1, V2_VESTED_DAY_1),
            (31, V2_VESTED_DAY_31),
            (61, V2_VESTED_DAY_61),
            (151, V2_VESTED_DAY_151),
            (511, V2_VESTED_DAY_511),
            (ECOSYSTEM_FULL_RELEASE_DAY, V2_VESTED_DAY_541),
        ],
    },
    // Treasury: a 9-month cliff, 4% per month, 25 tranches - the longest schedule.
    Section {
        label: "treasury",
        reference: V3_REFERENCE,
        tge_bps: TREASURY_TGE_BPS,
        monthly_bps: TREASURY_MONTHLY_BPS,
        cliff_months: TREASURY_CLIFF_MONTHS,
        full_release_day: TREASURY_FULL_RELEASE_DAY,
        anchors: &[
            // Day 270 is the last day of the cliff: nothing is unlocked, so a claim attempt on it
            // must answer `NothingToClaim`.
            (270, V3_VESTED_DAY_270),
            (271, V3_VESTED_DAY_271),
            (961, V3_VESTED_DAY_961),
            (TREASURY_FULL_RELEASE_DAY, V3_VESTED_DAY_991),
        ],
    },
    // Liquidity: 10% at TGE plus 7.5% per month, 12 tranches.
    Section {
        label: "liquidity",
        reference: LIQUIDITY_REFERENCE,
        tge_bps: LIQUIDITY_TGE_BPS,
        monthly_bps: LIQUIDITY_MONTHLY_BPS,
        cliff_months: LIQUIDITY_CLIFF_MONTHS,
        full_release_day: LIQUIDITY_FULL_RELEASE_DAY,
        anchors: &[
            (31, LIQUIDITY_VESTED_DAY_31),
            (331, LIQUIDITY_VESTED_DAY_331),
            (LIQUIDITY_FULL_RELEASE_DAY, LIQUIDITY_VESTED_DAY_361),
        ],
    },
];

/// How many buckets are pinned to the start of every scenario's set.
const FIXED_SECTIONS: usize = 2;

/// The mask "all five buckets of §10.5 were exercised" for the batch's coverage
/// counter.
const ALL_SECTIONS: u32 = (1u32 << SECTIONS.len()) - 1;

/// The TGE share of the "everything at TGE" schedule: it has no monthly tranches at
/// all, that is, the `monthly_bps == 0` branch of the formula of §10.2.
const FULL_TGE_BPS: u16 = 10_000;

/// Community Rewards' monthly share: 8.34% × 12 tranches do not add up to 100%, so the
/// terminal period tops up the flooring (§10.3).
const FLOORING_MONTHLY_BPS: u16 = 834;

/// A bucket's position in [`SECTIONS`] - for the batch's coverage counter.
fn section_index(section: &'static Section) -> usize {
    SECTIONS
        .iter()
        .position(|s| std::ptr::eq(s, section))
        .expect("the scenario's bucket comes from SECTIONS")
}

/// The properties of a scenario's schedule set: it contains a schedule with no monthly
/// tranches (`tge_bps == 10000`) and one with a monthly share of 8.34% whose terminal
/// period tops up the flooring. The activities' PARAMETERS are checked rather than the
/// buckets' indices: reordering [`SECTIONS`] or editing a reference literal then breaks
/// the assertion instead of quietly changing what is being checked.
fn assert_schedule_coverage(acts: &[Activity]) {
    assert!(
        acts.iter().any(|a| a.section.tge_bps == FULL_TGE_BPS),
        "the scenario's set must contain the \"everything at TGE\" schedule (tge_bps == {FULL_TGE_BPS})"
    );
    assert!(
        acts.iter()
            .any(|a| a.section.monthly_bps == FLOORING_MONTHLY_BPS),
        "the scenario's set must contain a schedule with monthly_bps == {FLOORING_MONTHLY_BPS} - only it produces flooring in the terminal period"
    );
}

// ---------- The scenario profiles ----------

#[derive(Debug, Clone, Copy)]
enum Profile {
    /// The minimum number of activities (3), dense claims, no cancellations and no closes:
    /// the baseline sweep of the unlock.
    Dense,
    /// 4-6 activities with sparse claims: long skipped periods and accrual.
    Sparse,
    /// Cancellations before TGE plus a second creation batch afterwards: a check of the id
    /// allocator and of vault isolation across a "holey" set of activities.
    WithCancels,
    /// A full drain: claims up to the terminal period on every activity, then
    /// `close_vesting` on each of them.
    FullDrain,
}

impl Profile {
    fn activities(self, rng: &mut Rng) -> usize {
        match self {
            Profile::Dense => 3,
            Profile::Sparse => rng.range(4, 6) as usize,
            Profile::WithCancels => rng.range(3, 5) as usize,
            Profile::FullDrain => rng.range(3, 4) as usize,
        }
    }

    /// The probability of claiming an activity on a NON-landmark day of the sweep.
    fn claim_pct(self) -> u64 {
        match self {
            Profile::Dense => 80,
            Profile::Sparse => 20,
            Profile::WithCancels => 50,
            Profile::FullDrain => 60,
        }
    }

    /// The probability of cancelling an activity before TGE.
    fn cancel_pct(self) -> u64 {
        match self {
            Profile::WithCancels => 40,
            _ => 0,
        }
    }

    /// How many random (non-landmark) days are added to the sweep.
    fn random_days(self, rng: &mut Rng) -> u64 {
        match self {
            Profile::Dense => rng.range(4, 8),
            Profile::Sparse => rng.range(2, 5),
            Profile::WithCancels => rng.range(2, 5),
            Profile::FullDrain => rng.range(1, 3),
        }
    }

    fn closes_activities(self) -> bool {
        matches!(self, Profile::FullDrain)
    }

    /// Whether the profile gives two activities to ONE wallet. The program does not forbid
    /// it: `recipient` is a field of the record rather than a unique key, and both
    /// activities pay out to that wallet's same ATA. The configuration is checked in
    /// `Dense` - the profile with the densest claims.
    fn shares_recipient(self) -> bool {
        matches!(self, Profile::Dense)
    }
}

// ---------- The scenario's coverage ----------

/// What the scenario actually executed. A property test must prove not only the
/// invariants but also that it checked them on a non-empty set of actions: a
/// degenerate generator (every claim skipped, not a single landmark point passed)
/// would give a green suite having verified nothing. The minimums are asserted at the
/// end of every scenario, and the profile-specific ones in the test over the batch's
/// total.
#[derive(Debug, Default, Clone, Copy)]
struct Stats {
    /// The claims that released a non-zero amount.
    paid_claims: u64,
    /// The claims that answered `NothingToClaim` (repeats and "nothing to release").
    empty_claims: u64,
    /// The checks against a reference literal on the schedules' landmark days.
    anchor_checks: u64,
    cancels: u64,
    closes: u64,
    /// The activities whose recipient wallet serves more than one activity.
    shared_recipient_activities: u64,
    /// The bitmask of the [`SECTIONS`] buckets that were exercised (bit = index).
    sections_used: u32,
}

impl Stats {
    fn add(&mut self, other: Stats) {
        self.paid_claims += other.paid_claims;
        self.empty_claims += other.empty_claims;
        self.anchor_checks += other.anchor_checks;
        self.cancels += other.cancels;
        self.closes += other.closes;
        self.shared_recipient_activities += other.shared_recipient_activities;
        self.sections_used |= other.sections_used;
    }
}

// ---------- The scenario's state ----------

struct Activity {
    id: u64,
    section: &'static Section,
    recipient: Keypair,
    /// Whether it is alive (an activity cancelled before TGE is closed together with its
    /// vault).
    alive: bool,
    /// Whether it was cancelled before TGE. It differs from `!alive` in that an activity
    /// closed after a full draw-down released its reference amount to the recipient while a
    /// cancelled one returned it to the withdrawal account: the final conservation counts
    /// them differently.
    cancelled: bool,
    /// The last OBSERVED value of `claimed` - for the monotonicity check.
    claimed: u64,
}

impl Activity {
    fn reference(&self) -> u64 {
        self.section.reference
    }

    /// The unlock reference on the landmark day `day`, if it is a landmark for the bucket.
    fn anchor(&self, day: i64) -> Option<u64> {
        self.section
            .anchors
            .iter()
            .find(|(d, _)| *d == day)
            .map(|(_, vested)| *vested)
    }
}

/// The vault balances and `claimed` of every live activity - a snapshot for the "the
/// step did not touch the neighbours" assertion. `None` for closed ones.
fn vaults_snapshot(env: &TestEnv, acts: &[Activity]) -> Vec<Option<(u64, u64)>> {
    acts.iter()
        .map(|a| {
            a.alive
                .then(|| (env.vault_balance(a.id), env.vesting(a.id).claimed))
        })
        .collect()
}

/// The "the neighbours did not change" assertion: it compares the snapshots, skipping
/// activity `skip_id` (the one the step actually moved).
fn assert_neighbours_untouched(
    env: &TestEnv,
    acts: &[Activity],
    before: &[Option<(u64, u64)>],
    skip_id: Option<u64>,
    what: &str,
) {
    let after = vaults_snapshot(env, acts);
    for (i, a) in acts.iter().enumerate() {
        if Some(a.id) == skip_id {
            continue;
        }
        assert_eq!(
            after[i], before[i],
            "{what}: activity {} ({}) changed even though the step did not touch it",
            a.id, a.section.label
        );
    }
}

/// The invariants of activity `acts[i]` after any step: the accounting never decreases,
/// never exceeds the reference amount, the vault holds the obligation's remainder
/// (funding being exactly the reference amount), and the recipient's ATA holds the
/// accounted total across ALL of its activities - one wallet may have several, and
/// every one of them pays out to the same account.
fn assert_activity_invariants(env: &TestEnv, acts: &mut [Activity], i: usize, what: &str) {
    let (id, reference, previous) = (acts[i].id, acts[i].reference(), acts[i].claimed);
    let claimed = env.vesting(id).claimed;
    assert!(
        claimed >= previous,
        "{what}: the claimed of activity {id} decreased: {previous} → {claimed}"
    );
    assert!(
        claimed <= reference,
        "{what}: the claimed of activity {id} ({claimed}) exceeded the reference {reference}"
    );
    assert_eq!(
        env.vault_balance(id),
        reference - claimed,
        "{what}: the vault of activity {id} diverged from the accounting"
    );
    acts[i].claimed = claimed;
    assert_recipient_ata_holds_sum(env, acts, i, what);
}

/// The ATA balance of activity `acts[i]`'s recipient == the sum accounted across every
/// live and closed activity of THAT wallet (a close does not take tokens off the
/// account, and a cancelled activity released nothing).
fn assert_recipient_ata_holds_sum(env: &TestEnv, acts: &[Activity], i: usize, what: &str) {
    let recipient = acts[i].recipient.pubkey();
    let expected: u64 = acts
        .iter()
        .filter(|a| a.recipient.pubkey() == recipient)
        .map(|a| a.claimed)
        .sum();
    assert_eq!(
        env.recipient_ata_balance(&recipient),
        expected,
        "{what}: the ATA of activity {}'s recipient does not hold what is accounted across its activities",
        acts[i].id
    );
}

/// An `assert_funded` on the activity from an outside key: the instruction is
/// permissionless, changes no state, and after any payout must pass
/// (`vault.amount + claimed >= reference_amount`).
fn assert_still_funded(env: &mut TestEnv, activity_id: u64, what: &str) {
    let stranger = env.actors.stranger.insecure_clone();
    let ix = assert_funded_ix(activity_id, env.win_mint);
    env.expire_blockhash();
    env.send_ixs(&[ix], &stranger.pubkey(), &[&stranger])
        .unwrap_or_else(|e| {
            panic!("{what}: assert_funded of activity {activity_id} failed: {e:?}");
        });
}

/// An activity's claim: signed by its recipient or by the operator (the choice is
/// random - both paths are equal, Arch §8; in either case the payout addressee is the
/// recipient).
#[allow(clippy::result_large_err)]
fn claim(env: &mut TestEnv, a: &Activity, by_operator: bool) -> TransactionResult {
    let recipient = a.recipient.pubkey();
    env.expire_blockhash();
    if by_operator {
        let operator = env.actors.operator.insecure_clone();
        env.claim_as(&operator, a.id, recipient)
    } else {
        let signer = a.recipient.insecure_clone();
        env.claim_as(&signer, a.id, recipient)
    }
}

// ---------- The scenario ----------

fn run_scenario(seed: u64, profile: Profile, idx: u64) -> Stats {
    let mut stats = Stats::default();
    let mut rng = Rng::new(seed);
    let mut env = TestEnv::new();
    let params = env.init_default(DEFAULT_TGE_OFFSET);
    let tge_ts = params.tge_ts;
    let owner = env.actors.owner.insecure_clone();
    let rent_collector = env.actors.rent_collector.pubkey();
    let win_mint = env.win_mint;
    // The single withdrawal recipient W: its ATA is the addressee of the remainders on a
    // cancellation or a close, and it is also the `rent_collector`
    // (`config.withdrawal_authority`).
    let beneficiary_token = env.create_ata(&rent_collector, &win_mint);
    let nothing_to_claim = err(VestingError::NothingToClaim);

    // --- creating the activities (only before TGE) ---
    let n = profile.activities(&mut rng);
    assert!(
        n > FIXED_SECTIONS,
        "the profile must create more than {FIXED_SECTIONS} activities: both the scenario's schedule coverage and the batch's bucket rotation rest on them"
    );
    // The first FIXED_SECTIONS buckets are pinned to the start of the set, the third to a
    // bucket chosen by the scenario's number within the batch (rotating over indices
    // 2..4, so a batch of three or more scenarios exercises all five buckets of §10.5),
    // and the rest are drawn at random. The set is fixed before execution - all the
    // scenario's randomness is drawn in advance.
    let rotated = FIXED_SECTIONS + (idx as usize % (SECTIONS.len() - FIXED_SECTIONS));
    let sections: Vec<&'static Section> = (0..n)
        .map(|i| match i {
            0 | 1 => &SECTIONS[i],
            2 => &SECTIONS[rotated],
            _ => &SECTIONS[rng.below(SECTIONS.len() as u64) as usize],
        })
        .collect();
    let mut acts: Vec<Activity> = Vec::new();
    for (i, section) in sections.into_iter().enumerate() {
        // The last activity of a shared-wallet profile goes to the SECOND activity's
        // recipient: two live activities on one recipient's ATA.
        let recipient = if profile.shares_recipient() && i + 1 == n {
            acts[1].recipient.insecure_clone()
        } else {
            env.new_wallet()
        };
        let id = env.create_activity_for(&recipient.pubkey(), &section.spec());
        assert_eq!(
            id, i as u64,
            "the ids are issued consecutively starting from zero"
        );
        stats.sections_used |= 1 << section_index(section);
        acts.push(Activity {
            id,
            section,
            recipient,
            alive: true,
            cancelled: false,
            claimed: 0,
        });
    }
    assert_schedule_coverage(&acts);
    let mut created_total = n as u64;
    assert_eq!(
        env.vesting_config().activity_count,
        created_total,
        "the activity counter == the number created"
    );

    // --- funding to exactly the reference amount plus the coverage check ---
    for a in &acts {
        env.fund_vault(a.id, a.section.reference);
    }
    for a in &acts {
        let id = a.id;
        assert_still_funded(&mut env, id, "after the funding");
    }

    // --- the cancellations before TGE ---
    let mut cancelled_total: u64 = 0;
    for i in 0..acts.len() {
        if !rng.chance(profile.cancel_pct()) {
            continue;
        }
        // At least one activity must stay alive: on an empty set the day sweep and the final
        // assertions would become degenerate.
        if acts.iter().filter(|a| a.alive).count() <= 1 {
            continue;
        }
        let before = vaults_snapshot(&env, &acts);
        let beneficiary_before = env.token_balance(&beneficiary_token);
        let rent_before = env.lamports_of(&rent_collector);
        let (id, reference) = (acts[i].id, acts[i].reference());
        let vault_rent = env.lamports_of(&vault_pda(id));
        let record_rent = env.lamports_of(&vesting_pda(id));

        env.expire_blockhash();
        let ix = cancel_vesting_ix(
            owner.pubkey(),
            id,
            beneficiary_token,
            rent_collector,
            win_mint,
        );
        let payer = env.payer.insecure_clone();
        env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &owner])
            .unwrap_or_else(|e| panic!("cancel_vesting of activity {id} failed: {e:?}"));

        assert!(
            !env.vesting_exists(id),
            "the cancellation closed record {id}"
        );
        assert!(!env.vault_exists(id), "the cancellation closed vault {id}");
        assert_eq!(
            env.token_balance(&beneficiary_token) - beneficiary_before,
            reference,
            "the whole remainder of vault {id} went to the withdrawal recipient"
        );
        assert_eq!(
            env.lamports_of(&rent_collector) - rent_before,
            vault_rent + record_rent,
            "the rent of both closed accounts {id} went to the withdrawal recipient"
        );
        assert_eq!(
            env.vesting_config().activity_count,
            created_total,
            "a cancellation does not decrement the id allocator"
        );
        acts[i].alive = false;
        acts[i].cancelled = true;
        cancelled_total += reference;
        stats.cancels += 1;
        assert_neighbours_untouched(&env, &acts, &before, Some(id), "the cancellation");
    }

    // --- the second creation batch: the ids keep growing after the cancellations ---
    if matches!(profile, Profile::WithCancels) {
        for _ in 0..rng.range(1, 2) {
            let section = &SECTIONS[rng.below(SECTIONS.len() as u64) as usize];
            let recipient = env.new_wallet();
            let id = env.create_activity_for(&recipient.pubkey(), &section.spec());
            assert_eq!(
                id, created_total,
                "the new activity got the next free id rather than the closed one's place"
            );
            env.fund_vault(id, section.reference);
            // The second batch's coverage is checked the same way as the first's: the vault was
            // created after the cancellations, and "funded past it" would only be caught here by
            // this check.
            assert_still_funded(&mut env, id, "after funding the second batch");
            created_total += 1;
            stats.sections_used |= 1 << section_index(section);
            acts.push(Activity {
                id,
                section,
                recipient,
                alive: true,
                cancelled: false,
                claimed: 0,
            });
        }
    }
    assert_eq!(
        env.vesting_config().activity_count,
        created_total,
        "the activity counter after the second batch"
    );

    // --- the day sweep: the live activities' landmark points plus random days ---
    let max_full_release = acts
        .iter()
        .filter(|a| a.alive)
        .map(|a| a.section.full_release_day)
        .max()
        .expect("at least one activity is alive (the last one is never cancelled)");
    let mut days: BTreeSet<i64> = acts
        .iter()
        .filter(|a| a.alive)
        .flat_map(|a| a.section.anchors.iter().map(|(d, _)| *d))
        .collect();
    for _ in 0..profile.random_days(&mut rng) {
        days.insert(rng.range(1, max_full_release as u64 + 60) as i64);
    }

    let mut order: Vec<usize> = (0..acts.len()).collect();
    let mut visited_max_day = 0i64;
    for day in days {
        env.warp_to_day(tge_ts, day);
        visited_max_day = day;
        rng.shuffle(&mut order);
        for &i in &order {
            if !acts[i].alive {
                continue;
            }
            let anchor = acts[i].anchor(day);
            // A landmark day is always claimed - otherwise the check against the literal would
            // depend on chance.
            if anchor.is_none() && !rng.chance(profile.claim_pct()) {
                continue;
            }
            let before = vaults_snapshot(&env, &acts);
            let id = acts[i].id;
            let by_operator = rng.chance(30);
            let r = claim(&mut env, &acts[i], by_operator);
            let paid = match &r {
                Ok(_) => {
                    stats.paid_claims += 1;
                    true
                }
                Err(_) => {
                    // The only lawful rejection on this path: there is nothing to claim.
                    assert_eq!(
                        custom_error_code(&r),
                        Some(nothing_to_claim),
                        "day {day}, activity {id}: an unexpected claim rejection: {r:?}"
                    );
                    stats.empty_claims += 1;
                    false
                }
            };
            assert_activity_invariants(
                &env,
                &mut acts,
                i,
                &format!("day {day}, the claim of {id}"),
            );
            assert_neighbours_untouched(&env, &acts, &before, Some(id), "the claim");
            assert_eq!(
                env.vesting_config().activity_count,
                created_total,
                "a claim does not touch the id allocator"
            );
            if let Some(expected) = anchor {
                assert_eq!(
                    acts[i].claimed, expected,
                    "day {day} is a landmark for bucket {}: after the claim what is accounted is not what the reference of §10.5 gives",
                    acts[i].section.label
                );
                stats.anchor_checks += 1;
            }
            if paid {
                assert_still_funded(&mut env, id, &format!("day {day}, after the claim"));
                // An immediate repeat: the same amount is unlocked and there is nothing to release.
                let before = vaults_snapshot(&env, &acts);
                let r = claim(&mut env, &acts[i], !by_operator);
                assert_eq!(
                    custom_error_code(&r),
                    Some(nothing_to_claim),
                    "day {day}, activity {id}: the repeated claim must answer NothingToClaim: {r:?}"
                );
                assert_activity_invariants(
                    &env,
                    &mut acts,
                    i,
                    &format!("day {day}, the repeat of {id}"),
                );
                assert_neighbours_untouched(&env, &acts, &before, None, "the repeated claim");
                stats.empty_claims += 1;
            }
        }
    }

    // --- the terminal day: the payout hits the reference amount on every live activity ---
    // Strictly later than both the last day passed (rolling the Clock back is impossible)
    // and the full-release day of the set's longest schedule.
    let final_day = visited_max_day.max(max_full_release) + 1 + rng.below(30) as i64;
    env.warp_to_day(tge_ts, final_day);
    for i in 0..acts.len() {
        if !acts[i].alive {
            continue;
        }
        let (id, reference) = (acts[i].id, acts[i].reference());
        let r = claim(&mut env, &acts[i], rng.chance(50));
        if r.is_err() {
            assert_eq!(
                custom_error_code(&r),
                Some(nothing_to_claim),
                "the terminal claim of {id}: an unexpected rejection: {r:?}"
            );
            stats.empty_claims += 1;
        } else {
            stats.paid_claims += 1;
        }
        assert_activity_invariants(&env, &mut acts, i, "the terminal claim");
        assert_eq!(
            acts[i].claimed, reference,
            "after the full-release day the claimed of activity {id} == reference_amount"
        );
        assert_eq!(
            env.vault_balance(id),
            0,
            "the vault of activity {id} emptied out to exactly zero"
        );

        // Nothing beyond the reference amount is released, not even after the terminal
        // period.
        let r = claim(&mut env, &acts[i], false);
        assert_eq!(
            custom_error_code(&r),
            Some(nothing_to_claim),
            "a claim after the full draw-down of {id}: {r:?}"
        );
        assert_activity_invariants(&env, &mut acts, i, "the claim after the full draw-down");
        stats.empty_claims += 1;
    }

    // --- closing the fully drawn activities ---
    if profile.closes_activities() {
        for i in 0..acts.len() {
            if !acts[i].alive {
                continue;
            }
            let id = acts[i].id;
            let rent_before = env.lamports_of(&rent_collector);
            let vault_rent = env.lamports_of(&vault_pda(id));
            let record_rent = env.lamports_of(&vesting_pda(id));
            let before = vaults_snapshot(&env, &acts);

            env.expire_blockhash();
            let ix = close_vesting_ix(
                owner.pubkey(),
                id,
                beneficiary_token,
                rent_collector,
                win_mint,
            );
            let payer = env.payer.insecure_clone();
            env.send_ixs(&[ix], &payer.pubkey(), &[&payer, &owner])
                .unwrap_or_else(|e| panic!("close_vesting of activity {id} failed: {e:?}"));
            acts[i].alive = false;

            assert!(!env.vesting_exists(id), "the close removed record {id}");
            assert!(!env.vault_exists(id), "the close removed vault {id}");
            assert_eq!(
                env.lamports_of(&rent_collector) - rent_before,
                vault_rent + record_rent,
                "the rent of both accounts {id} went to the withdrawal recipient"
            );
            assert_eq!(
                env.vesting_config().activity_count,
                created_total,
                "a close does not decrement the id allocator"
            );
            assert_neighbours_untouched(&env, &acts, &before, Some(id), "the close");
            stats.closes += 1;
        }
    }

    // --- the final conservation against the real balances ---
    // The payout is counted from the recipients' accounts (one account per wallet rather
    // than per activity: a shared wallet has just one), and the expectation is formed
    // independently of the program's accounting: the vault was funded to exactly the
    // reference amount, so "the reference amount minus what is left in the vault" is
    // precisely what left it. The vaults of closed activities no longer exist and their
    // remainder is zero. Cancelled ones are not part of the sum: their whole reference
    // amount went to the withdrawal account (the next assertion), and their recipients'
    // ATAs were never created and read as 0.
    let recipients: BTreeSet<Pubkey> = acts.iter().map(|a| a.recipient.pubkey()).collect();
    let paid_out: u128 = recipients
        .iter()
        .map(|r| u128::from(env.recipient_ata_balance(r)))
        .sum();
    let drained_from_vaults: u128 = acts
        .iter()
        .filter(|a| !a.cancelled)
        .map(|a| {
            let left = if env.vault_exists(a.id) {
                env.vault_balance(a.id)
            } else {
                0
            };
            u128::from(a.reference() - left)
        })
        .sum();
    assert_eq!(
        paid_out, drained_from_vaults,
        "the total payout across the recipients' accounts == what left the vaults (the reference amount minus the remainder)"
    );

    // The shared wallet: two activities pay out to ONE account, and it must hold the sum
    // accounted across them - otherwise one activity's payout would overwrite the other's
    // or be counted twice.
    for recipient in &recipients {
        let owned: Vec<&Activity> = acts
            .iter()
            .filter(|a| a.recipient.pubkey() == *recipient)
            .collect();
        if owned.len() < 2 {
            continue;
        }
        let sum: u64 = owned.iter().map(|a| a.claimed).sum();
        assert_eq!(
            env.recipient_ata_balance(recipient),
            sum,
            "the shared ATA of the recipient of activities {:?} does not hold the sum accounted across them",
            owned.iter().map(|a| a.id).collect::<Vec<u64>>()
        );
        stats.shared_recipient_activities += owned.len() as u64;
    }
    assert_eq!(
        u128::from(env.token_balance(&beneficiary_token)),
        u128::from(cancelled_total),
        "exactly what the cancellations returned arrived at the withdrawal recipient's account (the closes take away empty vaults)"
    );
    assert_eq!(
        env.vesting_config().activity_count,
        created_total,
        "the id allocator is monotonic across the whole scenario"
    );

    // --- the scenario was not degenerate ---
    // Every uncancelled activity travels the path "zero → the reference amount", so it had
    // at least one productive claim and passed at least one landmark day (every bucket of
    // §10.5 has at least one).
    let alive_created =
        u64::try_from(acts.len()).expect("fewer activities than u64::MAX") - stats.cancels;
    assert!(
        stats.paid_claims >= alive_created,
        "coverage: {} productive claims is fewer than the {alive_created} surviving activities - the sweep degenerated",
        stats.paid_claims
    );
    assert!(
        stats.anchor_checks >= alive_created,
        "coverage: {} landmark checks is fewer than the {alive_created} surviving activities - the amounts were not checked against the references of §10.5",
        stats.anchor_checks
    );
    assert!(
        stats.empty_claims > 0,
        "coverage: not a single NothingToClaim rejection - the repeats were not checked"
    );
    stats
}

// ---------- The batch runner with seed context ----------

fn seed_override() -> Option<u64> {
    let v = std::env::var("VESTING_LIFECYCLE_SEED").ok()?;
    let v = v.trim();
    Some(match v.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16).expect("VESTING_LIFECYCLE_SEED: hex"),
        None => v.parse().expect("VESTING_LIFECYCLE_SEED: a number"),
    })
}

fn derive_seed(salt: u64, i: u64) -> u64 {
    Rng::new(BASE_SEED ^ (salt << 32) ^ i).next()
}

fn run_with_context(seed: u64, profile: Profile, idx: u64) -> Stats {
    match catch_unwind(AssertUnwindSafe(|| run_scenario(seed, profile, idx))) {
        Ok(stats) => stats,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .map(String::as_str)
                .or_else(|| payload.downcast_ref::<&str>().copied())
                .unwrap_or("<a non-string panic>");
            panic!(
                "scenario {profile:?} #{idx} failed; to reproduce: VESTING_LIFECYCLE_SEED={seed:#x}: {msg}"
            );
        }
    }
}

/// Runs a batch and returns the total coverage: the profile-specific expectations
/// ("there were cancellations", "there were closes") are checked by the calling test -
/// on a single scenario they are probabilistic, on a batch they must hold.
///
/// Bucket coverage is asserted here too: a scenario's third activity is pinned to a
/// bucket by its number within the batch (rotating over indices 2-4), and buckets 0-1
/// are present in every scenario - so a batch of three or more scenarios must exercise
/// all five schedules of §10.5. In single-seed reproduction mode the expectation does
/// not apply: exactly one scenario runs there.
fn run_batch(profile: Profile, count: u64, salt: u64) -> Stats {
    let mut total = Stats::default();
    if let Some(seed) = seed_override() {
        total.add(run_with_context(seed, profile, 0));
        return total;
    }
    for i in 0..count {
        total.add(run_with_context(derive_seed(salt, i), profile, i));
    }
    assert_eq!(
        total.sections_used, ALL_SECTIONS,
        "coverage: the {profile:?} batch exercised buckets {:#07b} out of {ALL_SECTIONS:#07b} - some schedules of §10.5 were not checked by any scenario",
        total.sections_used
    );
    total
}

// ---------- The tests ----------

#[test]
fn lifecycle_random_dense_claims() {
    let stats = run_batch(Profile::Dense, 6, 1);
    assert_eq!(
        stats.cancels, 0,
        "the Dense profile performs no cancellations"
    );
    assert_eq!(stats.closes, 0, "the Dense profile closes no activities");
    assert!(
        stats.shared_recipient_activities > 0,
        "coverage: no wallet received two activities - the \"two live activities on one recipient\" configuration was not checked"
    );
}

#[test]
fn lifecycle_random_sparse_claims() {
    let stats = run_batch(Profile::Sparse, 6, 2);
    // Sparse claims mean long skipped periods: what accrued is released in a single call.
    // There are still plenty of "nothing to release" rejections all the same - they come
    // from the mandatory repeat after every productive payout.
    assert!(stats.paid_claims > 0 && stats.empty_claims > 0);
}

#[test]
fn lifecycle_random_with_cancels_before_tge() {
    let stats = run_batch(Profile::WithCancels, 6, 3);
    assert!(
        stats.cancels > 0,
        "coverage: the batch performed no cancellation - the cancel_vesting branch was not exercised"
    );
}

#[test]
fn lifecycle_random_full_drain_then_close() {
    let stats = run_batch(Profile::FullDrain, 4, 4);
    assert!(
        stats.closes > 0,
        "coverage: the batch closed no activity - the close_vesting branch was not exercised"
    );
}
