---
paths:
  - "**/*.rs"
---

# On-chain is expensive: store what the logic needs, nothing more

## The core

Every byte in an account is paid for with rent and lives permanently; every instruction spends compute units and a fee. So there is one criterion: **what is stored and computed on chain is what at least one instruction reads - for a decision or for an invariant.** Data that no instruction reads and that only a landing page, analytics or a human needs belongs in events, not in accounts. The test is whether the on-chain logic uses it, not the label "history" or "display": records on which the program genuinely takes decisions (per-transaction records for a per-transaction refund, say) are necessary state rather than surplus, and they stay on chain.

## The rules

- Store what at least one instruction reads (for a decision or an invariant). If no instruction reads it and it is needed only for display/analytics/audit, it is an event, not a field.
- "It looks like history" is not a criterion. Records the logic needs stay on chain. But if such records grow one per element, that means **one PDA per element** (closed once it has served its purpose), not a `Vec` that runs into rent, the size limit and reallocation.
- A primary counter the program maintains itself (`total`) is stored: it is the source of truth and the instructions decide on it. Do not store only what is mechanically derivable from other stored fields of the same account (`average = sum / count`) - that is rent plus a risk of divergence.
- An account's size matches its actual contents; no "for the future" slack and no unused fields.
- An instruction does bounded, predictable work: no loops over growing data that will one day not fit the compute budget. Cross-element summaries are built by the indexer from the events.

## Examples

### Display-only data goes into an event

```rust
// BAD: a field in the account that no instruction reads
#[account]
pub struct Record {
    pub wallet: Pubkey,
    pub amount: u64,
    pub note: String,        // a human-readable note for the UI - the logic never uses it
}

// GOOD: the field the logic needs; the display goes into an event
#[account]
pub struct Record {
    pub wallet: Pubkey,
    pub amount: u64,
}
emit!(RecordAdded { wallet, amount, note });   // note is for the UI, and the indexer holds it
```

`note` takes part in no check - it is rent for data that reaches the transaction logs anyway. A field in an account is justified only if an instruction relies on it.

### The records the logic needs grow - a PDA per element, not a Vec

```rust
// BAD: every record in one account as a growing vector
#[account]
pub struct Ledger {
    pub wallet: Pubkey,
    pub entries: Vec<Entry>,   // the logic needs them, but in one account: rent, the limit, reallocation
}

// GOOD: one record, one PDA, closed once it has served its purpose
#[account]                     // seeds = [b"entry", wallet, index]
pub struct Entry {
    pub wallet: Pubkey,
    pub amount: u64,
}
// the logic reads the record it needs by its PDA; walking/indexing happens off-chain
```

When the records are genuinely needed on chain (a per-transaction refund, AML on a specific transaction), they are kept - but as one PDA per element: each pays rent only while it lives and is closed once it has served its purpose. A growing `Vec` in one account runs into the size limit and gets more expensive with every addition.
