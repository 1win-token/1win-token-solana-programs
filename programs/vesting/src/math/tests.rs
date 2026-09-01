//! Unit tests for the unlock math (Arch §10): time boundaries, the five
//! reference schedules of §10.5, domain edges, properties over random inputs,
//! and the non-overlap of the seed schemes of §6.1.
//!
//! The expected values are hand-written literals from `vesting_expected::vesting`
//! (a crate with zero dependencies): the `vested()` formula is not
//! reproduced here, or the test would be checking the code against itself. Time
//! is likewise built from the reference `spec::MONTH` / `spec::DAY_SECS` rather
//! than from the program's constants - a separate pinning test asserts they agree.

use core::ops::RangeInclusive;

use anchor_lang::prelude::{Pubkey, Space};
use proptest::prelude::*;
use vesting_expected::vesting as spec;

use super::*;
use crate::state::{Vesting, VestingConfig, MAX_TGE_DELAY, TOKEN_DECIMALS};

/// The tests' reference TGE - a non-zero date (2027-01-15). Zero means "TGE is
/// not set" (Arch §4.3) and is never fed into the unlock computation.
const TGE: i64 = 1_800_000_000;

/// The instant day `d` begins: with the numbering of Arch §10.1
/// (`day = 1 + n·30`), day `d` starts at `tge + (d − 1)·DAY_SECS`. A unit
/// conversion, not a formula.
fn at_day(tge: i64, day: i64) -> i64 {
    tge + (day - 1) * spec::DAY_SECS
}

/// The instant period `p` begins: `tge + p·MONTH` (Arch §10.1).
fn at_period(tge: i64, p: i64) -> i64 {
    tge + p * spec::MONTH
}

fn sched(reference_amount: u64, tge_bps: u16, monthly_bps: u16, cliff_months: u16) -> Schedule {
    Schedule {
        reference_amount,
        tge_bps,
        monthly_bps,
        cliff_months,
    }
}

/// The "Marketing" schedule (§10.5), reference example V1.
fn marketing() -> Schedule {
    sched(
        spec::V1_REFERENCE,
        spec::MARKETING_TGE_BPS,
        spec::MARKETING_MONTHLY_BPS,
        spec::MARKETING_CLIFF_MONTHS,
    )
}

/// The "Ecosystem" schedule (§10.5), reference example V2.
fn ecosystem() -> Schedule {
    sched(
        spec::V2_REFERENCE,
        spec::ECOSYSTEM_TGE_BPS,
        spec::ECOSYSTEM_MONTHLY_BPS,
        spec::ECOSYSTEM_CLIFF_MONTHS,
    )
}

/// The "Community Rewards" schedule (§10.5) with the degenerate reference of example
/// V4 (1000 units): at that scale the flooring loss is visible in whole units.
fn community() -> Schedule {
    sched(
        spec::V4_REFERENCE,
        spec::COMMUNITY_TGE_BPS,
        spec::COMMUNITY_MONTHLY_BPS,
        spec::COMMUNITY_CLIFF_MONTHS,
    )
}

/// The "Treasury" schedule (§10.5), reference example V3 - the only bucket with
/// a cliff.
fn treasury() -> Schedule {
    sched(
        spec::V3_REFERENCE,
        spec::TREASURY_TGE_BPS,
        spec::TREASURY_MONTHLY_BPS,
        spec::TREASURY_CLIFF_MONTHS,
    )
}

/// The "Liquidity" schedule (§10.5), reference LIQUIDITY_REFERENCE.
fn liquidity() -> Schedule {
    sched(
        spec::LIQUIDITY_REFERENCE,
        spec::LIQUIDITY_TGE_BPS,
        spec::LIQUIDITY_MONTHLY_BPS,
        spec::LIQUIDITY_CLIFF_MONTHS,
    )
}

/// The combination of `cliff > 0` and `tge_bps > 0`: the validation of §9.5
/// permits it and no real bucket uses it - without this case the branch
/// "`p < s` yet `tge_amt > 0`" would stay uncovered.
fn mixed() -> Schedule {
    sched(
        spec::MIXED_REFERENCE,
        spec::MIXED_TGE_BPS,
        spec::MIXED_MONTHLY_BPS,
        spec::MIXED_CLIFF_MONTHS,
    )
}

// ===== Constant pinning =====

/// The program's constants must match the reference ones: every expectation in
/// this file is built on a 30-day month (§10.1) and on the schedule ceilings, so
/// a divergence would invalidate the whole file.
#[test]
fn spec_constants_match_expected() {
    assert_eq!(
        MONTH,
        spec::MONTH,
        "the program's MONTH diverged from the 30-day month of Arch §10.1"
    );
    assert_eq!(
        MONTH,
        spec::DAY_SECS * 30,
        "MONTH must be exactly 30 days: otherwise the day numbering of §10.1 shifts"
    );
    assert_eq!(
        u64::from(BPS_DENOM),
        spec::BPS_DENOM,
        "the basis-point denominator diverged from 10000 = 100% (§10.1)"
    );
    assert_eq!(
        MAX_SCHEDULE_MONTHS, 120,
        "the cliff + M ceiling changed - the boundaries are no longer these"
    );
    assert_eq!(MAX_TGE_DELAY, 365 * 86_400, "the TGE delay ceiling changed");
    assert_eq!(
        spec::TOKEN_UNIT,
        10u64.pow(u32::from(TOKEN_DECIMALS)),
        "the reference scale (1 $1WIN = 10⁸ token units) diverged from the program's TOKEN_DECIMALS"
    );
    assert_eq!(
        spec::EDGE_MAX_PRODUCT_U128,
        u128::from(u64::MAX) * u128::from(spec::BPS_DENOM),
        "the limiting intermediate product of §10.4 (u64::MAX × 10000) is computed wrong - \
         the u128 width headroom is built on it"
    );

    // Account sizes (Arch §5.1, §5.2): 8 discriminator bytes plus the body. A new
    // field changes both numbers - the edit must be deliberate (test, document,
    // rent estimate R-8), never incidental.
    assert_eq!(
        8 + VestingConfig::INIT_SPACE,
        218,
        "the VestingConfig size diverged from Arch §5.1 (210 B body + discriminator)"
    );
    assert_eq!(
        8 + Vesting::INIT_SPACE,
        62,
        "the Vesting size diverged from Arch §5.2 (54 B body + discriminator)"
    );
}

// ===== Time boundaries (the "Ecosystem" schedule, example V2) =====

/// Before TGE nothing is unlocked - the first branch of §10.2, even one second
/// before the start.
#[test]
fn time_before_tge_is_zero() {
    assert_eq!(
        vested(TGE - 1, TGE, &ecosystem()),
        Ok(0),
        "one second before TGE nothing is unlocked (§10.2, first branch)"
    );
}

/// The TGE instant (day 1, `p = 0 < s`): only the TGE share is released, no
/// monthly tranches.
#[test]
fn time_at_tge_gives_tge_share_only() {
    assert_eq!(
        vested(TGE, TGE, &ecosystem()),
        Ok(spec::V2_VESTED_DAY_1),
        "at TGE exactly the TGE share must be released (10% of the V2 reference)"
    );
}

