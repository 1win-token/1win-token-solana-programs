---
paths:
  - "**/*.rs"
---

# init vs re-initialization

## The core

Creating an account and re-entering an already created one are different operations with different security. `init_if_needed` erases that boundary: one instruction both creates a singleton and silently reinitializes an existing one, zeroing the state accumulated (claimed counters, balances, flags). Singleton and config accounts are created exactly once through `init`; a repeat attempt must fail. Who has the right to run the init of a privileged account is a separate question: "any signer" means the first transaction sender becomes the owner (an init front-run).

## The rules

- A singleton/config PDA uses `init`, not `init_if_needed`. `init_if_needed` is only for genuinely cumulative per-user accounts, and then the instruction's body does **not** overwrite the fields already accumulated.
- Belt-and-suspenders: with `init`, keep a semantic guard on "already initialized" if a flag/version is readable. Anchor rejects a real double init at the account level; the guard insures the logic against a refactor.
- The right to init is bound to an on-chain fact rather than to "any signer": for management accounts, compare the `signer` with `program_data.upgrade_authority_address` (or another authoritative source).
- Reject at init: zero / `Pubkey::default()` authorities, zero critical parameters (decimals, limits, shares). A zero admin means an account nobody will ever be able to manage.

## Examples

### init_if_needed on a singleton reinitializes state

```rust
// BAD: a claim goes through "creating" its own position again
#[account(init_if_needed, payer = user, seeds = [b"position", user.key().as_ref()], bump, space = ...)]
pub position: Account<'info, Position>;
// The second call takes the same branch, position.claimed is zeroed →
// the user claims again. "if_needed" disguised a re-initialization.

// GOOD: creation is separate and one-off; the claim works with an existing account
#[account(mut, seeds = [b"position", user.key().as_ref()], bump)]
pub position: Account<'info, Position>;
// the init of the position itself lives in a separate instruction with #[account(init, ...)]
```

`init_if_needed` creates a false convenience of "create it if it isn't there", but on a second pass it runs the initialization body over a live account. The accumulated counters are reset - with no error and no trace.

### An init with no binding to an authority - a front-run

```rust
// BAD: the owner will be whoever submits the transaction first
pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
    ctx.accounts.config.owner = ctx.accounts.signer.key();
    Ok(())
}

// GOOD: only the program's upgrade authority can run init
#[account(constraint = program_data.upgrade_authority_address == Some(signer.key()))]
pub program_data: Account<'info, ProgramData>;
pub signer: Signer<'info>;
// in the body: config.owner = signer.key();
```

The deploy and the init are different transactions; in the window between them anyone can call an open `initialize` and make themselves the owner. Binding to the `upgrade_authority` closes the window structurally rather than by "we'll manage to call first".

### A zero authority at init

```rust
// BAD: the admin is taken as is
config.admin = admin;                          // admin may be Pubkey::default()

// GOOD: a zero/default authority is rejected
require_keys_neq!(admin, Pubkey::default(), SaleError::InvalidAuthority);
config.admin = admin;
```

A zero admin will never be able to sign a single management instruction - the account is "initialized" but unmanageable forever.
