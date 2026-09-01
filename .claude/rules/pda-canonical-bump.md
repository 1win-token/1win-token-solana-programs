---
paths:
  - "**/*.rs"
---

# The canonical bump: a PDA's address is proven by recomputation, not by a cache

## The core

For the same seeds there exist several off-curve bumps, and each yields a valid PDA
address of its own; the canonical one is what `find_program_address` returns. A PDA check
proves the address's authenticity only if it forces the canonical bump. A valueless `bump`
in `#[account(seeds = …, bump)]` recomputes the canonical one and compares - a structural
guarantee. `bump = X.bump`, where the bump is taken from the account BEING CHECKED
(circular), verifies only the `key ↔ bump` consistency but NOT canonicity: it accepts any
cached bump. Canonicity then rests not on the check but on creation discipline (`init` must
be canonical) - a convention rather than a structure, and it breaks on the very first
non-canonical creation path.

## The rules

- A PDA check/derivation goes through a valueless `bump` (Anchor recomputes the canonical
  one through `find_program_address`), not through a circular `bump = X.bump` taken from the
  account being checked.
- A bump NEVER arrives as an instruction argument - only `ctx.bumps` (creation), a
  recomputation (a valueless `bump`) or an external canonical one.
- An external canonical bump is admissible: `bump = other.stored_bump`, where `other` is an
  account already verified structurally and the bump was fixed to the canonical value at ITS
  `init` (the typical case is the bump of a PDA authority, needed for a CPI signature). This
  is not circular: the bump comes from another account, not from the one being checked.
- A manual derivation for comparing an address is `find_program_address(seeds).0`, not
  `create_program_address(seeds, cached_bump)`.
- Where canonicity is structurally critical (an existence check for "is there a record of
  ours at this address"; the uniqueness of a per-wallet PDA), do not rely on a circular bump:
  a non-canonical record at the same seeds is a DIFFERENT address, which the canonical check
  either will not see (a bypass) or will duplicate.

## Examples

### A circular bump does not force canonicity

```rust
// BAD: the bump comes from the account being checked - the check does not force the canonical one
#[account(seeds = [b"aggregate", wallet.as_ref()], bump = aggregate.bump)]
pub aggregate: Account<'info, WalletAggregate>;
// create_program_address(seeds, aggregate.bump) only compares key ↔ bump. Should a
// non-canonical record appear at those seeds, it is a different address; uniqueness/existence
// checks that look at the canonical one do not see it → a duplicate or a bypass of the list.

// GOOD: a valueless bump - Anchor recomputes the canonical one (find_program_address)
#[account(seeds = [b"aggregate", wallet.as_ref()], bump)]
pub aggregate: Account<'info, WalletAggregate>;
```

A circular `bump = X.bump` is self-consistent but not self-sufficient: it is correct only
while EVERY creation path is canonical. Recomputation makes canonicity a property of the
check rather than of discipline.

### A bump from an argument - the caller chooses the address

```rust
// BAD: the caller dictates the bump → supplies a non-canonical off-curve bump
pub fn add(ctx: Context<Add>, wallet: Pubkey, bump: u8) -> Result<()> { … }
#[account(seeds = [b"rec", wallet.as_ref()], bump = bump)]

// GOOD: the canonical one from ctx.bumps (creation) / a recomputation (a check)
#[account(init, seeds = [b"rec", wallet.as_ref()], bump, …)]   // ctx.bumps.rec - the canonical one
```

A bump from an argument hands the choice of address to the caller: they create a "parallel"
record at a non-canonical bump that the canonical checks never see.

### An external canonical bump - admissible (not circular)

```rust
// GOOD: the bump of a PDA authority from an already verified account; needed for the CPI signature
#[account(seeds = [b"vault_authority"], bump = sale_state.vault_authority_bump)]
pub vault_authority: UncheckedAccount<'info>;
// sale_state is verified structurally; vault_authority_bump was fixed to the canonical value at its init.
// The only address that passes the constraint is the canonical one.
```

Tell this apart from the circular case: the bump comes from ANOTHER, already verified
account, where it was fixed to the canonical value at creation - not from the account being
checked.
