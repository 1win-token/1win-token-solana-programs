---
paths:
  - "**/*.rs"
  - "**/*.ts"
---

# Tests bind to the real code, not to a copy of its logic

## The core

A test exists in order to fail when the production code breaks. If a test checks not the result of calling the real instruction but its own copy of that instruction's logic, it is bound to the copy rather than to the code: the contract's formula may change or break while the test stays green, because both sides of the equality are computed inside the test itself. Every test calls the real handler and asserts its actual on-chain effects (account state, token-account balances, emitted events). The expected value is an independently computed constant or reference, not a recomputation of the contract's formula alongside it.

## The rules

- A test calls the real instruction (through the program/RPC); it does not extract or duplicate its body.
- The expected result is a fixed quantity derived separately (by hand from the conditions), not an expression repeating the formula from the contract.
- The assertions target observable effects: `token_account.amount`, changes to counters in the global state, the fact that an account was closed, an event's payload. Not intermediate numbers invisible on chain.
- Every guard gets both a positive path (a valid call goes through and has its effect) and a negative one (an invalid call reverts with a specific error), in the same configuration.
- No conditional wording like "should pass / would pass" instead of an executed transaction. A path that was not executed in the test is not covered.

## Examples

### A test that duplicates the contract's formula

```rust
// BAD: the test computes the allocation with the same formula as the contract
let expected = net.checked_mul(EMISSION).unwrap() / pool;   // a copy of the claim logic
let got = compute_allocation(net, pool);                    // the same function the contract calls
assert_eq!(got, expected);
// Both sides are one formula, so the equality is tautological. Change the rounding
// in compute_allocation and the test is still green.

// GOOD: the reference is computed independently, the assertion targets the real effect of claim
// e.g.: pool = 10_000_000 USDC, contribution = 10_000, emission = 130_000_000 → a 0.1% share
program.claim(wallet).await?;                               // the real instruction
assert_eq!(buyer_token_account.amount, 130_000 * 10u64.pow(8));   // a constant expectation
assert_eq!(buyer_usdc_delta, 6_100 * 10u64.pow(6));
assert!(contribution_pda_is_closed(wallet).await);
```

The expectation is a number known in advance, derived by hand from the conditions. Then any divergence between the real effect and the constant catches any drift in the production formula. A tautological "the formula == the same formula" catches nothing.

### Only the negative branch is checked

```rust
// BAD: the test reverts a purchase from the Blacklist but never checks that a clean wallet buys
let r = program.purchase(blacklisted, 100).await;
assert!(r.is_err());
// "the positive path obviously works" - but it was not executed. If purchase
// breaks for everyone (not just for the blacklisted), the test will not notice.

// GOOD: both branches in one test
let ok = program.purchase(clean_wallet, 100).await;
assert!(ok.is_ok());
assert_eq!(deposit_pda(clean_wallet, 0).await.amount, 100);     // the positive path was executed
let denied = program.purchase(blacklisted, 100).await;
assert_err!(denied, SaleError::WalletBlacklisted);              // the negative one, with a specific error
```

The positive branch must actually be executed and asserted - otherwise "the guard works" is only half checked, and a regression in the common path goes unnoticed.
