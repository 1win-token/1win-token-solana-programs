---
paths:
  - "**/*.rs"
---

# No overengineering

## The core

The simplest construction that satisfies the requirement is the right one. The contract holds other people's money: every superfluous instruction, account, parameter and layer of indirection is attack surface and a line an auditor will have to read. An abstraction with no second consumer right now is not needed. If a value can be fixed at init from an on-chain fact, fix it rather than making it configurable "for the future".

## What to do

- A direct instruction instead of a generic dispatcher when there is only one variant.
- A value derivable from an on-chain fact (a mint address, an owning program, a bump) is bound at init rather than taken as a parameter and "locked" afterwards.
- Fewer accounts in the context, fewer CPIs, fewer cross-program references. Coupling between programs means synchronizing state between them, i.e. a vulnerability class of its own; avoid it until it is genuinely needed.
- Pass parameters as instruction arguments rather than stashing them in a shared config "to fetch later".

## What NOT to do

- Do not introduce roles/levels/registries that have one caller and one scenario.
- Do not make a parameter `settable` where the value is known and immutable at init.
- Do not add "extensibility" (plugin addresses, version branches, generic wrappers) without a second real consumer.
- Do not write a backward-compat layer for code that has not been deployed yet: before mainnet there are no off-chain consumers and breaking changes are free.

## Examples

### Settable-then-locked instead of fixing at init

```rust
// BAD: the mint address can be set later and "locked" with a flag
pub fn set_mint(ctx: Context<SetMint>, mint: Pubkey) -> Result<()> {
    require!(!ctx.accounts.config.mint_locked, SaleError::Locked);
    ctx.accounts.config.token_mint = mint;
    ctx.accounts.config.mint_locked = true;     // a superfluous flag, a superfluous window for a mistake
    Ok(())
}

// GOOD: the mint is bound at init as a typed account
#[account(init, ...)]
pub config: Account<'info, Config>;
pub token_mint: Account<'info, Mint>;           // config.token_mint = token_mint.key() straight away
```

`settable-then-locked` adds state ("is it locked"), a window between the deploy and the lock, and a superfluous instruction. Binding the mint at init from a typed account is simpler and leaves no window.

### A dispatcher for a single variant

```rust
// BAD: an "extensible" choice of price source with a single source
let price = match config.oracle_kind {
    OracleKind::Pyth => read_pyth(...)?,
    // there are no other arms
};

// GOOD: a direct call
let price = read_pyth(...)?;
```

A `match` with one arm depicts several variants where there is one: it adds an enum, a config field and a branch that chooses nothing. When a second source appears, that is when the fork is introduced.
