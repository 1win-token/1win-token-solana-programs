---
paths:
  - "**/*.rs"
---

# The state machine and time

## The core

If a program goes through phases (collection → settlement → payout → close, epochs, rounds), then the phase is an invariant that every instruction must check before doing anything. An operation valid in one phase is a hole in another: a contribution after collection ends, a payout before it opens, a repeated finalization. The transitions between phases are explicit, monotonic and one-way; they never roll back. Time on Solana comes from the on-chain `Clock`; deadlines move only in the permitted direction.

## The rules

- Every instruction whose right depends on a phase begins with `require!(state.phase == Expected, …)`. There is no "the phase is implied".
- A phase transition is a separate instruction and is one-way: from `Active` you may go to `Ended`, never back. A double finalization is rejected (`require!(!state.finalized)` or a check of the current phase).
- Irreversible actions (fixing the pool, minting the emission, opening the payout) are protected by both the phase and a "done" flag, so that they cannot execute twice.
- Time is `Clock::get()?.unix_timestamp`, not a parameter from the caller. A deadline is extended only forward (`require!(new_deadline > current)`); moving it back is forbidden where the business rule is "forward only".
- The time gate and the phase agree: "the time has run out" does not by itself change the phase - the transition still goes through an explicit instruction (which the operator triggers on a timer), but the operations check against the actual phase rather than "it has presumably expired".

## Examples

### An instruction that does not check the phase

```rust
// BAD: purchase does not look at whether collection is still running
pub fn purchase(ctx: Context<Purchase>, amount: u64) -> Result<()> {
    ctx.accounts.state.pool += amount;         // a contribution is taken after Ended and during Claim alike
    Ok(())
}

// GOOD: the right to purchase is explicitly bound to the phase and the time
pub fn purchase(ctx: Context<Purchase>, amount: u64) -> Result<()> {
    require!(ctx.accounts.state.phase == Phase::Active, SaleError::NotActive);
    let now = Clock::get()?.unix_timestamp;
    require!(now < ctx.accounts.state.active_until, SaleError::SaleEnded);
    // ... effects
    Ok(())
}
```

With no phase check the instruction executes at any point of the lifecycle. A contribution taken after the pool is fixed will not enter the allocation calculation - the money is in the vault and there is no share.

### An irreversible action with no guard - a double execution

```rust
// BAD: the payout can be opened twice, reinitializing the deadline
pub fn open_claim(ctx: Context<OpenClaim>, deadline: i64) -> Result<()> {
    ctx.accounts.state.phase = Phase::Claim;
    ctx.accounts.state.claim_deadline = deadline;   // a second call overwrites the deadline
    Ok(())
}

// GOOD: the transition happens only from Ended, one-way
pub fn open_claim(ctx: Context<OpenClaim>, deadline: i64) -> Result<()> {
    require!(ctx.accounts.state.phase == Phase::Ended, SaleError::WrongPhase);
    require!(deadline > Clock::get()?.unix_timestamp, SaleError::DeadlineInPast);
    ctx.accounts.state.phase = Phase::Claim;
    ctx.accounts.state.claim_deadline = deadline;
    Ok(())
}
```

A transition that does not check the source phase executes again and overwrites parameters already fixed (the deadline, the references). Binding it to a specific source phase makes the transition one-shot by construction.
