---
paths:
  - "**/*.rs"
---

# No test code in the production contract

## The core

The contract is built in exactly one way - the way that goes to mainnet - and is checked from the outside, through its real instruction interface (the way users call it), on a validator/SVM. There is no test code inside the program and no feature flags switching behaviour or dependencies: otherwise the build that was tested is not the build that ships - the tests exercise one branch while mainnet runs another, and the hole lives precisely in the one that was never compiled under test. Test doubles (a mock oracle, a mock DEX) are separate programs, substituted at the level of the test environment (through the accounts passed to the program), not compiled into it.

## The rules

- The contract's code contains no `#[cfg(feature = ...)]` that changes behaviour or swaps dependencies (mock vs real). The program is built one way - the production way.
- The program has no test branches: no "if test → …", no compiled-in mocks, no test hooks. Test and production code do not mix.
- The contract is checked from the outside: through the real instructions on a validator/SVM, with the same binary that ships to production. Behaviour is tested through the external interface, not through internal back doors.
- Test doubles are separate programs; in a test they are deployed into the validator, and the program under test receives their accounts as ordinary input and validates them like any account. It neither references the doubles nor knows about them.
- If an expected external address (an oracle program, a DEX program) has to be substituted in tests, it is stored in the config and set at init by data, not forked by a feature flag in the code.

Unit tests of pure functions under `#[cfg(test)]` are fine: that code never reaches the SBF binary. What is forbidden is precisely feature flags and test branches that change the production build.

## Examples

### A feature flag swapping a dependency

```rust
// BAD: a mock under test, the real source in production; these are different binaries
#[cfg(feature = "mock-oracle")]
let price = mock_oracle::read(&ctx.accounts.price_source)?;
#[cfg(not(feature = "mock-oracle"))]
let price = real_oracle::read(&ctx.accounts.price_source)?;
// the tests exercise the mock branch, mainnet runs the real one - which nobody built under test

// GOOD: one path; in a test the mock is substituted from outside, at the account/environment level
let price = read_price(&ctx.accounts.price_source)?;   // validated identically in any build
// the test/prod difference lies only in which account/program is supplied from outside, not in the code
```

A feature flag forks two builds: the one that was tested (mock) and the one that ships (real) - different code. A bug in the production branch slips past the tests because that branch was never compiled under test. One path plus a double substituted from outside removes the test/prod divergence at the root.

### A check under cfg - security you can switch off

```rust
// BAD: the source's owner check can be built out
#[cfg(feature = "real-oracle")]
require_keys_eq!(price_source.owner, ORACLE_PROGRAM, Error::BadOracle);
// built without "real-oracle" → the check is not in the binary and a substitute source gets through

// GOOD: the check is unconditional - it compiles in every build
#[account(address = config.price_source)]
pub price_source: Account<'info, PriceFeed>;     // the type always carries the owning-program check
```

A `#[cfg]` around a check makes security a function of the build config: one wrong feature in the release command and the check does not compile. A structural check carries it always, regardless of the build.