/// The last second of period 0 (day 30, `tge + MONTH − 1`): the period's value
/// holds right up to the start of the next one - a tranche does not arrive
/// "during" the month.
#[test]
fn time_last_second_of_period_zero_holds_tge_share() {
    assert_eq!(
        vested(TGE + spec::MONTH - 1, TGE, &ecosystem()),
        Ok(spec::V2_VESTED_DAY_1),
        "one second before day 31 the first monthly tranche is not yet accrued"
    );
}

/// The start of period 1 (day 31): the first monthly tranche lands exactly on the
/// boundary.
#[test]
fn time_first_monthly_tranche_at_day_31() {
    assert_eq!(
        vested(TGE + spec::MONTH, TGE, &ecosystem()),
        Ok(spec::V2_VESTED_DAY_31),
        "on day 31 the first monthly tranche must accrue (10% + 5%)"
    );
}

/// The pre-terminal period `p = s + M − 2 = 17`: `M − 1` tranches have accrued
/// and the sum is strictly below the reference amount (§10.3).
#[test]
fn time_pre_terminal_period_is_below_reference() {
    let got = vested(at_period(TGE, 17), TGE, &ecosystem())
        .expect("the pre-terminal period of a valid schedule yields no error");
    assert_eq!(
        got,
        spec::V2_VESTED_DAY_511,
        "at period 17 (day 511) 17 of the 18 monthly tranches have accrued"
    );
    assert!(
        got < spec::V2_REFERENCE,
        "a non-terminal sum must be STRICTLY below the reference amount (§10.3): {got}"
    );
}

/// The terminal period `p = s + M − 1 = 18` (day 541): exactly the reference amount.
#[test]
fn time_terminal_period_is_exactly_reference() {
    assert_eq!(
        vested(at_day(TGE, spec::ECOSYSTEM_FULL_RELEASE_DAY), TGE, &ecosystem()),
        Ok(spec::V2_VESTED_DAY_541),
        "the terminal branch must return EXACTLY the reference amount, picking up the flooring remainders"
    );
}

/// Far beyond the terminal period the value does not grow: `months` is capped by
/// `min(·, M)`.
#[test]
fn time_far_future_stays_at_reference() {
    assert_eq!(
        vested(at_period(TGE, 1_000), TGE, &ecosystem()),
        Ok(spec::V2_VESTED_DAY_541),
        "after 1000 months the same amount is unlocked - the reference ceiling (§10.3)"
    );
}

/// `now = i64::MAX`: dividing by MONTH and `min(·, M)` keep the value at the
/// reference amount; there must be neither a panic nor a MathOverflow - the input
/// comes from `Clock`, but the formula must still be total (§10.4).
#[test]
fn time_i64_max_does_not_panic() {
    assert_eq!(
        vested(i64::MAX, TGE, &ecosystem()),
        Ok(spec::V2_VESTED_DAY_541),
        "at i64::MAX the unlock must stay at the reference amount without panicking"
    );
    // The negative side of the same guard: a difference outside i64. In production
    // `tge_ts` is already bounded (§9.2), so this branch is defensive - but it
    // must still yield an error.
    assert_eq!(
        vested(i64::MAX, i64::MIN, &ecosystem()),
        Err(VestingError::MathOverflow),
        "a now − tge_ts difference outside i64 must yield MathOverflow, not a panic (§10.4)"
    );
}

// ===== The five reference schedules of §10.5 =====

/// "100% at TGE": there are no monthly tranches, the tranche count is undefined,
/// and the whole amount is available from the TGE instant onwards, forever.
#[test]
fn section_marketing_matches_table() {
    let sch = marketing();
    assert_eq!(
        validate_schedule(sch.tge_bps, sch.monthly_bps, sch.cliff_months),
        Ok(()),
        "the \"100% at TGE\" schedule is valid: the whole amount at TGE, no monthly tranches"
    );
    assert_eq!(
        tranches_m(sch.tge_bps, sch.monthly_bps),
        Err(VestingError::InvalidSchedule),
        "M is undefined when monthly_bps == 0: the call is an error, not a division by zero"
    );
    assert_eq!(
        vested(TGE - 1, TGE, &sch),
        Ok(spec::V1_VESTED_BEFORE_TGE),
        "before TGE nothing is unlocked"
    );
    assert_eq!(
        vested(at_day(TGE, spec::MARKETING_FULL_RELEASE_DAY), TGE, &sch),
        Ok(spec::V1_VESTED_FROM_TGE),
        "the full-release day of \"100% at TGE\" is day 1: the whole reference at once"
    );
    assert_eq!(
        vested(at_period(TGE, 120), TGE, &sch),
        Ok(spec::V1_VESTED_FROM_TGE),
        "later the value does not change - the monthly_bps == 0 branch is time-independent"
    );
}

/// "Ecosystem": `M = 18`, first tranche on day 31, full release on day 541.
#[test]
fn section_games_matches_table() {
    let sch = ecosystem();
    assert_eq!(
        tranches_m(sch.tge_bps, sch.monthly_bps),
        Ok(u32::from(spec::ECOSYSTEM_TRANCHES_M)),
        "M of the \"Ecosystem\" schedule diverged from the table of §10.5"
    );
    assert_eq!(
        vested(
            at_day(TGE, spec::ECOSYSTEM_FIRST_MONTHLY_DAY) - 1,
            TGE,
            &sch
        ),
        Ok(spec::V2_VESTED_DAY_1),
        "one second before the first tranche only the TGE share is released"
    );
    assert_eq!(
        vested(at_day(TGE, spec::ECOSYSTEM_FIRST_MONTHLY_DAY), TGE, &sch),
        Ok(spec::V2_VESTED_DAY_31),
        "on the first tranche's day - the TGE share plus one monthly tranche"
    );
    assert_eq!(
        vested(at_day(TGE, spec::ECOSYSTEM_FULL_RELEASE_DAY) - 1, TGE, &sch),
        Ok(spec::V2_VESTED_DAY_511),
        "one second before the full release M − 1 tranches have accrued (strictly below the reference)"
    );
    assert_eq!(
        vested(at_day(TGE, spec::ECOSYSTEM_FULL_RELEASE_DAY), TGE, &sch),
        Ok(spec::V2_VESTED_DAY_541),
        "on the full-release day - EXACTLY the reference amount"
    );
}

/// "Community Rewards" on the degenerate reference of 1000 units: `M = 12`, first
/// tranche on day 31, full release on day 361.
#[test]
fn section_wager_matches_table() {
    let sch = community();
    assert_eq!(
        tranches_m(sch.tge_bps, sch.monthly_bps),
        Ok(u32::from(spec::COMMUNITY_TRANCHES_M)),
        "M of the \"Community Rewards\" schedule diverged from the table of §10.5"
    );
    assert_eq!(
        vested(
            at_day(TGE, spec::COMMUNITY_FIRST_MONTHLY_DAY) - 1,
            TGE,
            &sch
        ),
        Ok(spec::V4_TGE_AMT),
        "with tge_bps = 0 nothing is unlocked before the first tranche"
    );
    assert_eq!(
        vested(at_day(TGE, spec::COMMUNITY_FIRST_MONTHLY_DAY), TGE, &sch),
        Ok(spec::V4_MONTHLY_AMT),
        "on the first tranche's day exactly one monthly tranche is released (83 units)"
    );
    assert_eq!(
        vested(at_day(TGE, spec::COMMUNITY_FULL_RELEASE_DAY) - 1, TGE, &sch),
        Ok(spec::V4_VESTED_MONTHS_11),
        "one second before the full release 11 tranches have accrued = 913, not 1000"
    );
    assert_eq!(
        vested(at_day(TGE, spec::COMMUNITY_FULL_RELEASE_DAY), TGE, &sch),
        Ok(spec::V4_VESTED_MONTHS_12),
        "on the full-release day the terminal branch tops up to the reference of 1000"
    );
}

