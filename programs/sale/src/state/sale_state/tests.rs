use super::*;

/// The error code taken from a validation result; success is `None`.
fn code(r: Result<()>) -> Option<u32> {
    match r {
        Ok(()) => None,
        Err(Error::AnchorError(e)) => Some(e.error_code_number),
        Err(other) => panic!("expected a program error, got: {other:?}"),
    }
}

fn err(e: SaleError) -> Option<u32> {
    Some(u32::from(e))
}

// ----- Price -----

/// Both ends of the allowed range: 0/1 at the bottom, TOKEN_UNITS−1/TOKEN_UNITS
/// at the top.
#[test]
fn price_boundaries() {
    let cases: [(u64, Option<u32>); 6] = [
        (0, err(SaleError::InvalidParams)),
        (1, None),
        (TOKEN_UNITS - 1, None),
        (TOKEN_UNITS, err(SaleError::InvalidParams)),
        (TOKEN_UNITS + 1, err(SaleError::InvalidParams)),
        (u64::MAX, err(SaleError::InvalidParams)),
    ];
    for (price, want) in cases {
        assert_eq!(
            code(SaleState::validate_price(price)),
            want,
            "price = {price}"
        );
    }
}

// ----- Emission -----

/// The only boundary is zero.
#[test]
fn emission_boundaries() {
    let cases: [(u64, Option<u32>); 3] = [
        (0, err(SaleError::InvalidParams)),
        (1, None),
        (u64::MAX, None),
    ];
    for (emission, want) in cases {
        assert_eq!(
            code(SaleState::validate_emission(emission)),
            want,
            "emission = {emission}"
        );
    }
}

// ----- Purchase limits -----

/// Zero as the minimum, and the crossing of the min == max equality.
#[test]
fn tx_limits_boundaries() {
    let cases: [(u64, u64, Option<u32>); 7] = [
        (0, 0, err(SaleError::InvalidParams)),
        (0, 100, err(SaleError::InvalidParams)),
        (1, 0, err(SaleError::InvalidParams)),
        (1, 1, None),
        (100, 99, err(SaleError::InvalidParams)),
        (100, 100, None),
        (100, 101, None),
    ];
    for (min, max, want) in cases {
        assert_eq!(
            code(SaleState::validate_tx_limits(min, max)),
            want,
            "min = {min}, max = {max}"
        );
    }
}

// ----- Sale window -----

const NOW: i64 = 1_000_000;

/// The start must lie in the future: equality with now and any past value are
/// rejected.
#[test]
fn sale_window_start_must_be_ahead_of_now() {
    let cases: [(i64, Option<u32>); 3] = [
        (NOW - 1, err(SaleError::InvalidDeadline)),
        (NOW, err(SaleError::InvalidDeadline)),
        (NOW + 1, None),
    ];
    for (start, want) in cases {
        assert_eq!(
            code(SaleState::validate_sale_window(NOW, start, start + 1_000)),
            want,
            "start = {start}"
        );
    }
}

/// sale_start_date against sale_end_date: equality and the reversed order are
/// both rejected.
#[test]
fn sale_window_order_boundaries() {
    let cases: [(i64, i64, Option<u32>); 3] = [
        (NOW + 1_000, NOW + 999, err(SaleError::InvalidDeadline)),
        (NOW + 1_000, NOW + 1_000, err(SaleError::InvalidDeadline)),
        (NOW + 1_000, NOW + 1_001, None),
    ];
    for (start, end, want) in cases {
        assert_eq!(
            code(SaleState::validate_sale_window(NOW, start, end)),
            want,
            "start = {start}, end = {end}"
        );
    }
}

/// The start-delay ceiling: exactly on it passes, one second beyond does not.
#[test]
fn sale_window_start_delay_ceiling() {
    let at = NOW + MAX_SALE_START_DELAY;
    let cases: [(i64, Option<u32>); 3] = [
        (at - 1, None),
        (at, None),
        (at + 1, err(SaleError::StartDateTooFar)),
    ];
    for (start, want) in cases {
        assert_eq!(
            code(SaleState::validate_sale_window(NOW, start, start + 1_000)),
            want,
            "start = {start}"
        );
    }
}

/// The window-length ceiling: exactly on it passes, one second beyond does not.
#[test]
fn sale_window_duration_ceiling() {
    let start = NOW + 1_000;
    let cases: [(i64, Option<u32>); 3] = [
        (MAX_SALE_DURATION - 1, None),
        (MAX_SALE_DURATION, None),
        (MAX_SALE_DURATION + 1, err(SaleError::SaleDurationTooLong)),
    ];
    for (duration, want) in cases {
        assert_eq!(
            code(SaleState::validate_sale_window(
                NOW,
                start,
                start + duration
            )),
            want,
            "duration = {duration}"
        );
    }
}

/// The i64 extremes: both arithmetic paths return an error rather than panic.
#[test]
fn sale_window_extremes_are_errors() {
    // now + MAX_SALE_START_DELAY overflows; the date order is still correct.
    assert_eq!(
        code(SaleState::validate_sale_window(
            i64::MAX - 1_000,
            i64::MAX - 500,
            i64::MAX - 100
        )),
        err(SaleError::MathOverflow)
    );
    // sale_end_date - sale_start_date overflows (the delay ceiling has already
    // been passed at that point).
    assert_eq!(
        code(SaleState::validate_sale_window(
            i64::MIN,
            i64::MIN + 1,
            i64::MAX
        )),
        err(SaleError::MathOverflow)
    );
}

