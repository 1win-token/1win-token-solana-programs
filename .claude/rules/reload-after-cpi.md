---
paths:
  - "**/*.rs"
---

# An account's data is stale after a CPI

## The core

An `Account<T>` inside an instruction is a snapshot read at its start. A CPI that changed the account does not refresh that snapshot by itself: the `amount`/`balance` field after the CPI still shows the pre-call value until `reload()` is called. A decision taken on such a value is built on stale data. Checks and calculations are therefore made on values read before the CPI, and if a value is needed after the call, the account is re-read explicitly.

## The rules

- A value from an account that a CPI could have changed is not used after the CPI without a `reload()`. Anything cached before the call is stale.
- Checks and calculations run on pre-CPI values; the CPI itself comes last.
- If the logic genuinely needs the value after the call (the amount actually transferred, say), call `account.reload()?` and read it again rather than reconstructing it arithmetically from the pre-call snapshot.

## Examples

### A decision on a value read before the CPI

```rust
// BAD: balance is read before the CPI and used after - the CPI changed it
let balance = ctx.accounts.vault.amount;            // a pre-call snapshot
withdraw_via_cpi(&ctx, amount)?;                    // the CPI reduced the vault
require!(balance >= needed, Error::Insufficient);   // a check against a stale balance

// GOOD: the check comes before the CPI, on a fresh value
require!(ctx.accounts.vault.amount >= needed, Error::Insufficient);
withdraw_via_cpi(&ctx, amount)?;
```

`Account<T>` is not re-read automatically after a CPI: `vault.amount` stays at its pre-call value until `reload()` is called. A check against it misses the change the CPI made.

### When the actual value after the call is needed

```rust
// BAD: assuming the balance after the transfer is exactly before + amount
let after = before + amount;                        // an assumption, not a fact

// GOOD: re-read and take the actual value
ctx.accounts.vault.reload()?;
let after = ctx.accounts.vault.amount;              // the actual value after the CPI
```

A CPI's result is taken from the re-read account rather than reconstructed arithmetically from the pre-call snapshot: fees, rounding and partial execution may diverge from the expectation.