/// "Treasury": `M = 25`, a 9-month cliff, first tranche on day 271, full release
/// on day 991.
#[test]
fn section_treasury_matches_table() {
    let sch = treasury();
    assert_eq!(
        tranches_m(sch.tge_bps, sch.monthly_bps),
        Ok(u32::from(spec::TREASURY_TRANCHES_M)),
        "M of the \"Treasury\" schedule diverged from the table of §10.5"
    );
    assert_eq!(
        vested(at_day(TGE, spec::TREASURY_FIRST_MONTHLY_DAY) - 1, TGE, &sch),
        Ok(spec::V3_TGE_AMT),
        "throughout the cliff nothing is unlocked (tge_bps = 0)"
    );
    assert_eq!(
        vested(at_day(TGE, spec::TREASURY_FIRST_MONTHLY_DAY), TGE, &sch),
        Ok(spec::V3_VESTED_DAY_271),
        "the first tranche lands exactly on day 271 (p = s = 9)"
    );
    assert_eq!(
        vested(at_day(TGE, spec::TREASURY_FULL_RELEASE_DAY) - 1, TGE, &sch),
        Ok(spec::V3_VESTED_DAY_961),
        "one second before the full release 24 of the 25 tranches have accrued"
    );
    assert_eq!(
        vested(at_day(TGE, spec::TREASURY_FULL_RELEASE_DAY), TGE, &sch),
        Ok(spec::V3_VESTED_DAY_991),
        "on day 991 - EXACTLY the reference amount"
    );
}

/// "Liquidity": `M = 12`, first tranche on day 31, full release on day 361.
#[test]
fn section_liquidity_matches_table() {
    let sch = liquidity();
    assert_eq!(
        tranches_m(sch.tge_bps, sch.monthly_bps),
        Ok(u32::from(spec::LIQUIDITY_TRANCHES_M)),
        "M of the \"Liquidity\" schedule diverged from the table of §10.5"
    );
    assert_eq!(
        vested(
            at_day(TGE, spec::LIQUIDITY_FIRST_MONTHLY_DAY) - 1,
            TGE,
            &sch
        ),
        Ok(spec::LIQUIDITY_TGE_AMT),
        "one second before the first tranche only the TGE share is released (10%)"
    );
    assert_eq!(
        vested(at_day(TGE, spec::LIQUIDITY_FIRST_MONTHLY_DAY), TGE, &sch),
        Ok(spec::LIQUIDITY_VESTED_DAY_31),
        "on day 31 - the TGE share plus the 7.5% monthly tranche"
    );
    assert_eq!(
        vested(at_day(TGE, spec::LIQUIDITY_FULL_RELEASE_DAY) - 1, TGE, &sch),
        Ok(spec::LIQUIDITY_VESTED_DAY_331),
        "one second before the full release 11 of the 12 tranches have accrued"
    );
    assert_eq!(
        vested(at_day(TGE, spec::LIQUIDITY_FULL_RELEASE_DAY), TGE, &sch),
        Ok(spec::LIQUIDITY_VESTED_DAY_361),
        "on day 361 - EXACTLY the reference amount"
    );
}

// ===== The cliff (Treasury, example V3) and the "cliff + TGE share" combination =====

/// The last day of the cliff (day 270, `p = 8 < s = 9`): nothing is unlocked.
#[test]
fn cliff_last_day_of_cliff_is_zero() {
    assert_eq!(
        vested(at_day(TGE, 270), TGE, &treasury()),
        Ok(spec::V3_VESTED_DAY_270),
        "inside the cliff no monthly tranches accrue, and tge_bps = 0 ⟹ 0"
    );
}

/// The first day after the cliff (day 271, `p = 9 = s`): exactly one tranche.
#[test]
fn cliff_first_tranche_on_day_271() {
    assert_eq!(
        vested(at_day(TGE, 271), TGE, &treasury()),
        Ok(spec::V3_VESTED_DAY_271),
        "the first Treasury monthly tranche lands exactly on day 271 (4%)"
    );
}

/// `cliff > 0` together with `tge_bps > 0`: inside the cliff the TGE share is
/// released rather than 0 - the `p < s` branch returns `tge_amt`, not zero.
#[test]
fn cliff_with_tge_share_pays_tge_during_cliff() {
    let sch = mixed();
    assert_eq!(
        tranches_m(sch.tge_bps, sch.monthly_bps),
        Ok(u32::from(spec::MIXED_TRANCHES_M)),
        "M of the combined schedule (1000, 400) = 23"
    );
    assert_eq!(
        validate_schedule(sch.tge_bps, sch.monthly_bps, sch.cliff_months),
        Ok(()),
        "the validation of §9.5 allows cliff > 0 with tge_bps > 0 (cliff + M = 32 ≤ 120)"
    );
    assert_eq!(
        vested(at_day(TGE, 270), TGE, &sch),
        Ok(spec::MIXED_VESTED_DAY_270),
        "inside the cliff with tge_bps > 0 the TGE share (10%) is released, not 0"
    );
}

/// The same combination after the cliff: the TGE share plus the first monthly
/// tranche.
#[test]
fn cliff_with_tge_share_adds_monthly_after_cliff() {
    let sch = mixed();
    assert_eq!(
        vested(at_day(TGE, 271), TGE, &sch),
        Ok(spec::MIXED_VESTED_DAY_271),
        "on day 271 one monthly tranche is added to the TGE share (10% + 4%)"
    );
    assert_eq!(
        vested(at_day(TGE, spec::MIXED_FULL_RELEASE_DAY), TGE, &sch),
        Ok(spec::MIXED_REFERENCE),
        "the combined schedule reaches full release on day 931 (p = s + M − 1 = 31)"
    );
}

// ===== "100% at TGE" (example V1) =====

/// Before TGE the `monthly_bps == 0` branch is unreachable: time is checked first.
#[test]
fn tge_only_before_tge_is_zero() {
    assert_eq!(
        vested(TGE - 1, TGE, &marketing()),
        Ok(spec::V1_VESTED_BEFORE_TGE),
        "the time check comes FIRST (§10.2): before TGE even \"100% at TGE\" yields 0"
    );
}

/// At the TGE instant the whole reference amount is released.
#[test]
fn tge_only_at_tge_gives_reference() {
    assert_eq!(
        vested(TGE, TGE, &marketing()),
        Ok(spec::V1_VESTED_FROM_TGE),
        "the monthly_bps == 0 branch must return the whole reference amount"
    );
}

/// Ten years later - the same value: the branch does not depend on `now`.
#[test]
fn tge_only_stays_at_reference_ten_years_later() {
    assert_eq!(
        vested(TGE + 10 * 365 * spec::DAY_SECS, TGE, &marketing()),
        Ok(spec::V1_VESTED_FROM_TGE),
        "\"100% at TGE\" neither grows nor shrinks over time"
    );
}

