use proptest::prelude::*;

use super::*;

const START: i64 = 1_000;
const END: i64 = 2_000;
const CLAIM_START: i64 = 3_000;
const CLAIM_DEADLINE: i64 = 4_000;

fn before_fixation() -> PhaseSchedule {
    PhaseSchedule {
        sale_start_date: START,
        sale_end_date: END,
        pool_fixed: false,
        claim_start_date: CLAIM_START,
        claim_duration: CLAIM_DEADLINE - CLAIM_START,
    }
}

fn after_fixation() -> PhaseSchedule {
    PhaseSchedule {
        sale_start_date: START,
        sale_end_date: END,
        pool_fixed: true,
        claim_start_date: CLAIM_START,
        claim_duration: CLAIM_DEADLINE - CLAIM_START,
    }
}

/// Before fixation: PreSale → Sale → SaleEnded, every boundary at t−1 / t / t+1.
#[test]
fn phase_boundaries_before_fixation() {
    let cases: [(i64, Phase); 8] = [
        (i64::MIN, Phase::PreSale),
        (START - 1, Phase::PreSale),
        (START, Phase::Sale),
        (START + 1, Phase::Sale),
        (END - 1, Phase::Sale),
        (END, Phase::SaleEnded),
        (END + 1, Phase::SaleEnded),
        (i64::MAX, Phase::SaleEnded),
    ];
    for (now, want) in cases {
        assert_eq!(current_phase(now, &before_fixation()), want, "now = {now}");
    }
}

/// After fixation: SaleEnded → Claim → ClaimClosed, every boundary at
/// t−1 / t / t+1.
#[test]
fn phase_boundaries_after_fixation() {
    let cases: [(i64, Phase); 8] = [
        (i64::MIN, Phase::SaleEnded),
        (CLAIM_START - 1, Phase::SaleEnded),
        (CLAIM_START, Phase::Claim),
        (CLAIM_START + 1, Phase::Claim),
        (CLAIM_DEADLINE - 1, Phase::Claim),
        (CLAIM_DEADLINE, Phase::ClaimClosed),
        (CLAIM_DEADLINE + 1, Phase::ClaimClosed),
        (i64::MAX, Phase::ClaimClosed),
    ];
    for (now, want) in cases {
        assert_eq!(current_phase(now, &after_fixation()), want, "now = {now}");
    }
}

/// Cases per invariant: the default is fast, PROPTEST_CASES raises it (M2: 10^5).
/// We read the env var ourselves: `with_cases` would override the env value.
fn proptest_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: proptest_cases(),
        ..ProptestConfig::default()
    })]

    /// The only gate on Claim is pool_fixed (§4.2). claim_start_date and
    /// claim_duration hold real values set by initialize, but while
    /// pool_fixed == false the phase must depend ONLY on sale_start_date and
    /// sale_end_date, for ANY value of the claim fields. Sweeping arbitrary
    /// claim fields proves the gate holds against realistically-valued inputs -
    /// a stronger property than "unset ≠ 0".
    #[test]
    fn claim_fields_ignored_before_fixation(
        now in any::<i64>(),
        any_claim_start_date in any::<i64>(),
        any_claim_duration in any::<i64>(),
    ) {
        let with_arbitrary_claim = PhaseSchedule {
            claim_start_date: any_claim_start_date,
            claim_duration: any_claim_duration,
            ..before_fixation()
        };
        prop_assert_eq!(
            current_phase(now, &with_arbitrary_claim),
            current_phase(now, &before_fixation())
        );
    }
}
