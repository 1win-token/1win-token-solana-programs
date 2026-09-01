---
paths:
  - "**/*.rs"
---

# Explicit errors, no panic on fallible paths

## The core

Every invariant gets a named error variant of its own: the error code on chain must show what exactly was violated, with no excavation required. A single catch-all error for different failures collapses the diagnostics - "something went wrong" reads the same for an overflow, an insufficient balance and a wrong phase. On fallible paths there are no `unwrap`/`expect`/indexing operations that panic: a panic in a Solana program is an aborted transaction with an opaque code, whereas a `Result` carries the concrete reason.

## The rules

- A separate variant in `#[error_code]` per class of violation: `CalculationOverflow`, `InsufficientBalance`, `WrongPhase`, `Unauthorized`, `Blacklisted` and so on. Do not reuse one generic variant for everything.
- A failed `checked_*` → `.ok_or(Error::CalculationOverflow)?`. A balance comparison → a balance-specific error. Do not disguise math and logic behind a shared code.
- No `.unwrap()` / `.expect()` / `arr[i]` on data that depends on input or on the runtime. `Option` → `ok_or`, `Result` → `?`, indexing → `.get(i).ok_or(...)?`.
- A variant's message describes the invariant that was violated, not a place in the code.

## Examples

### A generic error for different failures

```rust
// BAD: one code for everything - the reason is indistinguishable on chain
require!(pool > 0, SaleError::InvalidState);
let t = net.checked_mul(EMISSION).ok_or(SaleError::InvalidState)?;
require_keys_eq!(signer, config.owner, SaleError::InvalidState);
// an overflow, an empty pool and an unauthorized call are indistinguishable in the logs

// GOOD: a variant per invariant
require!(pool > 0, SaleError::EmptyPool);
let t = net.checked_mul(EMISSION).ok_or(SaleError::CalculationOverflow)?;
require_keys_eq!(signer, config.owner, SaleError::Unauthorized);
```

One `InvalidState` for every case turns on-chain diagnostics into guesswork: a failed transaction cannot be classified by its code and has to be reproduced locally. Separate variants make the reason visible at once.

### unwrap on runtime data

```rust
// BAD: a panic on an empty Option / a short slice
let pending = config.pending_owner.unwrap();        // None → an aborted tx with no reason
let first = tiers[0];                               // an empty Vec → a panic

// GOOD: an explicit error
let pending = config.pending_owner.ok_or(SaleError::NoPendingOwner)?;
let first = tiers.first().ok_or(SaleError::NoTiers)?;
```

`unwrap` / indexing on data that depends on the accounts' state panics on legal empty values. The transaction fails with an opaque panic instead of a domain error.
