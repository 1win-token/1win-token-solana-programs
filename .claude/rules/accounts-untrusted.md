---
paths:
  - "**/*.rs"
---

# Accounts are untrusted input until authenticity is proven structurally

## The core

On Solana the caller supplies every account to an instruction themselves. An address passed under the name `vault`, `mint` or `authority` is just 32 bytes from an untrusted party; the program must prove that it is that very account and not a substitute with the same data shape. The proof must be **structural**: an Anchor type (`Account<'info, T>` checks the owning program and the discriminator), `seeds`+`bump` (the program derives the address rather than taking it on trust), `address = …`, `has_one`. Until an account is bound that way, any `require!` over its fields is checking a substitute account's data.

This is the largest vulnerability class by volume: confused deputy, vault substitution, a counterfeit oracle, "a bare Pubkey dereferenced later".

## The rules

- Typed `Account<'info, T>` / `InterfaceAccount` by default. `AccountInfo` / `UncheckedAccount` only where the type is genuinely not needed, and then a constraint is **mandatory**: `owner = …`, `address = …` or `seeds`+`bump`.
- A token account is bound to a canonical PDA through `seeds` / `has_one` / `token::authority`+`token::mint`, **not by the mint alone**. "The same mint and the same authority" is not uniqueness: an external actor can create a second such account.
- If an address arrives both as an account field and as an instruction parameter, compare them (`require_keys_eq!`) or do not duplicate it (take it only from the typed account).
- An address stored in a config as a `Pubkey` is revalidated on **every** write path (both `init` and `update`), and when used it is supplied as a typed account rather than as a "bare" key dereferenced later.
- A canonical `bump`: `ctx.bumps` / recomputation (a valueless `bump`) / an externally verified account - NOT a circular `bump = X.bump` and not from the arguments (see `pda-canonical-bump.md`).
- If two mutable positions must logically be different accounts (a transfer's source and destination, and so on), check their inequality explicitly (`require_keys_neq!`). Anchor does not itself forbid supplying the same account under both roles.
- `remaining_accounts` are entirely untrusted: each one is checked explicitly before use.

## Examples

### A vault bound only by its mint

```rust
// BAD: "the same mint" does not make it our vault
#[account(mut, constraint = vault.mint == config.token_mint)]
pub vault: Account<'info, TokenAccount>;
// The attacker supplies their own token account with the same mint → the CPI transfer
// under the PDA signature debits/drains something other than the canonical vault.
// A drain through a side vault.

// GOOD: the program derives the vault's address from the seeds
#[account(
    mut,
    seeds = [b"vault", config.key().as_ref()],
    bump = config.vault_bump,
)]
pub vault: Account<'info, TokenAccount>;
```

A `constraint` on the mint checks a property that is easy to reproduce. `seeds`+`bump` makes the address underivable from anything but the canonical one - a foreign account cannot be substituted by construction.

### A bare Pubkey in the config, dereferenced later

```rust
// BAD: on init the key is taken as a parameter, on use as a bare AccountInfo
pub fn set_oracle(ctx: Context<SetOracle>, oracle: Pubkey) -> Result<()> {
    ctx.accounts.config.oracle = oracle;       // nothing validates that this is an oracle at all
    Ok(())
}
// ... in another instruction ...
#[account(address = config.oracle)]
pub oracle: UncheckedAccount<'info>;           // the owning program is not checked
let price = read_price(&oracle.data.borrow()); // we parse a substitute account's data

// GOOD: type it - Anchor checks the owning program
#[account(address = config.oracle)]
pub oracle: Account<'info, PriceFeed>;         // owner == the expected program + the discriminator
```

`UncheckedAccount` + `address` guarantees only the address, not that the account belongs to the right program. A counterfeit account with arbitrary data gets through. A typed account moves the owner check into the validation layer.

### The account versus the parameter

```rust
// BAD: the wallet parameter and the wallet account diverge
pub fn blacklist(ctx: Context<Blacklist>, wallet: Pubkey) -> Result<()> {
    ctx.accounts.record.wallet = wallet;       // a record about one wallet,
    // while the record's seeds were derived from a different key → the PDA does not match its contents
    Ok(())
}

// GOOD: one source of truth - derive the record from the same key
#[account(init, seeds = [b"record", wallet.key().as_ref()], bump, payer = payer, space = ...)]
pub record: Account<'info, WalletRecord>;
pub wallet: SystemAccount<'info>;              // work with wallet.key(), not with the parameter
```

A "parameter vs account" mismatch creates a record whose PDA does not match its contents: the checks by PDA and by field look at different wallets.

### The same account under two roles

```rust
// BAD: source and destination are two mutable positions, but the caller supplied one account
from.balance = from.balance.checked_sub(amount).ok_or(Error::Overflow)?;   // debited
to.balance   = to.balance.checked_add(amount).ok_or(Error::Overflow)?;     // credited
// with from == to Anchor holds two copies of one account; on write the "to" copy
// (the credit) overwrites the "from" copy (the debit) → the balance grew by amount out of nowhere

// GOOD: explicitly require that they are different accounts
require_keys_neq!(from.key(), to.key(), Error::SameAccount);
from.balance = from.balance.checked_sub(amount).ok_or(Error::Overflow)?;
to.balance   = to.balance.checked_add(amount).ok_or(Error::Overflow)?;
```

Anchor loads the two named accounts independently and writes each one back at the end of the instruction. If they are the same address, the last write overwrites the first: the debit is lost and the credit remains. A program that needs the two positions to be distinct must check that itself.