/// With `monthly_bps == 0` the value of `cliff_months` does not affect the payout
/// (the branch returns the reference amount before `s` is computed), yet the
/// validation rejects that combination - meaningful input must not be silently
/// ignored.
#[test]
fn tge_only_ignores_cliff_months() {
    for cliff in [0u16, 9, u16::MAX] {
        let sch = sched(
            spec::V1_REFERENCE,
            spec::MARKETING_TGE_BPS,
            spec::MARKETING_MONTHLY_BPS,
            cliff,
        );
        assert_eq!(
            vested(TGE, TGE, &sch),
            Ok(spec::V1_VESTED_FROM_TGE),
            "cliff = {cliff}: with monthly_bps == 0 no branch reads the cliff"
        );
        assert_eq!(
            vested(at_period(TGE, 120), TGE, &sch),
            Ok(spec::V1_VESTED_FROM_TGE),
            "cliff = {cliff}: beyond the schedule ceiling (120 months) - the same reference"
        );
    }
    assert_eq!(
        validate_schedule(spec::MARKETING_TGE_BPS, spec::MARKETING_MONTHLY_BPS, 0),
        Ok(()),
        "\"100% at TGE\" without a cliff is a valid schedule"
    );
    assert_eq!(
        validate_schedule(spec::MARKETING_TGE_BPS, spec::MARKETING_MONTHLY_BPS, 9),
        Err(VestingError::InvalidSchedule),
        "a cliff with monthly_bps == 0 must be rejected at the entry, not ignored"
    );
}

/// The second sub-branch of the same guard: with `monthly_bps == 0` the TGE share
/// must be exactly 10000. A smaller share means there are no monthly tranches,
/// nothing to top up the remainder with, and the reference amount NEVER unlocks
/// in full - the funds stay locked in the activity vault until `cancel_vesting`.
#[test]
fn tge_only_requires_full_tge_share() {
    assert_eq!(
        validate_schedule(0, 0, 0),
        Err(VestingError::InvalidSchedule),
        "schedule (0, 0): nothing ever unlocks - it must be rejected"
    );
    assert_eq!(
        validate_schedule(5_000, 0, 0),
        Err(VestingError::InvalidSchedule),
        "schedule (5000, 0): 50% at TGE with no monthly tranches - the second half of \
         the reference amount never unlocks"
    );
    assert_eq!(
        validate_schedule(9_999, 0, 0),
        Err(VestingError::InvalidSchedule),
        "boundary: 9999 bps with monthly_bps == 0 is 1 bp short of 100% and must be rejected"
    );
    // The positive side of the boundary: exactly 10000 bps passes.
    assert_eq!(
        validate_schedule(BPS_DENOM, 0, 0),
        Ok(()),
        "exactly 100% at TGE with monthly_bps == 0 is the only valid schedule of this branch"
    );
    // An input that tells the share gate apart from the `monthly_bps == 0` branch:
    // with monthly_bps > 0 both yield the same code, here only the share gate fires.
    assert_eq!(
        validate_schedule(10_001, 0, 0),
        Err(VestingError::InvalidBps),
        "tge_bps = 10001 must be rejected by the share gate, not fall through to the \
         InvalidSchedule of the monthly_bps == 0 branch"
    );
}

// ===== Flooring and the top-up by the last tranche (example V4) =====

/// The monthly tranche is `floor(1000 × 834 / 10000) = 83`: observed as the
/// payout of the first period (`tge_amt = 0`).
#[test]
fn floor_monthly_amount_is_83() {
    assert_eq!(
        vested(at_period(TGE, 1), TGE, &community()),
        Ok(spec::V4_MONTHLY_AMT),
        "the monthly share is rounded down: floor(83.4) = 83, losing 0.4 unit per tranche"
    );
}

/// Eleven tranches fall short of the reference amount: 11 × 83 = 913 (the
/// pre-terminal period `p = 11 < M = 12`).
#[test]
fn floor_eleven_tranches_fall_short_of_reference() {
    assert_eq!(
        vested(at_period(TGE, 11), TGE, &community()),
        Ok(spec::V4_VESTED_MONTHS_11),
        "after 11 tranches 913 is unlocked - flooring keeps it from reaching 1000"
    );
}

/// The terminal tranche returns exactly the reference amount.
#[test]
fn floor_terminal_tranche_reaches_reference_exactly() {
    assert_eq!(
        vested(at_period(TGE, 12), TGE, &community()),
        Ok(spec::V4_VESTED_MONTHS_12),
        "at the terminal period (months == M = 12) EXACTLY the reference 1000 is released"
    );
}

/// The last tranche is 87 units rather than the nominal 83: it picks up
/// everything flooring dropped (§10.3).
#[test]
fn floor_last_tranche_is_eighty_seven_not_eighty_three() {
    let sch = community();
    let before = vested(at_period(TGE, 11), TGE, &sch)
        .expect("the pre-terminal period of a valid schedule yields no error");
    let after = vested(at_period(TGE, 12), TGE, &sch)
        .expect("the terminal period of a valid schedule yields no error");
    assert_eq!(
        after - before,
        spec::V4_LAST_TRANCHE,
        "the last tranche must hand out the remainder 1000 − 913 = 87 units"
    );
    assert_ne!(
        after - before,
        spec::V4_MONTHLY_AMT,
        "the last tranche is NOT the nominal monthly one (83) - the terminal branch tops up the flooring"
    );
}

/// Across the whole period grid the payout never exceeds the reference amount, and
/// the naive sum `13 × 83 = 1079` is unreachable.
#[test]
fn floor_never_exceeds_reference_over_period_grid() {
    let sch = community();
    for p in 0..=20i64 {
        let got = vested(at_period(TGE, p), TGE, &sch)
            .expect("the formula is total on a valid schedule (§10.4)");
        assert!(
            got <= spec::V4_REFERENCE,
            "period {p}: unlocked {got} > the reference {} - the vault would be overdrawn",
            spec::V4_REFERENCE
        );
        assert_ne!(
            got,
            spec::V4_NAIVE_SUM_UNREACHABLE,
            "period {p}: the naive share sum 13 × 83 = 1079 was reached (the §10.3 ceiling is broken)"
        );
        if p >= i64::from(spec::COMMUNITY_TRANCHES_M) {
            assert_eq!(
                got,
                spec::V4_VESTED_MONTHS_12,
                "period {p}: past the terminal one the value must hold at the reference amount"
            );
        }
    }
}

// ===== Accrual across skipped periods (example V5) =====

/// The recipient did not claim until day 151: a single claim releases the TGE
/// share and five monthly tranches at once (`claimed = 0`, so
/// `claimable == vested`).
#[test]
fn accrual_skipped_periods_sum_up() {
    assert_eq!(
        vested(at_day(TGE, 151), TGE, &ecosystem()),
        Ok(spec::V2_VESTED_DAY_151),
        "on day 151 35% of the reference has accrued - skipping periods loses nothing"
    );
}

/// After the day-151 claim the remainder is released at the terminal period:
/// `claimable = vested − claimed`.
#[test]
fn accrual_remainder_claimable_at_terminal_period() {
    let got = vested(
        at_day(TGE, spec::ECOSYSTEM_FULL_RELEASE_DAY),
        TGE,
        &ecosystem(),
    )
    .expect("the terminal period of a valid schedule yields no error");
    assert_eq!(
        got,
        spec::V5_CLAIMED_AFTER_DAY_541,
        "on day 541 the whole reference amount is unlocked"
    );
    assert_eq!(
        got - spec::V5_CLAIMED_AFTER_DAY_151,
        spec::V5_CLAIMABLE_DAY_541,
        "the remainder after the day-151 claim must be exactly 65% of the reference"
    );
}

