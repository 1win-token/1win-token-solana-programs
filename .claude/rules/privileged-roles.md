---
paths:
  - "**/*.rs"
---

# Privileged operations and the lifecycle of roles

## The core

Every instruction that changes parameters, moves funds or manages access must structurally check the caller against an authoritative role (owner / admin / officer / operator). On Solana it is convenient to keep a role as the presence of a PDA: the PDA exists ⇒ the right is granted. Revoking the right is then closing the PDA, not an `enabled: bool` flag inside a live account (two truths about the same thing are waiting to diverge). Changing the owner is two-step (propose → accept), so that a typo in a key does not hand control off into the void.

## The rules

- A privileged instruction checks the signer against the role structurally (`has_one = owner`, the seeds of an admin PDA, `address`). There are no "open" state mutators.
- A role as the presence of a PDA: granting = `init`, revoking = `close`. Do not model a revoke with an `enabled`/`disabled` flag in an account that either exists or does not.
- A revoke is implemented for real (closing the PDA and returning the rent), not as a one-way `disable` with no inverse operation.
- Changing the owner is two-step: `propose_owner(new)` stores the pending value and `accept_owner` is executed by the new key. A direct `set_owner` in a single transaction is forbidden.
- Rotating the owner does **not** touch the delegates automatically: the new owner revokes them explicitly. Auto-invalidation on rotation is a hidden side effect that breaks expectations.
- Every privileged operation emits an event (who, what, when) for off-chain auditing.

## Examples

### Revoking a right with a flag instead of a close

```rust
// BAD: the right lives as two truths - the account's existence AND a flag
#[account(mut, seeds = [b"admin", admin.key().as_ref()], bump)]
pub admin_pda: Account<'info, AdminPda>;
// ... revoke:
ctx.accounts.admin_pda.enabled = false;        // the account is alive but "switched off"
// Any check that forgets to read .enabled considers the admin active.

// GOOD: the PDA's presence = the right; the revoke = the close
#[account(mut, close = owner, seeds = [b"admin", admin.key().as_ref()], bump)]
pub admin_pda: Account<'info, AdminPda>;
// the right is checked in other instructions by the very presence of admin_pda at those seeds
```

An `enabled` flag duplicates what the account's existence already expresses. Any check that forgets to consult the flag sees an "active" admin who was believed revoked. Closing the PDA leaves exactly one truth.

### A one-step owner change

```rust
// BAD: a typo in the key - control is lost forever
pub fn set_owner(ctx: Context<SetOwner>, new_owner: Pubkey) -> Result<()> {
    ctx.accounts.config.owner = new_owner;     // nobody confirms ownership of new_owner
    Ok(())
}

// GOOD: two-step - the new key must confirm ownership
pub fn propose_owner(ctx: Context<ProposeOwner>, new_owner: Pubkey) -> Result<()> {
    ctx.accounts.config.pending_owner = Some(new_owner);
    Ok(())
}
pub fn accept_owner(ctx: Context<AcceptOwner>) -> Result<()> {
    let pending = ctx.accounts.config.pending_owner.ok_or(SaleError::NoPendingOwner)?;
    require_keys_eq!(pending, ctx.accounts.signer.key(), SaleError::Unauthorized);
    ctx.accounts.config.owner = ctx.accounts.signer.key();
    ctx.accounts.config.pending_owner = None;
    Ok(())
}
```

A one-step handover gives control to a key nobody checked for ownership: a typo sends the owner to an address with no private key. The two-step scheme requires the new owner to actually sign the accept.