/// The order of the checks inside the function: with two violations at once, the
/// date-order code is returned first, not the ceiling code.
#[test]
fn sale_window_order_checked_before_ceiling() {
    let far = NOW + MAX_SALE_START_DELAY + 1;
    assert_eq!(
        code(SaleState::validate_sale_window(NOW, far, far - 1)),
        err(SaleError::InvalidDeadline)
    );
}

// ----- Claim start -----

const END: i64 = 2_000_000;

/// The claim start against the sale end: equality is rejected.
#[test]
fn claim_start_date_order_boundaries() {
    let cases: [(i64, Option<u32>); 3] = [
        (END - 1, err(SaleError::ClaimStartInPast)),
        (END, err(SaleError::ClaimStartInPast)),
        (END + 1, None),
    ];
    for (claim_start_date, want) in cases {
        assert_eq!(
            code(SaleState::validate_claim_start(NOW, END, claim_start_date)),
            want,
            "claim_start_date = {claim_start_date}"
        );
    }
}

/// The claim-delay ceiling: exactly on it passes, one second beyond does not.
#[test]
fn claim_start_date_delay_ceiling() {
    let at = END + MAX_CLAIM_START_DELAY;
    let cases: [(i64, Option<u32>); 3] = [
        (at - 1, None),
        (at, None),
        (at + 1, err(SaleError::ClaimStartTooFar)),
    ];
    for (claim_start_date, want) in cases {
        assert_eq!(
            code(SaleState::validate_claim_start(NOW, END, claim_start_date)),
            want,
            "claim_start_date = {claim_start_date}"
        );
    }
}

/// At the sale's end itself the anchor is still the sale's end: the table matches
/// the one measured before the sale.
#[test]
fn claim_start_date_anchor_is_sale_end_until_it_passes() {
    let cases: [(i64, Option<u32>); 4] = [
        (END, err(SaleError::ClaimStartInPast)),
        (END + 1, None),
        (END + MAX_CLAIM_START_DELAY, None),
        (
            END + MAX_CLAIM_START_DELAY + 1,
            err(SaleError::ClaimStartTooFar),
        ),
    ];
    for (claim_start_date, want) in cases {
        assert_eq!(
            code(SaleState::validate_claim_start(END, END, claim_start_date)),
            want,
            "claim_start_date = {claim_start_date}"
        );
    }
}

/// Once the sale has ended the anchor is `now`: a start behind `now` is rejected even
/// though it is after the sale's end, and the ceiling moves with `now`.
#[test]
fn claim_start_date_anchor_follows_now_after_sale_end() {
    let now = END + 10_000;
    let cases: [(i64, Option<u32>); 6] = [
        (END + 1, err(SaleError::ClaimStartInPast)),
        (now, err(SaleError::ClaimStartInPast)),
        (now + 1, None),
        (END + MAX_CLAIM_START_DELAY + 1, None),
        (now + MAX_CLAIM_START_DELAY, None),
        (
            now + MAX_CLAIM_START_DELAY + 1,
            err(SaleError::ClaimStartTooFar),
        ),
    ];
    for (claim_start_date, want) in cases {
        assert_eq!(
            code(SaleState::validate_claim_start(now, END, claim_start_date)),
            want,
            "claim_start_date = {claim_start_date}"
        );
    }
}

/// The i64 extremes: the ceiling addition returns an error rather than panics,
/// whichever operand is the anchor.
#[test]
fn claim_start_date_extremes_are_errors() {
    // The sale's end is the anchor and overflows on the ceiling addition.
    assert_eq!(
        code(SaleState::validate_claim_start(NOW, i64::MAX - 1, i64::MAX)),
        err(SaleError::MathOverflow)
    );
    // `now` is the anchor and overflows the same way.
    assert_eq!(
        code(SaleState::validate_claim_start(i64::MAX - 1, END, i64::MAX)),
        err(SaleError::MathOverflow)
    );
}

// ----- Claim window length -----

/// Both ends: MIN_CLAIM_DURATION−1/MIN_CLAIM_DURATION at the bottom (zero, one
/// second and every negative value fall below the floor), MAX_CLAIM_DURATION/+1
/// at the top. Each side has its own code.
#[test]
fn claim_duration_boundaries() {
    let cases: [(i64, Option<u32>); 10] = [
        (i64::MIN, err(SaleError::ClaimDurationTooShort)),
        (-1, err(SaleError::ClaimDurationTooShort)),
        (0, err(SaleError::ClaimDurationTooShort)),
        (1, err(SaleError::ClaimDurationTooShort)),
        (
            MIN_CLAIM_DURATION - 1,
            err(SaleError::ClaimDurationTooShort),
        ),
        (MIN_CLAIM_DURATION, None),
        (MIN_CLAIM_DURATION + 1, None),
        (MAX_CLAIM_DURATION - 1, None),
        (MAX_CLAIM_DURATION, None),
        (MAX_CLAIM_DURATION + 1, err(SaleError::ClaimDurationTooLong)),
    ];
    for (duration, want) in cases {
        assert_eq!(
            code(SaleState::validate_claim_duration(duration)),
            want,
            "duration = {duration}"
        );
    }
}
