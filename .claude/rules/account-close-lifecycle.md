---
paths:
  - "**/*.rs"
---

# Closing an account is the end of its lifecycle

## The core

Closing an account (returning the rent, zeroing the data) is the last state in its life, not an intermediate step. Therefore, on a close: its quantity is subtracted from every counter it was added to (otherwise the counter stays overstated after the account itself is gone); the close does not happen while the account carries an unresolved obligation (after the close it can no longer be resolved - there is no record); and the close serves as a one-shot guard against a repeat (an operation that closes its own account will not execute a second time - a repeat call finds no account and fails). The rent recipient is a predetermined, verified account rather than an arbitrary one from untrusted input (otherwise the caller redirects it to themselves).

## The rules

- On a close, the account's quantity is subtracted from every counter it was previously added to - in the same instruction as the close itself. Closing a record without reducing the counter by its quantity leaves the counter overstated forever.
- Do not close an account with an unresolved obligation (an unfinished operation, a deferred action, a locked portion). The close comes after the settlement, not instead of it.
- The close is used as a replay guard: a one-shot operation closes its own account, and a repeat call fails on its absence rather than on a separate "already done" flag.
- The rent recipient (the lamports of the closed account) is an account predetermined by design and verified (the original payer, or the administrator/protocol for internal accounts), not an arbitrary one from untrusted input: otherwise the caller sets `close = my_account` and takes the rent for themselves.
- A batch close performs the same subtraction from the counters as a single one; the list of accounts to close is untrusted input and each one is checked.

## Examples

### A close with no subtraction from the counter

```rust
// BAD: the account is closed but the counter its quantity entered is not reduced
#[account(mut, close = recipient)]
pub item: Account<'info, Item>;
// the body: item was closed, and state.total still includes item.value
// → the counter is overstated forever: there is no record, and its quantity is still in the sum

// GOOD: subtract the quantity from the counter in the same instruction
ctx.accounts.state.total = ctx.accounts.state.total
    .checked_sub(ctx.accounts.item.value).ok_or(Error::Overflow)?;
// and only then does close = recipient return the rent
```

A close that did not reduce the counter by the closed account's quantity leaves it overstated forever: the record is gone and its quantity is still being summed. Everything computed from that counter is skewed.

### A close with an unresolved obligation

```rust
// BAD: the account is closed while an unresolved portion hangs on it
#[account(mut, close = owner)]
pub item: Account<'info, Item>;
// if item.locked > 0, a later unlock/refund will not find the account - the obligation is lost

// GOOD: close only after the obligations are settled
require!(ctx.accounts.item.locked == 0, Error::PendingObligation);
// and only now the close
```

An account closed with an obligation still hanging makes resolving it impossible: the operation that was to unlock or refund a portion addresses a record that no longer exists.
