//! Pure sale math: token allocation, USDC refunds and the soft cap.

use crate::errors::SaleError;
use crate::state::TOKEN_DECIMALS;

/// Token units in one whole $1WIN (= 10^decimals). Derived from
/// [`TOKEN_DECIMALS`]: the decimals are stated in exactly one place.
pub(crate) const TOKEN_UNITS: u64 = 10u64.pow(TOKEN_DECIMALS as u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Allocation {
    pub tokens: u64,
    pub refund: u64,
}

/// Whether more than the soft cap was collected. The comparison is strict:
/// landing exactly on the cap is not yet oversubscription - there are enough
/// tokens for everyone and nothing to refund.
pub(crate) fn is_oversubscribed(
    pool: u64,
    emission: u64,
    price_usdc: u64,
    token_units: u64,
) -> Result<bool, SaleError> {
    let lhs = u128::from(pool)
        .checked_mul(u128::from(token_units))
        .ok_or(SaleError::MathOverflow)?;
    let rhs = u128::from(emission)
        .checked_mul(u128::from(price_usdc))
        .ok_or(SaleError::MathOverflow)?;
    Ok(lhs > rhs)
}

/// a × b / denom, rounded down. The multiplication happens in u128 so it cannot
/// overflow, and the value is narrowed back to u64 only after the division.
/// A zero divisor yields an error, not a panic.
fn mul_div_floor(a: u64, b: u64, denom: u64) -> Result<u64, SaleError> {
    let wide = u128::from(a)
        .checked_mul(u128::from(b))
        .ok_or(SaleError::MathOverflow)?
        .checked_div(u128::from(denom))
        .ok_or(SaleError::MathOverflow)?;
    u64::try_from(wide).map_err(|_| SaleError::MathOverflow)
}

/// How many tokens a buyer receives for `usdc` at the given price.
/// Rounded down.
fn tokens_for(usdc: u64, price_usdc: u64, token_units: u64) -> Result<u64, SaleError> {
    mul_div_floor(usdc, token_units, price_usdc)
}

/// A wallet's share of the final pool (used by claim).
///
/// Undersubscribed: tokens = net × token_units / price, no refund.
/// Oversubscribed:  tokens = net × emission / pool           (down),
///                  spent  = tokens × price / token_units    (up),
///                  refund = net − spent.
/// Rounding always favours the protocol.
pub(crate) fn allocate(
    net: u64,
    pool: u64,
    emission: u64,
    price_usdc: u64,
    token_units: u64,
) -> Result<Allocation, SaleError> {
    if !is_oversubscribed(pool, emission, price_usdc, token_units)? {
        return Ok(Allocation {
            tokens: tokens_for(net, price_usdc, token_units)?,
            refund: 0,
        });
    }

    let tokens = mul_div_floor(net, emission, pool)?;
    // Adding (token_units - 1) before the division rounds the spent amount up:
    // understating it is not allowed, or the refund would grow at the
    // protocol's expense.
    let ceil_bias = u128::from(token_units)
        .checked_sub(1)
        .ok_or(SaleError::MathOverflow)?;

    let spent_wide = u128::from(tokens)
        .checked_mul(u128::from(price_usdc))
        .ok_or(SaleError::MathOverflow)?
        .checked_add(ceil_bias)
        .ok_or(SaleError::MathOverflow)?
        .checked_div(u128::from(token_units))
        .ok_or(SaleError::MathOverflow)?;

    let spent = u64::try_from(spent_wide).map_err(|_| SaleError::MathOverflow)?;
    let refund = net.saturating_sub(spent);

    Ok(Allocation { tokens, refund })
}

/// How many tokens are handed out in total:
/// oversubscribed - the whole emission; undersubscribed - total × token_units / price.
pub(crate) fn distributable(
    oversubscribed: bool,
    total: u64,
    emission: u64,
    price_usdc: u64,
    token_units: u64,
) -> Result<u64, SaleError> {
    if oversubscribed {
        Ok(emission)
    } else {
        tokens_for(total, price_usdc, token_units)
    }
}

/// The soft cap in USDC: emission × price / token_units.
/// Rounding the cap down guarantees the money covers every refund.
pub(crate) fn soft_cap_usdc(
    emission: u64,
    price_usdc: u64,
    token_units: u64,
) -> Result<u64, SaleError> {
    mul_div_floor(emission, price_usdc, token_units)
}

#[cfg(test)]
mod tests;