// ===== Domain edges =====

/// `reference_amount = u64::MAX` in the "100% at TGE" branch: the reference amount
/// is handed out without any multiplication, so no downcast is needed.
#[test]
fn edge_reference_u64_max_with_full_tge_share() {
    let sch = sched(
        spec::EDGE_MAX_REFERENCE,
        spec::MARKETING_TGE_BPS,
        spec::MARKETING_MONTHLY_BPS,
        0,
    );
    assert_eq!(
        vested(TGE - 1, TGE, &sch),
        Ok(0),
        "before TGE it is 0 even at the maximum reference amount"
    );
    assert_eq!(
        vested(TGE, TGE, &sch),
        Ok(spec::EDGE_MAX_REFERENCE),
        "the monthly_bps == 0 branch must return u64::MAX without a MathOverflow"
    );
    assert_eq!(
        vested(i64::MAX, TGE, &sch),
        Ok(spec::EDGE_MAX_REFERENCE),
        "at i64::MAX - the same value"
    );
}

/// The `(9999, 1)` schedule: `M = 1`, the intermediate `u64::MAX × 9999 ≈ 1.8·10²³`
/// lives in u128, and the single tranche tops up what flooring dropped.
#[test]
fn edge_bps_9999_1_single_tranche_tops_up_to_reference() {
    let sch = sched(spec::EDGE_MAX_REFERENCE, 9_999, 1, 0);
    assert_eq!(
        tranches_m(9_999, 1),
        Ok(u32::from(spec::EDGE_M_BPS_9999_1)),
        "ceil((10000 − 9999) / 1) = 1 - a single tranche tops up everything"
    );
    assert_eq!(
        validate_schedule(9_999, 1, 0),
        Ok(()),
        "cliff + M = 1 ≤ 120 - the schedule is valid"
    );
    assert_eq!(
        vested(TGE, TGE, &sch),
        Ok(spec::EDGE_MAX_REF_TGE_AMT_9999),
        "the TGE share of u64::MAX is computed in u128 and narrowed after the division (§10.4)"
    );
    let terminal = vested(at_period(TGE, 1), TGE, &sch)
        .expect("the terminal period yields no error: the value equals the reference amount");
    assert_eq!(
        terminal,
        spec::EDGE_MAX_REFERENCE,
        "the terminal branch must return EXACTLY u64::MAX"
    );
    assert!(
        terminal > spec::EDGE_MAX_REF_NAIVE_SUM,
        "the terminal tranche must top up the 1 unit lost to flooring: {terminal} vs the naive sum {}",
        spec::EDGE_MAX_REF_NAIVE_SUM
    );
}

/// The `(0, 1)` schedule yields `M = 10000` (≈833 years) - the formula can express
/// it, and the `MAX_SCHEDULE_MONTHS` ceiling must reject it. The
/// "boundary passes / boundary + 1 is rejected" pair, at the minimum
/// `monthly_bps` with `cliff = 0`.
#[test]
fn edge_bps_0_1_max_tranches_rejected_by_schedule_cap() {
    assert_eq!(
        tranches_m(0, 1),
        Ok(u32::from(spec::EDGE_M_BPS_0_1)),
        "ceil(10000 / 1) = 10000 - the longest schedule the formula can express"
    );
    assert_eq!(
        validate_schedule(0, 1, 0),
        Err(VestingError::InvalidSchedule),
        "monthly_bps = 1 (M = 10000) must be rejected by the schedule ceiling"
    );
    assert_eq!(
        validate_schedule(0, 84, 0),
        Ok(()),
        "monthly_bps = 84 yields M = 120 - exactly the ceiling, the schedule passes"
    );
    assert_eq!(
        validate_schedule(0, 83, 0),
        Err(VestingError::InvalidSchedule),
        "monthly_bps = 83 yields M = 121 - one above the ceiling, it must be rejected"
    );
}

/// The tightest product margin: `months = 9999` monthly shares of `u64::MAX` - the
/// result still fits into u64, and the terminal period tops up to the reference
/// amount.
#[test]
fn edge_bps_0_1_widest_product_downcasts() {
    let sch = sched(spec::EDGE_MAX_REFERENCE, 0, 1, 0);
    assert_eq!(
        vested(at_period(TGE, 1), TGE, &sch),
        Ok(spec::EDGE_MAX_REF_MONTHLY_AMT_1),
        "with tge_bps = 0 the first tranche equals the monthly share floor(u64::MAX / 10000)"
    );
    assert_eq!(
        vested(
            at_period(TGE, i64::from(spec::EDGE_MONTHS_BPS_0_1)),
            TGE,
            &sch
        ),
        Ok(spec::EDGE_MAX_REF_VESTED_MONTHS_9999),
        "9999 × floor(u64::MAX / 10000) must be computed in u128 and narrowed into u64"
    );
    assert_eq!(
        vested(at_period(TGE, i64::from(spec::EDGE_M_BPS_0_1)), TGE, &sch),
        Ok(spec::EDGE_MAX_REFERENCE),
        "at the terminal period 10000 - EXACTLY the reference amount"
    );
}

/// `monthly_bps = 10000`: `M = 1`, the whole reference amount arrives in one tranche.
#[test]
fn edge_monthly_bps_10000_single_tranche() {
    assert_eq!(
        tranches_m(0, 10_000),
        Ok(u32::from(spec::EDGE_M_MONTHLY_10000)),
        "ceil(10000 / 10000) = 1"
    );
    assert_eq!(
        validate_schedule(0, 10_000, 0),
        Ok(()),
        "schedule (0, 10000) is valid: cliff + M = 1"
    );
    let sch = sched(spec::V2_REFERENCE, 0, 10_000, 0);
    assert_eq!(
        vested(TGE, TGE, &sch),
        Ok(0),
        "before the first tranche, with tge_bps = 0, nothing is unlocked"
    );
    assert_eq!(
        vested(at_period(TGE, 1), TGE, &sch),
        Ok(spec::V2_REFERENCE),
        "the single tranche releases the whole reference amount"
    );
    assert_eq!(
        validate_schedule(10_000, 10_000, 0),
        Err(VestingError::InvalidSchedule),
        "monthly_bps > 0 with tge_bps = 10000 - the schedule sums above the reference and must be rejected"
    );
    // The counterpart of the 10000 boundary: 10001 is a share above 100%.
    assert_eq!(
        validate_schedule(0, 10_001, 0),
        Err(VestingError::InvalidBps),
        "monthly_bps = 10001 is a share above 100% and must be rejected"
    );
    assert_eq!(
        validate_schedule(10_001, 500, 0),
        Err(VestingError::InvalidBps),
        "tge_bps = 10001 is a share above 100% and must be rejected"
    );
    assert_eq!(
        tranches_m(10_001, 1),
        Err(VestingError::InvalidBps),
        "M is undefined for a share above 100%: the remainder 10000 − tge_bps would go negative"
    );
}

