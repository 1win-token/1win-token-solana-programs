//! Pure UTC-period helpers: day and week indices derived from unix time, and
//! accumulation of the burned total inside a period window with a lazy reset
//! (§6.3).

use crate::errors::BurnError;
use crate::state::PeriodWindow;

pub(crate) const SECONDS_PER_DAY: i64 = 86_400;
pub(crate) const SECONDS_PER_WEEK: i64 = 604_800;

/// The (UTC day, UTC week) indices of `now`: `now / 86400` and `now / 604800`
/// (§6.3). Negative time is an error (fail-closed), not a "pre-epoch period".
pub(crate) fn period_indices(now: i64) -> Result<(u64, u64), BurnError> {
    if now < 0 {
        return Err(BurnError::InvalidTimestamp);
    }
    let day_index = u64::try_from(now / SECONDS_PER_DAY).map_err(|_| BurnError::MathOverflow)?;
    let week_index = u64::try_from(now / SECONDS_PER_WEEK).map_err(|_| BurnError::MathOverflow)?;
    Ok((day_index, week_index))
}

/// Accumulates a burn inside a period window, resetting lazily when the index
/// changes: a window holding a foreign index counts as empty (base = 0).
/// Returns the new total for period `cur_index`; the window itself is NOT
/// mutated. Clock guarantees time is monotonic (`cur_index` never decreases),
/// so a "foreign" index is always a past one. Exceeding `limit` yields
/// `limit_err` (DailyLimitExceeded / WeeklyLimitExceeded for its period kind).
pub(crate) fn accumulate(
    window: &PeriodWindow,
    cur_index: u64,
    amount: u64,
    limit: u64,
    limit_err: BurnError,
) -> Result<u64, BurnError> {
    let base = if window.index == cur_index {
        window.burned
    } else {
        0
    };
    let total = base.checked_add(amount).ok_or(BurnError::MathOverflow)?;
    if total > limit {
        return Err(limit_err);
    }
    Ok(total)
}

#[cfg(test)]
mod tests;
