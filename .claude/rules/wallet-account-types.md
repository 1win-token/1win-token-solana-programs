---
paths:
  - "**/*.rs"
---

# Wallet account types: Signer, SystemAccount, on-curve / off-curve

## The core

A "wallet" in an instruction is an account, and its type decides WHO may be it. Three facts
that are easy to confuse into a hole:

- `Signer` checks only `is_signer`, NOT on-curve. A PDA that signed through `invoke_signed`
  (which is how a multisig executes a CPI on behalf of its vault PDA) passes as a `Signer`.
  So a `Signer` wallet accepts both ordinary keys and program wallets.
- `SystemAccount` requires `owner == System Program`. That rejects program-owned accounts
  (multisigs/custody) but does NOT prove "a real user": an uncreated off-curve PDA is also
  System-owned (there is no account → the owner defaults to System) and can be a `Signer`
  through `invoke_signed`. So `owner == System` on its own is not enough.
- Real user wallets are on-curve (an Ed25519 point on the curve). A PDA is guaranteed
  off-curve (by construction). The only thing that proves "a genuine private key stands
  behind this account" is an on-curve check together with a signature.

Hence: a wallet account's type is chosen deliberately, to answer "do we support program
wallets", and the same type is kept throughout the lifecycle. Inconsistency (`Signer` at the
entry, `SystemAccount` in the lists/refund) yields "they can buy, but you cannot ban or
refund them".

## The rules

- Decide IN ADVANCE: are program wallets (multisigs/custody/smart wallets) supported? The
  answer is applied uniformly across ALL lifecycle instructions (entry, lists/AML, refund,
  payout), not case by case.
- `Signer` = "we accept any account that signed, program ones included (through
  `invoke_signed`)". If that is not what you want, `Signer` is insufficient and an extra
  check is needed.
- If an on-curve user key is specifically required (to exclude PDAs/program accounts), use an
  explicit on-curve check. `owner == System` on its own is NOT enough (an uncreated off-curve
  PDA is System-owned too).
- When `wallet` is needed only as an address (the source of seeds), use a `Pubkey` argument
  rather than an account: every kind of wallet is covered and a superfluous account leaves the
  context.

## Examples

### owner == System does not prove "a real wallet"

```rust
// BAD: "users only" through owner == System
require_keys_eq!(*user.owner, system_program::ID, Error::NotUser);
// An uncreated off-curve PDA is also owned by System: a program derives it, funds it with
// lamports, does NOT create it (the owner stays System) and signs through invoke_signed →
// the check passes, yet this is not a wallet.

// GOOD: an explicit on-curve check (a PDA is guaranteed off-curve)
fn is_on_curve(key: &Pubkey) -> bool {
    curve25519_dalek::edwards::CompressedEdwardsY::from_slice(key.as_ref())
        .map(|p| p.decompress().is_some())
        .unwrap_or(false)
}
require!(is_on_curve(&ctx.accounts.user.key()), Error::NotOnCurveWallet);
// on-curve + Signer ⟹ a genuine Ed25519 signature by a key ⟹ a user wallet.
```

The native equivalent with no external dependency is the `sol_curve_validate_point` syscall;
keep the `curve25519-dalek` version in step with the Solana toolchain.

### Signer accepts a PDA - cutting off CPIs when needed

```rust
// Signer checks only is_signer. A PDA that signed through invoke_signed (a multisig execute)
// passes as a Signer. To cut off ANY CPI caller (a blunter tool than on-curve):
use anchor_lang::solana_program::instruction::{get_stack_height, TRANSACTION_LEVEL_STACK_HEIGHT};
require!(get_stack_height() == TRANSACTION_LEVEL_STACK_HEIGHT, Error::CpiNotAllowed);
// The downside: it also cuts off legitimate CPI integrations (an aggregator buying on behalf
// of an on-curve wallet). on-curve is more precise: it lets wallets through and rejects PDAs
// without depending on whether a CPI is involved.
```

### One wallet type across the whole lifecycle

```rust
// BAD: the entry accepts a PDA while the lists/refund do not
// purchase:  pub user:   Signer<'info>          // a PDA passes (invoke_signed)
// blacklist: pub wallet: SystemAccount<'info>   // a PDA is rejected → it cannot be banned
// refund:    pub wallet: SystemAccount<'info>   // a PDA is rejected → it cannot be refunded
// The result: a multisig buys and claims, but cannot be banned or refunded.

// GOOD: one policy across the whole circuit.
// (a) supported: the lists take a Pubkey argument, refund takes an UncheckedAccount (bound by
//     has_one), and purchase/claim stay as they are;
// (b) not supported: an on-curve gate at the purchase entry → a PDA takes part nowhere.
```

The choice between (a) and (b) is a product decision, but it is ONE decision for the whole
lifecycle.
