//! Pure unlock math (Arch §10): the number of monthly tranches, schedule
//! validation, and the accumulated `vested()`. No accounts are read.

use crate::errors::VestingError;
use crate::state::{Schedule, MAX_SCHEDULE_MONTHS};

pub(crate) const MONTH: i64 = 2_592_000;

pub(crate) const BPS_DENOM: u16 = 10_000;

/// The number of monthly tranches `M = ceil((10000 − tge_bps) / monthly_bps)`.
/// `(rest + step − 1) / step` is the integer idiom for ceiling division: the
/// remainder gets its own tranche instead of being dropped.
pub(crate) fn tranches_m(tge_bps: u16, monthly_bps: u16) -> Result<u32, VestingError> {
    if monthly_bps == 0 {
        return Err(VestingError::InvalidSchedule);
    }
    let rest = u32::from(
        BPS_DENOM
            .checked_sub(tge_bps)
            .ok_or(VestingError::InvalidBps)?,
    );
    let step = u32::from(monthly_bps);

    let m = rest
        .checked_add(step)
        .ok_or(VestingError::MathOverflow)?
        .checked_sub(1)
        .ok_or(VestingError::MathOverflow)?
        .checked_div(step)
        .ok_or(VestingError::MathOverflow)?;

    // `M == 0` means `rest == 0` while `step > 0` - a schedule that declares
    // monthly tranches with nothing left to hand out. No valid input produces
    // that, hence a rejection rather than `Ok(0)`: otherwise the terminal branch
    // of `vested()` would release the full reference amount on a forged record.
    if m == 0 {
        return Err(VestingError::InvalidSchedule);
    }
    Ok(m)
}

pub(crate) fn validate_schedule(
    tge_bps: u16,
    monthly_bps: u16,
    cliff_months: u16,
) -> Result<(), VestingError> {
    if tge_bps > BPS_DENOM || monthly_bps > BPS_DENOM {
        return Err(VestingError::InvalidBps);
    }

    if monthly_bps == 0 {
        if tge_bps != BPS_DENOM || cliff_months != 0 {
            return Err(VestingError::InvalidSchedule);
        }
        return Ok(());
    }

    // With 100% at TGE there is nothing left for the monthly tranches: the
    // schedule would sum above the reference amount.
    if tge_bps == BPS_DENOM {
        return Err(VestingError::InvalidSchedule);
    }

    // A one-month cliff is rejected as ambiguous input: `vested()` computes it
    // through `s = max(cliff_months, 1)` identically to a zero cliff and would
    // silently ignore it.
    if cliff_months == 1 {
        return Err(VestingError::InvalidSchedule);
    }

    // `M` is needed here only to check the schedule ceiling; it is not returned.
    let m = tranches_m(tge_bps, monthly_bps)?;
    let total = u32::from(cliff_months)
        .checked_add(m)
        .ok_or(VestingError::MathOverflow)?;
    if total > MAX_SCHEDULE_MONTHS {
        return Err(VestingError::InvalidSchedule);
    }

    Ok(())
}

/// The `bps` share of an amount. Rounded down, in the protocol's favour: handing
/// out less than due is safe, and what was dropped is picked up by the terminal
/// branch of `vested()`.
fn bps_amount(amount: u64, bps: u16) -> Result<u64, VestingError> {
    let wide = u128::from(amount)
        .checked_mul(u128::from(bps))
        .ok_or(VestingError::MathOverflow)?
        .checked_div(u128::from(BPS_DENOM))
        .ok_or(VestingError::MathOverflow)?;
    u64::try_from(wide).map_err(|_| VestingError::MathOverflow)
}

/// The accumulated unlock at time `now` (Arch §10.2). Public on purpose:
/// off-chain code computes `claimable` from this very implementation rather than
/// from its own copy of the formula.
pub fn vested(now: i64, tge_ts: i64, schedule: &Schedule) -> Result<u64, VestingError> {
    if now < tge_ts {
        return Ok(0);
    }
    if schedule.monthly_bps == 0 {
        return Ok(schedule.reference_amount);
    }

    let p = now
        .checked_sub(tge_ts)
        .ok_or(VestingError::MathOverflow)?
        .checked_div(MONTH)
        .ok_or(VestingError::MathOverflow)?;
    // Cliffs of 0 and 1 are identical: the first tranche still lands in period 1.
    // That is why `validate_schedule` rejects `cliff_months == 1` as input.
    let s = i64::from(schedule.cliff_months.max(1));
    let m = tranches_m(schedule.tge_bps, schedule.monthly_bps)?;

    let months = if p < s {
        0
    } else {
        let accrued = p
            .checked_sub(s)
            .ok_or(VestingError::MathOverflow)?
            .checked_add(1)
            .ok_or(VestingError::MathOverflow)?;
        u64::try_from(accrued)
            .map_err(|_| VestingError::MathOverflow)?
            .min(u64::from(m))
    };

    // The terminal branch returns the whole reference amount rather than the sum
    // of the tranches: this picks up the remainder dropped by flooring each
    // tranche's share.
    if months == u64::from(m) {
        return Ok(schedule.reference_amount);
    }

    let tge_amt = bps_amount(schedule.reference_amount, schedule.tge_bps)?;
    let monthly_amt = bps_amount(schedule.reference_amount, schedule.monthly_bps)?;
    let total = u128::from(months)
        .checked_mul(u128::from(monthly_amt))
        .ok_or(VestingError::MathOverflow)?
        .checked_add(u128::from(tge_amt))
        .ok_or(VestingError::MathOverflow)?;
    u64::try_from(total).map_err(|_| VestingError::MathOverflow)
}

#[cfg(test)]
mod tests;
