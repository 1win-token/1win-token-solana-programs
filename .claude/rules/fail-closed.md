---
paths:
  - "**/*.rs"
---

# Fail-closed: security code reverts, it does not stay silent

## The core

Code responsible for security, compliance and validation must, when in doubt, **refuse** (revert) rather than quietly let something through, substitute a default or carry on with incomplete state. The construction "if the dependency is supplied, apply the check" silently switches the check off when the dependency is not supplied. A silent pass looks like success, which is why a missing check is noticed not at review time but after the fact, through its consequences. This is the on-chain analogue of the ban on a silent fallback.

## The rules

- A missing mandatory dependency / account / config is a `require!`/revert, not an `if present { … }` with no else.
- "Not set" is a separate sentinel (an explicit `Option::None` or a reserved value) and never merges with zero/the default. `gap == 0` and "gap is not configured" are different states.
- A check that applies to several participants or inputs covers **all** of them (sender, recipient, initiator), not just one.
- A guard on the executing path is mirrored in the corresponding view/quote: if `claim` rejects a blocked wallet, then the `claimable` view returns 0 for it rather than "as though it were allowed".
- Do not substitute "could not check" for "the check passed". No data to decide on ⇒ revert, not "let us assume it is fine".

## Examples

### A check conditional on a dependency being present

```rust
// BAD: no compliance config - and the calculation simply walks past the check
if let Some(registry) = &ctx.accounts.aml_registry {
    require!(!registry.is_blacklisted(buyer), SaleError::Blacklisted);
}
settle_purchase()?;                            // registry was not supplied → a purchase with no AML

// GOOD: the dependency is mandatory; its absence is a rejection
let registry = &ctx.accounts.aml_registry;     // a typed, mandatory account
require!(!registry.is_blacklisted(buyer), SaleError::Blacklisted);
settle_purchase()?;
```

`if let Some` turns a mandatory check into an optional one: whoever does not supply the registry account buys around AML. Compliance code has no right to become a no-op because a dependency was not supplied.

### Zero as "not configured"

```rust
// BAD: 0 means both "zero" and "not set"
let gap = if config.custom_gap != 0 { config.custom_gap } else { DEFAULT_GAP };
// A deliberately set gap = 0 is silently replaced by the default.

// GOOD: a sentinel separates "not set" from "zero"
let gap = config.custom_gap.unwrap_or(DEFAULT_GAP);   // Option<u64>: None != Some(0)
```

As long as "not set" is encoded as zero, a deliberately set zero is indistinguishable from an omission and is silently replaced by the default. An explicit `Option` (or a reserved sentinel) separates those states.

### Not every participant is checked

```rust
// BAD: the block is only looked up for the recipient
require!(!is_blacklisted(to), SaleError::Blacklisted);

// GOOD: every party to the transfer
require!(!is_blacklisted(from), SaleError::Blacklisted);
require!(!is_blacklisted(to), SaleError::Blacklisted);
require!(!is_blacklisted(authority), SaleError::Blacklisted);
```

Checking one participant leaves the path open for the rest: a blocked sender moves funds through if only the recipient is looked at.
