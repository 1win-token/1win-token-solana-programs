---
paths:
  - "**/*.rs"
---

# Checked arithmetic and rounding

## The core

Any arithmetic over on-chain quantities or values scaled by decimals is done through `checked_*` (or the `?` operators), not with bare `+ - * /`. A `u64` overflow arrives sooner than it seems: the product of amounts with 8–9 decimal places leaves `u64` at perfectly ordinary market values, and a single `panic` on overflow turns into a DoS - the claim/purchase instruction stops executing. Intermediate products are computed in `u128`, and the downcast to `u64` goes through `try_from` and only **after** the divisions. Division by zero is ruled out by an explicit check. Rounding is always in the protocol's favour.

## The rules

- No `+ - * /` on on-chain quantities without a check: `checked_add/sub/mul/div` + `ok_or(Error::Overflow)?`, or compute in `u128`.
- Scaled products (an amount × the token multiplier, a share × the emission) are computed in `u128`; the downcast to `u64` goes through `u64::try_from(…)?` **after** the divisions, once the value has already shrunk.
- Every divisor is checked against zero before the division (`require!(divisor != 0, …)`), or `checked_div` is used with `None` handled.
- The rounding direction is explicit and in the protocol's favour: when handing out or refunding to a user, round down; when computing their obligation, round up. Never hand out more than is due.
- Move the math into pure functions (no `Context`, no accounts) - they are unit-tested on boundary values separately from the on-chain wiring.

## Examples

### A u64 product overflows at market numbers

```rust
// BAD: net and EMISSION are u64 - the product leaves u64
let tokens = net * EMISSION / pool;            // a panic on overflow → the claim cannot execute
// net ~ 10^13 (USDC), EMISSION ~ 1.3*10^16 (8 decimals) → the product >> u64::MAX

// GOOD: compute in u128, downcast after the division
let tokens_u128 = (net as u128)
    .checked_mul(EMISSION as u128).ok_or(SaleError::CalculationOverflow)?
    .checked_div(pool as u128).ok_or(SaleError::CalculationOverflow)?;
let tokens = u64::try_from(tokens_u128).map_err(|_| SaleError::CalculationOverflow)?;
```

In `u64` the product of two scaled amounts overflows long before any exotic values. A `panic` on overflow (in release with `overflow-checks`) makes the instruction unexecutable - a DoS on the main path. `u128` with the downcast after the division keeps the intermediate in a safe range.

### A division with no divisor guard

```rust
// BAD: pool may become 0 (every deposit refunded) → a panic
let share = net * EMISSION / pool;

// GOOD: a zero divisor is a handled error, not a panic
require!(pool > 0, SaleError::EmptyPool);
let share = (net as u128).checked_mul(EMISSION as u128).ok_or(SaleError::CalculationOverflow)?
    .checked_div(pool as u128).ok_or(SaleError::CalculationOverflow)?;
```

A divisor zeroed by a legal scenario (a full refund of every contribution) brings the instruction down with a panic instead of a diagnosable error.

### Rounding on a payout

```rust
// BAD: rounding up on a payout - the protocol gives out more than it owes
let payout = (owed + price - 1) / price;       // ceil in the user's favour

// GOOD: when handing out, round down
let payout = owed.checked_div(price).ok_or(SaleError::CalculationOverflow)?;  // floor
```

Systematically rounding payouts up accumulates into a vault shortfall: across many claims the protocol hands out more than the total emission. Payouts round down, a user's obligation rounds up.
