---
paths:
  - "**/*.rs"
---

# Bookkeeping by the program's own state, not by an observed balance

## The core

The program takes its decisions about quantities (how much is accounted for, how much is due, whether there are enough funds) from its own state fields, which it maintains itself - not from a token account's observed balance (`token_account.amount`). A token account's balance is how many tokens sit in it right now; it does not equal what the program considers its own: tokens can be transferred to the address directly, bypassing the program's logic, and rounding remainders settle there. The bookkeeping is kept symmetrically: every counter increment has a paired decrement in the cancellation scenario. A divergence between "the observed balance" and "the accounted quantity" is either foreign funds or a bookkeeping bug, and in both cases it is handled explicitly rather than pulled into the calculation.

## The rules

- Quantities and thresholds are computed from state fields that the program increments/decrements itself, not from `token_account.amount` or any other observed quantity.
- Every increment of an accounting counter has a paired decrement: the operation that added the quantity and the operation that removed it keep the counter consistent.
- The observed balance is not used as an indirect source of truth for "how much we have": stray inflows to the address must not change the calculations.
- If a calculation is nonetheless reconciled against the balance, the divergence is accounted for explicitly (separately from "our own" funds) rather than silently drawn into the logic.

## Examples

### A decision by a token account's balance instead of a counter

```rust
// BAD: "how much is accounted for" is read from the token account's balance
let total = ctx.accounts.token_account.amount;     // includes direct transfers to the address
let share = part.checked_mul(SCALE).ok_or(Error::Overflow)?
    .checked_div(total).ok_or(Error::Overflow)?;   // the denominator is skewed by foreign funds

// GOOD: the source of truth is the counter the program maintains itself
let total = ctx.accounts.state.tracked_total;      // changed only by the program's logic
let share = (part as u128).checked_mul(SCALE as u128).ok_or(Error::Overflow)?
    .checked_div(total as u128).ok_or(Error::Overflow)?;
```

`token_account.amount` is not "how much the program considers its own" but "how much sits in the account", including any direct transfers and settled rounding. A calculation off the observed balance is skewed by a transfer to the address that the program's logic never saw.

### An increment with no paired decrement

```rust
// BAD: the quantity is added to the counter but never removed in the cancellation scenario
state.tracked_total = state.tracked_total.checked_add(amount).ok_or(Error::Overflow)?;
// ... cancelling that quantity removes the tokens but does not reduce tracked_total
// → the counter is overstated forever and every derived calculation is skewed

// GOOD: a paired decrement for every increment in the cancellation
// when adding:
state.tracked_total = state.tracked_total.checked_add(amount).ok_or(Error::Overflow)?;
// when cancelling:
state.tracked_total = state.tracked_total.checked_sub(amount).ok_or(Error::Overflow)?;
```

A counter that only grows falls out of step with reality on the very first cancellation: the accounted quantity exceeds the actual one, and everything computed from it is skewed.