/// The maximum cliff at the schedule ceiling (`M = 1` ⟹ `cliff ≤ 119`) with TGE at
/// the `MAX_TGE_DELAY` ceiling: the terminal period stays inside the i64 range.
#[test]
fn edge_max_cliff_at_schedule_cap() {
    assert_eq!(
        validate_schedule(0, 10_000, 119),
        Ok(()),
        "cliff 119 + M 1 = 120 - exactly the ceiling, the schedule passes"
    );
    assert_eq!(
        validate_schedule(0, 10_000, 120),
        Err(VestingError::InvalidSchedule),
        "cliff 120 + M 1 = 121 - the ceiling must reject it"
    );
    let tge = TGE + MAX_TGE_DELAY;
    let sch = sched(spec::V2_REFERENCE, 0, 10_000, 119);
    assert_eq!(
        vested(at_period(tge, 118), tge, &sch),
        Ok(0),
        "inside the 119-month cliff nothing is unlocked"
    );
    assert_eq!(
        vested(at_period(tge, 119), tge, &sch),
        Ok(spec::V2_REFERENCE),
        "the first and only tranche lands at period 119 (day 3571)"
    );
}

/// A reference of 1 unit with `monthly_bps = 500`: `monthly_amt = floor(0.05) = 0`,
/// so the entire payout arrives in the terminal tranche.
#[test]
fn edge_reference_one_unit_pays_only_at_terminal() {
    let sch = sched(1, 0, 500, 0);
    assert_eq!(
        tranches_m(0, 500),
        Ok(u32::from(spec::EDGE_TINY_M)),
        "ceil(10000 / 500) = 20"
    );
    for p in 0..i64::from(spec::EDGE_TINY_M) {
        assert_eq!(
            vested(at_period(TGE, p), TGE, &sch),
            Ok(spec::EDGE_TINY_MONTHLY_AMT),
            "period {p}: monthly_amt = 0, there is nothing to release before the terminal tranche"
        );
    }
    assert_eq!(
        vested(at_day(TGE, spec::EDGE_TINY_TERMINAL_DAY), TGE, &sch),
        Ok(1),
        "the terminal period (day 601) releases the whole reference in one tranche"
    );
}

// ===== Pinning: cliff = 0 and cliff = 1 are indistinguishable =====

/// `s = max(cliff_months, 1)` makes cliffs of 0 and 1 the same schedule. A refactor
/// to `s = cliff + 1` would shift every cliff-less schedule by a month - this test
/// holds the property across the whole period grid.
#[test]
fn cliff_zero_and_one_are_indistinguishable() {
    let zero = ecosystem();
    let one = sched(
        spec::V2_REFERENCE,
        spec::ECOSYSTEM_TGE_BPS,
        spec::ECOSYSTEM_MONTHLY_BPS,
        1,
    );
    assert_eq!(
        vested(TGE - 1, TGE, &zero),
        vested(TGE - 1, TGE, &one),
        "before TGE both schedules yield 0"
    );
    for p in 0..=i64::from(spec::ECOSYSTEM_TRANCHES_M) + 2 {
        for offset in [0i64, 1, spec::MONTH / 2, spec::MONTH - 1] {
            let now = at_period(TGE, p) + offset;
            assert_eq!(
                vested(now, TGE, &zero),
                vested(now, TGE, &one),
                "period {p}, offset {offset}: cliff = 0 and cliff = 1 must be \
                 indistinguishable"
            );
        }
    }
    assert_eq!(
        vested(at_day(TGE, spec::ECOSYSTEM_FIRST_MONTHLY_DAY), TGE, &one),
        Ok(spec::V2_VESTED_DAY_31),
        "with cliff = 1 the first tranche lands on day 31, not day 61"
    );
}

// ===== cliff_months == 1 is rejected when monthly_bps > 0 (decision 4) =====

/// With `monthly_bps > 0`, `validate_schedule` rejects `cliff_months == 1` as
/// ambiguous input: through `.max(1)` `vested()` treats cliffs of 0 and 1
/// identically, so "a one-month cliff" is indistinguishable from "no cliff".
/// Cliffs of 0 and 2 on the same schedule must pass (the boundaries around the
/// rejected value).
#[test]
fn cliff_one_rejected_zero_and_two_accepted() {
    assert_eq!(
        validate_schedule(spec::ECOSYSTEM_TGE_BPS, spec::ECOSYSTEM_MONTHLY_BPS, 1),
        Err(VestingError::InvalidSchedule),
        "cliff_months == 1 with monthly_bps > 0 must be rejected as ambiguous (decision 4)"
    );
    assert!(
        validate_schedule(spec::ECOSYSTEM_TGE_BPS, spec::ECOSYSTEM_MONTHLY_BPS, 0).is_ok(),
        "cliff_months == 0 on the same schedule is a valid input"
    );
    assert!(
        validate_schedule(spec::ECOSYSTEM_TGE_BPS, spec::ECOSYSTEM_MONTHLY_BPS, 2).is_ok(),
        "cliff_months == 2 on the same schedule is a valid input"
    );
}

// ===== Properties over random inputs =====
//
// proptest runs each property over thousands of random schedules. The properties
// carry no reference values by construction - they check the invariants of §10.3
// (monotonicity, the ceiling, terminal equality), while the fixture tests hold the
// numeric tie to the spec.

/// Cases per invariant: the default is fast, `PROPTEST_CASES` raises the run.
/// We read the env var ourselves: an explicit `cases` field would override it.
fn proptest_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024)
}

/// A valid schedule with a reference amount from the given range. The `cliff + M`
/// ceiling is enforced by `validate_schedule()` itself: recomputing `M` inside the
/// strategy is not allowed - that would be a copy of the formula.
fn valid_schedule_with_reference(
    reference: RangeInclusive<u64>,
) -> impl Strategy<Value = Schedule> {
    (reference, 0u16..=9_999u16, 1u16..=10_000u16, 0u16..=100u16)
        .prop_filter(
            "the schedule fails the validation of §9.5 (the cliff + M ceiling)",
            |&(_, tge_bps, monthly_bps, cliff_months)| {
                validate_schedule(tge_bps, monthly_bps, cliff_months).is_ok()
            },
        )
        .prop_map(
            |(reference_amount, tge_bps, monthly_bps, cliff_months)| Schedule {
                reference_amount,
                tge_bps,
                monthly_bps,
                cliff_months,
            },
        )
}

/// A valid schedule with any positive reference amount.
fn valid_schedule() -> impl Strategy<Value = Schedule> {
    valid_schedule_with_reference(1..=u64::MAX)
}

