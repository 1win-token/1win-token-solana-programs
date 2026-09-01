use proptest::prelude::*;

use super::*;

/// Period boundaries: the day (86,400 s) and week (604,800 s) rollovers, zero
/// and i64::MAX. The expected values are computed by hand.
#[test]
fn period_boundaries() {
    let cases: [(i64, (u64, u64)); 6] = [
        (0, (0, 0)),
        (86_399, (0, 0)),
        (86_400, (1, 0)),
        (604_799, (6, 0)),
        (604_800, (7, 1)),
        (i64::MAX, (106_751_991_167_300, 15_250_284_452_471)),
    ];
    for (now, want) in cases {
        assert_eq!(period_indices(now).unwrap(), want, "now = {now}");
    }
}

/// Negative time is always an error (fail-closed), never a "zero period".
#[test]
fn negative_now_is_error() {
    for now in [-1_i64, -86_400, i64::MIN] {
        assert_eq!(
            period_indices(now),
            Err(BurnError::InvalidTimestamp),
            "now = {now}"
        );
    }
}

/// Same index - accumulate on top of what is already burned (base = burned).
#[test]
fn accumulate_same_index_adds_to_burned() {
    let window = PeriodWindow {
        index: 7,
        burned: 100,
    };
    assert_eq!(
        accumulate(&window, 7, 50, 1_000, BurnError::DailyLimitExceeded),
        Ok(150)
    );
}

/// A different index - lazy reset (base = 0): the previous period's total plays
/// no part; even a window sitting exactly at the limit does not stop the new
/// period from starting over at zero.
#[test]
fn accumulate_new_index_resets_base() {
    let window = PeriodWindow {
        index: 7,
        burned: 900,
    };
    assert_eq!(
        accumulate(&window, 8, 50, 1_000, BurnError::DailyLimitExceeded),
        Ok(50)
    );

    let full = PeriodWindow {
        index: 7,
        burned: 1_000,
    };
    assert_eq!(
        accumulate(&full, 8, 1_000, 1_000, BurnError::WeeklyLimitExceeded),
        Ok(1_000)
    );
}

/// Exactly at the limit - Ok; one unit above - the limit error that was passed
/// in (both of them).
#[test]
fn accumulate_limit_boundary_both_errors() {
    let window = PeriodWindow {
        index: 3,
        burned: 999,
    };
    assert_eq!(
        accumulate(&window, 3, 1, 1_000, BurnError::DailyLimitExceeded),
        Ok(1_000)
    );
    assert_eq!(
        accumulate(&window, 3, 2, 1_000, BurnError::DailyLimitExceeded),
        Err(BurnError::DailyLimitExceeded)
    );
    assert_eq!(
        accumulate(&window, 3, 2, 1_000, BurnError::WeeklyLimitExceeded),
        Err(BurnError::WeeklyLimitExceeded)
    );
}

/// An overflowing sum yields MathOverflow (a Result, not a panic).
#[test]
fn accumulate_overflow_is_math_overflow() {
    let window = PeriodWindow {
        index: 0,
        burned: u64::MAX - 1,
    };
    assert_eq!(
        accumulate(&window, 0, 2, u64::MAX, BurnError::DailyLimitExceeded),
        Err(BurnError::MathOverflow)
    );
}

/// limit = u64::MAX imposes no bound: the maximum amount passes in full.
#[test]
fn accumulate_max_limit_passes_max_amount() {
    let window = PeriodWindow {
        index: 0,
        burned: 0,
    };
    assert_eq!(
        accumulate(
            &window,
            0,
            u64::MAX,
            u64::MAX,
            BurnError::DailyLimitExceeded
        ),
        Ok(u64::MAX)
    );
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

    /// The day and week indices never decrease as time grows (monotonicity).
    #[test]
    fn indices_monotonic_in_now(a in 0i64..=i64::MAX, b in 0i64..=i64::MAX) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let (day_lo, week_lo) = period_indices(lo).unwrap();
        let (day_hi, week_hi) = period_indices(hi).unwrap();
        prop_assert!(day_lo <= day_hi);
        prop_assert!(week_lo <= week_hi);
    }

    /// A week is longer than a day, so its index never exceeds the day index.
    #[test]
    fn week_index_not_above_day_index(now in 0i64..=i64::MAX) {
        let (day_index, week_index) = period_indices(now).unwrap();
        prop_assert!(week_index <= day_index);
    }

    /// Any negative time is always InvalidTimestamp.
    #[test]
    fn negative_now_always_error(now in i64::MIN..0) {
        prop_assert_eq!(period_indices(now), Err(BurnError::InvalidTimestamp));
    }
}