/// A schedule's monthly-tranche count and the period of its first tranche. Taken
/// from the program itself: their numeric tie to the table of §10.5 is held by
/// , so this is not a tautology but a way to address the terminal period.
fn tranche_bounds(sch: &Schedule) -> (i64, i64) {
    let m = tranches_m(sch.tge_bps, sch.monthly_bps)
        .expect("a valid schedule has monthly_bps > 0, so M is defined");
    (i64::from(sch.cliff_months.max(1)), i64::from(m))
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: proptest_cases(),
        // prop_filter in the schedule generator: the reject budget scales with the
        // case count.
        max_local_rejects: proptest_cases().saturating_mul(64),
        max_global_rejects: proptest_cases().saturating_mul(64),
        ..ProptestConfig::default()
    })]

    /// The unlock never decreases over time (I11, §10.3): otherwise `claimable`
    /// would go negative and the `claimed` ceiling would become unprovable.
    #[test]
    fn vested_is_monotonic_in_now(sch in valid_schedule(), a in any::<i64>(), b in any::<i64>()) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let v_lo = vested(lo, TGE, &sch)
            .expect("a valid schedule: no errors before or after TGE (§10.4)");
        let v_hi = vested(hi, TGE, &sch)
            .expect("a valid schedule: no errors before or after TGE (§10.4)");
        prop_assert!(
            v_lo <= v_hi,
            "the unlock decreased: now {lo} → {v_lo}, now {hi} → {v_hi}, schedule {:?}",
            sch
        );
    }

    /// The unlock never exceeds the reference amount (§10.3): an activity vault
    /// can never be overdrawn.
    #[test]
    fn vested_never_exceeds_reference(sch in valid_schedule(), now in any::<i64>()) {
        let v = vested(now, TGE, &sch)
            .expect("a valid schedule: the formula is total across all of i64 (§10.4)");
        prop_assert!(
            v <= sch.reference_amount,
            "unlocked {v} > the reference {}, schedule {:?}",
            sch.reference_amount,
            sch
        );
    }

    /// Before TGE nothing is unlocked - the first branch of §10.2 cuts off any
    /// time value, i64::MIN included.
    #[test]
    fn vested_before_tge_is_zero(sch in valid_schedule(), now in i64::MIN..TGE) {
        prop_assert_eq!(
            vested(now, TGE, &sch),
            Ok(0),
            "before TGE it must be 0, not a share of schedule {:?}",
            sch
        );
    }

    /// At the terminal period and beyond - STRICT equality with the reference
    /// amount (§10.3): that is exactly what lets `close_vesting` gate on
    /// `claimed ≥ reference`.
    #[test]
    fn vested_at_terminal_period_equals_reference(
        sch in valid_schedule(),
        extra in 0i64..=1_000_000i64,
    ) {
        let (s, m) = tranche_bounds(&sch);
        let now = at_period(TGE, s + m - 1 + extra);
        prop_assert_eq!(
            vested(now, TGE, &sch),
            Ok(sch.reference_amount),
            "the terminal period (+{} months) did not release exactly the reference, schedule {:?}",
            extra,
            sch
        );
    }

    /// The increments over periods `0..=s+M` sum to the reference amount: the
    /// unlock loses nothing along the way and releases nothing in excess.
    #[test]
    fn increments_sum_to_reference(sch in valid_schedule()) {
        let (s, m) = tranche_bounds(&sch);
        let mut prev = 0u64;            // nothing is unlocked before TGE
        let mut sum = 0u128;
        for p in 0..=(s + m) {
            let v = vested(at_period(TGE, p), TGE, &sch)
                .expect("a valid schedule: no errors across the period grid");
            prop_assert!(v >= prev, "period {p}: the unlock decreased ({prev} → {v})");
            sum += u128::from(v - prev);
            prev = v;
        }
        prop_assert_eq!(
            sum,
            u128::from(sch.reference_amount),
            "the increments do not sum to the reference amount, schedule {:?}",
            sch
        );
    }

    /// The payout grows STRICTLY on every tranche. `monthly_amt > 0` is guaranteed
    /// by the generator: a reference ≥ 10⁷ implies `floor(ref × bps / 10000) ≥ 1000`
    /// for any `monthly_bps ≥ 1`. The terminal tranche is strictly larger by §10.3.
    #[test]
    fn vested_grows_strictly_on_every_tranche(
        sch in valid_schedule_with_reference(10_000_000..=u64::MAX),
    ) {
        let (s, m) = tranche_bounds(&sch);
        for p in s..=(s + m - 1) {
            let before = vested(at_period(TGE, p) - 1, TGE, &sch)
                .expect("a valid schedule: no errors across the period grid");
            let after = vested(at_period(TGE, p), TGE, &sch)
                .expect("a valid schedule: no errors across the period grid");
            prop_assert!(
                after > before,
                "period {p}: the tranche did not increase the payout ({before} → {after}), schedule {:?}",
                sch
            );
        }
    }

    /// Totality across the full domain: shares above 10000, a zero `monthly_bps`,
    /// the i64 edges of `now` and `tge_ts` - everything returns a Result and
    /// nothing panics (a panic in a program is an aborted transaction with no
    /// diagnostics).
    #[test]
    fn math_is_total_on_full_domain(
        reference_amount in any::<u64>(),
        tge_bps in any::<u16>(),
        monthly_bps in any::<u16>(),
        cliff_months in any::<u16>(),
        now in prop_oneof![Just(i64::MIN), Just(i64::MAX), Just(0i64), any::<i64>()],
        tge_ts in prop_oneof![Just(i64::MIN), Just(i64::MAX), Just(0i64), any::<i64>()],
    ) {
        let sch = Schedule { reference_amount, tge_bps, monthly_bps, cliff_months };
        let _ = tranches_m(tge_bps, monthly_bps);
        let _ = validate_schedule(tge_bps, monthly_bps, cliff_months);
        // The ceiling holds even for invalid shares: wherever a value was computed
        // at all, it does not exceed the reference amount.
        if let Ok(v) = vested(now, tge_ts, &sch) {
            prop_assert!(
                v <= reference_amount,
                "unlocked {v} > the reference {reference_amount}, schedule {:?}",
                sch
            );
        }
    }
}

// ===== The seed schemes do not overlap =====

/// The five PDA schemes (§6.1) do not overlap: each has its own concatenated seed
/// material and its own address. The load-bearing pair is `[b"vesting", id]`
/// (7 + 8 = 15 B) and `[b"vault_authority"]` (15 B): their lengths are equal, so
/// the argument "the schemes differ in length" does not apply to it - the
/// difference is in the bytes alone.
#[test]
fn pda_seed_materials_do_not_collide() {
    /// The concatenated seed material (the hash preimage) and the address derived
    /// from it.
    struct Pda {
        name: String,
        material: Vec<u8>,
        address: Pubkey,
    }

    let derive = |name: &str, seeds: &[&[u8]]| -> Pda {
        let (address, _bump) = Pubkey::find_program_address(seeds, &crate::ID);
        Pda {
            name: name.to_string(),
            material: seeds.concat(),
            address,
        }
    };

    // The addresses that depend on activity_id - used to compare across activities.
    let mut per_activity: Vec<Pda> = Vec::new();

    for id in [0u64, 1, 2, u64::MAX] {
        let id_le = id.to_le_bytes();
        let schemes = [
            derive("config", &[b"config"]),
            derive("vesting", &[b"vesting", &id_le]),
            derive("vault", &[b"vault", &id_le]),
            derive("vault_authority", &[b"vault_authority"]),
            derive("event_authority", &[b"__event_authority"]),
        ];

        for (i, a) in schemes.iter().enumerate() {
            for b in schemes.iter().skip(i + 1) {
                assert_ne!(
                    a.material, b.material,
                    "activity_id {id}: the concatenated seed material {} matched {} - the PDA \
                     schemes overlap (Arch §6.1)",
                    a.name, b.name
                );
                assert_ne!(
                    a.address, b.address,
                    "activity_id {id}: address {} matched {} - a PDA collision",
                    a.name, b.name
                );
            }
        }

        // The load-bearing case: equal length, differing only in the material's
        // bytes.
        let vesting_material = [b"vesting".as_slice(), id_le.as_slice()].concat();
        let authority_material = b"vault_authority".to_vec();
        assert_eq!(
            vesting_material.len(),
            authority_material.len(),
            "the pair [b\"vesting\", id] / [b\"vault_authority\"] must have equal length - \
             which is exactly why the \"they differ in length\" argument fails here"
        );
        assert_eq!(
            vesting_material.len(),
            15,
            "the length of the concatenated material [b\"vesting\", id] changed - the \
             argument no longer concerns this pair"
        );
        assert_ne!(
            vesting_material, authority_material,
            "activity_id {id}: the materials [b\"vesting\", id] and [b\"vault_authority\"] matched - \
             that would be a direct preimage collision"
        );

        per_activity.push(derive(&format!("vesting#{id}"), &[b"vesting", &id_le]));
        per_activity.push(derive(&format!("vault#{id}"), &[b"vault", &id_le]));
    }

    for (i, a) in per_activity.iter().enumerate() {
        for b in per_activity.iter().skip(i + 1) {
            assert_ne!(
                a.material, b.material,
                "the seed materials {} and {} matched - different activities would share one PDA",
                a.name, b.name
            );
            assert_ne!(
                a.address, b.address,
                "the addresses {} and {} matched - different activities would share one account",
                a.name, b.name
            );
        }
    }
}

// ===== Observable shares: the monthly tranche and the TGE instant =====
//
// The `*_MONTHLY_AMT` and `*_TGE_AMT` reference values are terms of the formula
// rather than its result, so they are invisible to the tests on their own. Below
// they are tied to observable quantities: the monthly share is the step between
// adjacent tranches, and the TGE share is the payout at the TGE instant. Without
// this, a typo in a term would reach the V3 tests unnoticed.

/// The step between adjacent tranches equals the monthly share - across every
/// §10.5 schedule with `monthly_bps > 0` and across the combined one. The first
/// and second tranches are used: both are non-terminal (`M ≥ 12` on every
/// schedule), so the last tranche's top-up (§10.3) does not enter the step.
#[test]
fn tranche_step_equals_monthly_amount() {
    let cases = [
        (
            "Ecosystem",
            ecosystem(),
            spec::ECOSYSTEM_FIRST_MONTHLY_DAY,
            spec::V2_MONTHLY_AMT,
        ),
        (
            "Treasury",
            treasury(),
            spec::TREASURY_FIRST_MONTHLY_DAY,
            spec::V3_MONTHLY_AMT,
        ),
        (
            "Liquidity",
            liquidity(),
            spec::LIQUIDITY_FIRST_MONTHLY_DAY,
            spec::LIQUIDITY_MONTHLY_AMT,
        ),
        (
            "Community Rewards",
            community(),
            spec::COMMUNITY_FIRST_MONTHLY_DAY,
            spec::V4_MONTHLY_AMT,
        ),
        // The combined schedule's cliff equals Treasury's (9 months), so its first
        // tranche arrives on the same day 271.
        ("cliff + TGE share", mixed(), 271, spec::MIXED_MONTHLY_AMT),
    ];

    for (name, sch, first_day, monthly_amt) in cases {
        let first = vested(at_day(TGE, first_day), TGE, &sch)
            .expect("a valid schedule: no errors across the period grid");
        let second = vested(at_day(TGE, first_day) + spec::MONTH, TGE, &sch)
            .expect("a valid schedule: no errors across the period grid");
        assert_eq!(
            second - first,
            monthly_amt,
            "{name}: the step between the first and second tranche must equal the monthly share"
        );
    }
}

/// At the TGE instant (`p = 0`) exactly the TGE share is released: no schedule has
/// a monthly tranche yet (`s ≥ 1`), so `vested(tge) == tge_amt`.
#[test]
fn vested_at_tge_moment_equals_tge_amount() {
    let cases = [
        ("Ecosystem", ecosystem(), spec::V2_TGE_AMT),
        ("Liquidity", liquidity(), spec::LIQUIDITY_TGE_AMT),
        ("cliff + TGE share", mixed(), spec::MIXED_TGE_AMT),
        ("Treasury", treasury(), spec::V3_TGE_AMT),
        ("Community Rewards", community(), spec::V4_TGE_AMT),
    ];

    for (name, sch, tge_amt) in cases {
        assert_eq!(
            vested(TGE, TGE, &sch),
            Ok(tge_amt),
            "{name}: at the TGE instant exactly the TGE share must be released"
        );
    }
}

// ===== The "stored state → math" adapter =====

/// [`Vesting::schedule`] is the only path from the account's fields to `vested()`:
/// everything the math reads in V3 goes through it. The fields are chosen to be
/// pairwise distinguishable (`4×10¹⁵` / `1000` / `500` / `9`), so swapping any two
/// shows up both in the field assertions and in the unlock; `claimed = 0` against a
/// non-zero reference catches a swap of the two `u64`s.
#[test]
fn vesting_schedule_maps_stored_fields() {
    let account = Vesting {
        recipient: Pubkey::new_unique(),
        reference_amount: spec::V3_REFERENCE,
        claimed: 0,
        tge_bps: spec::BRIDGE_TGE_BPS,
        monthly_bps: spec::BRIDGE_MONTHLY_BPS,
        cliff_months: spec::BRIDGE_CLIFF_MONTHS,
    };

    let sch = account.schedule();
    assert_eq!(
        sch.reference_amount,
        spec::V3_REFERENCE,
        "the adapter must take the reference amount from reference_amount, not from claimed"
    );
    assert_eq!(
        sch.tge_bps,
        spec::BRIDGE_TGE_BPS,
        "the adapter must take the TGE share from tge_bps"
    );
    assert_eq!(
        sch.monthly_bps,
        spec::BRIDGE_MONTHLY_BPS,
        "the adapter must take the monthly share from monthly_bps"
    );
    assert_eq!(
        sch.cliff_months,
        spec::BRIDGE_CLIFF_MONTHS,
        "the adapter must take the cliff from cliff_months"
    );

    // Day 301 is the second monthly tranche (p = 10, months = 2). At the first
    // tranche the sum tge_amt + monthly_amt is symmetric in the shares and would
    // not tell a tge_bps ↔ monthly_bps swap apart.
    assert_eq!(
        vested(at_day(TGE, 301), TGE, &sch),
        Ok(spec::BRIDGE_VESTED_DAY_301),
        "the unlock through the adapter diverged from the hand-written reference of 20% of 4×10¹⁵"
    );
}

// ===== DELIBERATELY NOT COVERED HERE =====
//
// - The same guards exercised through the real `create_vesting` instruction (a
//   zero reference, `label` length, the share coupling, the schedule ceiling) -
//   a pure function knows nothing about accounts or
//   phases.
// - `claimable = vested − claimed`, the `claimed ≤ reference_amount` ceiling and
//   the atomicity of the payout - iteration V3 (the on-chain effect of claim);
//   only the pure `vested()` is checked here.
// - The `MathOverflow` branches inside `tranches_m` and `validate_schedule`
//   (`rest + step`, `cliff + M` in u32) are unreachable given the input ranges:
//   the shares are u16, `M ≤ 10000`, and the sum is ≤ 75_535 ≪ u32::MAX. They are
//   defensive and cannot be expressed as a negative test - the totality of these
//   functions is held by `math_is_total_on_full_domain`.
