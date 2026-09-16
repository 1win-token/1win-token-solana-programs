# The Token-Sale program - architecture (Anchor / Solana)

This technical document describes the Sale program - the core of the $1WIN system - at the level of accounts, seeds, instructions, invariants and math. The program implements the business mechanics of [1WIN_Programs_Business_Requirements.md](./1WIN_Programs_Business_Requirements.md) (BRD) and goes deeper into §5 and §8–§9 of [1WIN_Programs_Technical_Requirements.md](./1WIN_Programs_Technical_Requirements.md) (TRD). Where the wordings diverge, the Business and Technical Requirements define the business truth and this document defines the technical truth about Sale.

**The platform:** Anchor 1.x (the `event-cpi` event mechanism; a conditional `init_if_needed` on accounts holding state is forbidden, see §13); classic **SPL Token** (not Token-2022); the standard ATA and System programs. The environment's limits: nested-call (CPI) depth - 4; the compute budget per transaction (CU) - 1,400,000; the transaction size - 1232 bytes.

**Reference notation.** `In` is invariant number n from the table in §3. `§X.Y` is a section of this document. `BRD §X` / `TRD §X` are sections of the Business / Technical Requirements. `G-n` / `T-n` / `TS-n` are requirement identifiers from the Business Requirements (G - the general ones and the roles, T - the token, TS - the token sale); they are carried through for end-to-end traceability.

## Contents

- [Glossary](#glossary)
- [1. Purpose and boundaries](#1-purpose-and-boundaries)
- [2. The system context](#2-the-system-context)
- [3. The invariants](#3-the-invariants)
- [4. The phase machine](#4-the-phase-machine)
- [5. The account model](#5-the-account-model)
- [6. The PDAs and the seeds](#6-the-pdas-and-the-seeds)
- [7. The vaults and fund isolation](#7-the-vaults-and-fund-isolation)
- [8. The roles and authorization](#8-the-roles-and-authorization)
- [9. The instructions](#9-the-instructions)
- [10. The allocation math](#10-the-allocation-math)
- [11. Executing a claim](#11-executing-a-claim)
- [12. The events](#12-the-events)
- [13. Security](#13-security)
- [Worked numeric examples](#worked-numeric-examples)

---

## Glossary

| Term | Meaning |
|---|---|
| **net** | A single wallet's net contribution after AML refunds - the aggregate's `net_usdc_contributed` field. |
| **pool** | The final pool size, frozen when Claim is activated |
| **Soft Cap** | The oversubscription threshold. It is **not stored** separately (TS-4) but computed from the price and the emission (§10.1); the "undersubscribed / oversubscribed" boundary is decided by a comparison with no rounding (§10.5). |
| **activating Claim** | The `activate_claim` instruction (§9.9): a one-time freeze of the pool (`pool_fixed_amount`), the checks that the tokens and the USDC are covered, the creation of `proceeds_vault` / `refund_vault` and the physical split of the USDC between them (§7, §9.9). |
| **the computed phase** | The phase is not stored in a field of its own but computed from the time and the flags on every instruction (§4.1). |
| **the aggregate** | `WalletAggregate` - the PDA that keeps the per-wallet bookkeeping (§5.2). |
| **a deposit** | `Deposit` - a PDA per contribution transaction; the unit of AML granularity (§5.3). |
| **a vault** | An SPL token account under the program's `vault_authority` PDA that holds funds (§5.5). |
| **the refund account** | A fresh USDC token account per refunded deposit, seeds `[b"refund_account", wallet, index]`, created by `refund` with the wallet as its SPL authority (§5.7, §9.8). Not a vault: it is the wallet's. |
| **Graylist** | A `Graylist{wallet, min_per_tx, max_per_tx, max_total}` record: its presence switches the wallet onto individual limits instead of the global ones (§5.6, §9.6a). |

---

## 1. Purpose and boundaries

Sale is a single program in which a participant's entire lifecycle takes place: contribution → AML refund → allocation calculation → claim → rent refund. All of these operations are local to one program. The responsibility boundary in terms of accounts and instructions:

| Inside Sale | Outside Sale |
|---|---|
| Taking USDC, the contribution bookkeeping (the aggregate + the deposits), the computed phases, the pause flag | Minting the emission (done by Owner MS); the token's mint/freeze authority |
| The Blacklist and the Graylist keyed by wallet public key; refunding an individual purchase (before the pool is fixed) | Risk scoring and matching "transaction → deposit" (off-chain, the backend) |
| The vaults under PDAs; the allocation calculation; claim / withdraw / sweep; refunding the rent to the accounts' owners | Burn (its own bookkeeping) - it holds no role in Sale; cross-wallet aggregations (built by the indexer from the events); the multisig signatures |

**The boundary invariant:** Sale does not reference Burn and grants it no roles; the pool's funds are physically unreachable for other programs (I6).

---

## 2. The system context

**The external inputs (they exist before `initialize`):**

- The USDC mint (external, mainnet) and the $1WIN mint (decimals = 8, initially supply = 0). Both are pinned in `SaleState` at initialization.
- The public keys of the three on-chain roles: owner, admin, operator (TRD §2.1).
- The standard programs: Token, ATA, System Program.

The token's mint/freeze authority live outside Sale (§8).

```
   USDC mint (external)     $1WIN mint (supply 0)
          │ read-only              │ read-only
          └──────────┬─────────────┘
                     ▼
              The Sale program (deposit_vault + token_vault under a PDA;
                                proceeds_vault / refund_vault are added when Claim is activated)
   Burn (its own bookkeeping) - there are no mutual references and no cycles
```

---

## 3. The invariants

These statements must hold across every state transition of the program. The "Test" column defines the mandatory minimum of coverage: a positive and a negative path per check.

| # | Invariant | How it is enforced | Test |
|---|---|---|---|
| **I1** | A wallet's contribution bookkeeping balances: before the pool is fixed, `aggregate.net == Σ of the amounts of the wallet's open deposits`. `purchase` increases both quantities, `refund` decreases both and closes the deposit. After the pool is fixed a refund is impossible and `net` does not change until the claim. | The aggregate and the deposit change together inside one instruction; the arithmetic is overflow-checked. | Random purchase/refund sequences → `Σ of the open deposits == net`. |
| **I2** | The overall pool counter balances: before the fix, `deposits_usdc_contributed == Σ net over all aggregates`, and the USDC in the vault is no less than what is accounted for (`deposit_vault.amount ≥ deposits_usdc_contributed`). We count by the program's state, not by the vault's balance. | `purchase` increases `deposits_usdc_contributed` by the contribution amount and `refund` decreases it by the refund amount - each by exactly the amount transferred into or out of `deposit_vault`, so the counter cannot diverge from the actual USDC movement; `activate_claim` checks `deposit_vault.amount ≥ deposits_usdc_contributed` explicitly (§9.9, step 3). | On activation: `Σ net == deposits_usdc_contributed ≤ deposit_vault.amount`. |
| **I3** | No more is handed out than was minted: the sum of the tokens paid out ≤ `distributable` ≤ the `token_vault` balance at activation time; tokens round down. | §10, I12; a token transfer never exceeds the vault's balance. | After every claim: `token_vault ≥ 0`, Σ of the payouts ≤ `distributable`. |
| **I4** | There is always enough for the refunds: after the fix, refunds are paid out of `refund_vault`, into which activation put `refund = pool_fixed_amount − proceeds` (the whole refund amount plus the rounding reserve). `withdraw_proceeds` has no access to `refund_vault`. | The split at activation (§9.9, step 5) puts everything except the proceeds into `refund_vault`; the justification that the reserve suffices is in §10.7. | After the maximum proceeds withdrawal and every claim, the refunds are paid and `refund_vault ≥ 0`. |
| **I5** | A deposit and an aggregate are spent **exactly once**: a deposit is closed by a refund or by a batch close, an aggregate by a claim or by a batch. `claim` closes the aggregate unconditionally, so the only protection against a repeat is the already-closed account (`AccountNotInitialized`); there is no separate flag. On top of that, `refund` creates the deposit's refund account with `init` at seeds that carry the deposit's index (§5.7): a second, independent guard against a repeat, since `deposit_count` never decreases (§6.2). | Closing zeroes the lamports and the data and returns the account to the System Program → it cannot be read again; an `init` at an occupied address fails. | **Negative:** a repeated claim / refund fails (`AccountNotInitialized`). |
| **I6** | Money leaves the vaults only along permitted paths (the full inflow/outflow table for `deposit_vault` / `proceeds_vault` / `refund_vault` / `token_vault` is in §7): `refund` (before the fix, out of `deposit_vault` into the deposit's refund account, whose SPL authority is the contributing wallet - the wallet's account, not a vault, §5.7), `claim` (tokens out of `token_vault` and, when the refund is > 0, USDC out of `refund_vault`), `withdraw_proceeds` (Owner, the whole `proceeds_vault` balance), `sweep_unclaimed` (Owner, after the deadline, one vault at a time). There are no other paths. | Every vault is owned by the single `vault_authority` PDA; only the Sale program signs a transfer or a vault close. | **Negative:** a transfer out of a vault or a vault close by a foreign key or program → an error. |
| **I7** | Money is counted without overflow: every operation on amounts is checked (`checked_*`), and an overflow reverts. | Amounts are u64, the intermediates are u128 (§10.4). | Boundary values (max u64, the maximum number of participants). |
| **I8** | Parameters and the schedule are never moved retroactively: the sale dates (`start_date` / `end_date`) change **only before the start** (the PreSale phase, TS-9) - a running or finished sale cannot be extended; **the price, the emission and the per-transaction limits are frozen forever from the start** (`now ≥ start_date`, TS-7, §4.3); the Claim schedule (`claim_start`, the duration) is editable **until the pool is fixed** (`!pool_fixed`) and immutable after; activating Claim happens **once** (`pool_fixed` and `pool_fixed_amount` are written a single time); after the fix `pool_fixed_amount` is immutable, including while accounts are being closed (TS-26, §9.13). | The checks in `set_*` require the PreSale phase (the dates / the price / the limits) or `!pool_fixed` (the Claim schedule); `activate_claim` requires and then sets `pool_fixed`. | **Negative:** `set_sale_schedule` / `set_price` / `set_tx_limits` with `now ≥ start_date` → `SaleAlreadyStarted`; `set_claim_schedule` with `pool_fixed` → `PoolAlreadyFixed`; a repeated activation fails; `close_deposits` / `close_aggregates` do not change `pool_fixed_amount`. |
| **I9** | The project cannot withdraw more than its proceeds: the withdrawal limit equals the `proceeds_vault` balance, into which activation put exactly the proceeds. Oversubscribed: `proceeds_vault = soft_cap − reserve` (the reserve is held back on the refund side, in `refund_vault`). Undersubscribed: `proceeds_vault = pool_fixed_amount` (there are no refunds and no reserve is held back). | `withdraw_proceeds` holds only `proceeds_vault` in its context, transfers its entire balance and closes the vault; there is no separate withdrawn-amount counter and no arithmetic limit check | **Negative:** an instruction without `proceeds_vault` in its context cannot reach the proceeds (`claim` / `sweep_unclaimed` do not hold it). **Positive:** after the whole proceeds withdrawal and every claim, `refund_vault` still covers the remaining refunds (§10.7). |
| **I11** | The Claim window is always at full length and always available in full: `claim_deadline − claim_start == claim_duration` (TS-28), and at the moment the pool is fixed the window has not yet begun (`now < claim_start`). | `claim_deadline` is computed as `claim_start + claim_duration` on every phase query; the equality holds by construction (there is no stored field). `claim_duration` itself is bounded below on every write path (`MIN_CLAIM_DURATION`, §9): a window too short for participants to use never reaches the state, and after the fix nobody could widen it. `activate_claim` requires `now < claim_start` (§9.9) - otherwise, once `pool_fixed` was set, the phase would immediately compute as Claim or ClaimClosed and participants would get a truncated window or none at all. The setters enforce the same predicate at the moment of the edit (§4.3), so a start that has already passed never reaches the state; `activate_claim` re-asserts it because the pool is fixed later than the schedule is set. | Moving `claim_start` moves the beginning but does not shorten the window: the deadline is recomputed from the same duration. **Negative:** fixing with `now ≥ claim_start` (including after the deadline) → `ClaimStartInPast`; **positive:** fixing at `claim_start − 1` succeeds, the phase stays SaleEnded until `claim_start`, and from `claim_start` the claim is open. |
| **I12** | Claim cannot switch on without tokens in the vault: activation requires `token_vault.amount ≥ distributable` (≥, not ==); between activation and the start of Claim no tokens leave the vault. On top of that, the vault's funding is verifiable on-chain before the first payouts: `assert_funded` requires `token_vault.amount ≥ emission` and serves as the gate of the issuance transaction before the right to issue is revoked. | The check in `activate_claim` (§9.9, step 2); a separate side-effect-free check - `assert_funded` (§9.9a). | **Negative:** an under-mint → `VaultNotFunded` from both instructions; an under-mint inside the issuance transaction brings the whole thing down and the right to issue survives. **Positive:** the mint and the activation in a single transaction; the check passes on an exact emission. |
| **I13** | Purchase limits are applied on a "no data - no permission" basis: when a Graylist record exists for the wallet, **its** `min` / `max` apply along with the condition `aggregate.net + amount ≤ max_total`; otherwise the global limits do. The Graylist slot is a mandatory account bound by seeds. | `purchase` takes a mandatory Graylist slot at the seeds `[b"graylist", wallet]`. | **Negative:** a purchase outside the Graylist limit → `BelowMin` / `AboveMax` / `WalletTotalExceeded`; a purchase with a substituted or missing slot → an error. **Positive:** the Graylist limits replace the global ones; with no record, the global ones apply. |
| **I14** | Oversubscribed: `proceeds_vault.amount + refund_vault.amount == pool_fixed_amount`, with the whole rounding reserve in `refund_vault`. Undersubscribed: `refund_vault` is not created and `proceeds_vault.amount == pool_fixed_amount`. | `activate_claim` computes `proceeds + refund == pool_fixed_amount` and transfers the shares out of `deposit_vault` (when undersubscribed, the whole pool goes into `proceeds_vault`); it is verified from the `ClaimActivated` event and the measured balances - there are no stored `*_vault_amount` fields (§10.5). | **Positive:** oversubscribed - `proceeds_vault_amount + refund_vault_amount == pool_fixed_amount`; undersubscribed - `proceeds_vault_amount == pool_fixed_amount`, `refund_vault_amount == 0`, and `refund_vault` does not exist. |

---

## 4. The phase machine

### 4.1 The lazy model

The source of time is `Clock::unix_timestamp`. The phase is **not stored** in a field of its own but derived on every instruction from the time and the flags. What is stored: `start_date`, `end_date`, `claim_start`, `claim_duration`, and the `pool_fixed` / `paused` flags. `claim_deadline` is not stored - it is computed as `claim_start + claim_duration`.

```rust
fn current_phase(now: i64, s: &PhaseSchedule) -> Phase {
    let claim_deadline = s.claim_start.saturating_add(s.claim_duration); // the deadline is not stored
    if s.pool_fixed {
        if now >= claim_deadline { Phase::ClaimClosed }     // the project withdraws the unclaimed
        else if now >= s.claim_start { Phase::Claim }       // handing out tokens and refunding USDC
        else { Phase::SaleEnded }                           // activated, waiting for claim_start
    } else {
        if now < s.start_date { Phase::PreSale }            // intake closed, the parameters are editable
        else if now < s.end_date { Phase::Sale }            // taking USDC (unless paused)
        else { Phase::SaleEnded }                           // the AML settlement window
    }
}
```

`current_phase()` is a pure function: its `PhaseSchedule` argument carries only the time fields and the phase flags (`start_date`, `end_date`, `claim_start`, `claim_duration`, `pool_fixed`), so the function is tested on boundary time values separately from the on-chain wiring. Outside the tests the only constructor of `PhaseSchedule` is the wrapper method `SaleState::phase(now)`, which assembles a `PhaseSchedule` from the state's fields and calls `current_phase()`. Every instruction checks the phase only through `SaleState::phase()`; scattered time comparisons are forbidden. This removes the main risk of a computed phase - inequalities drifting out of sync when time is compared in several places.

### 4.2 The lifecycle

```
initialize ──► PreSale ──(start_date)──► Sale ──(end_date)──► SaleEnded ──► activate_claim
                                          (paused - a flag)                 (the mint is already in the vault - I12)
            SaleEnded(activated) ──(claim_start)──► Claim ──(deadline)──► ClaimClosed (terminal:
                                                                          sweep, batch closing)
```

There are **no** dedicated phase-transition instructions - every transition happens automatically with time (TS-42). The only instruction that changes the trajectory is `activate_claim`. There is no "end the sale manually" instruction (TS-10): the only way to stop the intake early is the pause.

**Why the pool cannot be fixed while the intake is open (TS-12).** `pool_fixed` is set **only** in `activate_claim`, which requires the SaleEnded phase (`now ≥ end_date`); a purchase is possible only in the Sale phase (`now < end_date`). Sale and SaleEnded do not overlap in time, so the pool cannot be fixed while the intake is open - that is guaranteed by the structure, not by a separate check.

**The two sub-modes of the SaleEnded phase:**
- **before the pool is fixed** - `refund`, `set_claim_schedule` and `activate_claim` are available (this is exactly the AML settlement window, TS-24);
- **after the fix and before `claim_start`** - `withdraw_proceeds` is available only from `claim_start` (§9.12); `refund`, `set_claim_schedule`, a repeated activation and both batch closes (§9.13) revert.

### 4.3 The time invariants

- `set_sale_schedule` changes the schedule only in the PreSale phase (the exact conditions are in the table of §9.2); once Sale begins the schedule is immutable - a running or finished sale cannot be extended.
- The price, the emission and the per-transaction limits (`set_tx_limits`) change only in PreSale (`now < start_date`). The prohibition is irreversible by construction: `start_date` is editable only before the start, so once the condition `now ≥ start_date` becomes true it never becomes false again → the terms of contributions already taken cannot be changed retroactively (TS-7).
- The Claim schedule (`claim_start`, `claim_duration`) is set by Admin before the pool is fixed (`set_claim_schedule`, `!pool_fixed`): `anchor < claim_start ≤ anchor + MAX_CLAIM_START_DELAY` (30 days) with `anchor = max(end_date, now)`, `now` being the moment of the edit; the duration satisfies `MIN_CLAIM_DURATION ≤ claim_duration ≤ MAX_CLAIM_DURATION` (1 hour … 12 months). Before the sale ends the anchor is `end_date`; after it, the moment of the edit - so a future start can be scheduled at any time while the pool is not fixed, and the fixation is never locked out by a ceiling that has already passed. `activate_claim` re-asserts `now < claim_start` at the moment the pool is fixed (§9.9): the schedule may be set long before the fix, so the window is checked again at the fix itself. After the fix the schedule is immutable - no instruction to edit it exists. `claim_deadline` is not stored (= `claim_start + claim_duration`).
- Time precision: validator seconds drift slightly; the deadlines are measured in weeks and months, so second-level precision is not relied upon.

---

## 5. The account model

Solana has no global mutable "mapping" - state lives in individual accounts. Per-participant bookkeeping is kept as a PDA per wallet (the aggregate) plus a PDA per contribution (the deposit): this isolates the bookkeeping between wallets, gives a fixed account size and makes it possible to return the rent to its owner.

There is therefore **no** full parallelism of purchases: every `purchase` writes the shared `total` counter in `SaleState`, so the runtime executes concurrent purchases sequentially. This is an accepted throughput limitation (§13.2).

The account sizes below are a guide; the final size is fixed through `#[derive(InitSpace)]`.

### 5.1 SaleState (a singleton, seeds `["sale_state"]`)

| Field | Type | Meaning |
|---|---|---|
| `owner` | Pubkey | Owner MS - a narrow set of rights: minting, withdrawing funds, the sweep, role management (§8). Changed in two steps: `propose_owner` + `accept_owner` (§9.4). |
| `pending_owner` | Option\<Pubkey\> | The owner candidate before confirmation. `None` - no handover is in progress; `Some(key)` - nominated, but the role has not moved yet. Filled in by `propose_owner`, cleared by `accept_owner` (the handover happened) and by `cancel_owner_proposal` (the handover was cancelled) - §9.4. |
| `admin` | Pubkey | Ops MS - every parameter, the schedule, the pause, activating Claim. |
| `operator` | Pubkey | The automation key. |
| `usdc_mint`, `token_mint` | Pubkey ×2 | The mints (read-only references). |
| `deposit_vault`, `token_vault` | Pubkey ×2 | The PDA token accounts of the vaults that exist from `initialize` on. `proceeds_vault` / `refund_vault` are created at activation and their addresses are not stored in the state (§5.5, §6.1). |
| `price_usdc` | u64 | USDC units per one whole $1WIN token (`30_000` for $0.03, §10.1). Immutable from the start of Sale (I8). |
| `emission` | u64 | The emission in token units (130M × 10^8). Immutable from the start of Sale (I8). |
| `min_per_tx`, `max_per_tx` | u64 ×2 | The **global** per-transaction limits (USDC units); they apply to wallets with no Graylist record (§5.6, I13). Immutable from the start of Sale (I8). |
| `deposits_usdc_contributed` | u64 | The sum of the contributions taken before the fix. Its mutators are `purchase` (↑) and `refund` (↓), both only before activation. After the fix the logic never reads it - all of the claim math runs off `pool_fixed_amount`. |
| `pool_fixed_amount` | u64 | The frozen pool size: written **once** in `activate_claim` (= `deposits_usdc_contributed` at the moment of the fix). The allocation, the participant refunds and the split of the USDC into proceeds and refunds are all computed from it. Its only mutator is `activate_claim`, once (I8). |
| `participants` | u64 | How many unique wallets contributed in total. It grows by 1 on a wallet's first contribution (`init_aggregate`) and never decreases - not even after a wallet has taken everything back and its record has been closed; the monotonicity holds by construction (neither `refund` nor `close_aggregates` touches it - §9.13). It sets the size of the rounding reserve when the proceeds are withdrawn (§9.12, §10.7): overstating it only increases `refund_vault` and therefore preserves the refunds' solvency. |
| `start_date`, `end_date` | i64 ×2 | The window of the Sale phase. Two timestamps are stored (rather than "start + duration") so that the conditions are checked independently with no derived recomputation. |
| `claim_duration` | i64 | The Claim duration: 12 months by default, bounded by `MIN_CLAIM_DURATION` (1 hour) and `MAX_CLAIM_DURATION` (12 months, coinciding with the default). Editable until the pool is fixed. |
| `claim_start` | i64 | The start of Claim. Set on `initialize`, editable until the pool is fixed; `max(end_date, now) < claim_start ≤ max(end_date, now) + 30 days` at the moment of the edit (§4.3); at the moment the pool is fixed it must still lie ahead (`now < claim_start`, §9.9). |
| `pool_fixed`, `paused` | bool ×2 | The pool fix (written once) / the intake pause. |
| `vault_authority_bump` | u8 | The canonical bump of the `vault_authority` PDA; needed to sign CPIs (§6.3). |

**The size:** Pubkey 7×32 = 224 + Option\<Pubkey\> 33 + u64 7×8 = 56 + i64 4×8 = 32 + 3×1 (the two flags and the bump) = a 348 B body, **356 B** with the discriminator. The account is never closed.

### 5.2 WalletAggregate (a PDA per wallet, seeds `["aggregate", wallet]`)

The single record of truth about a wallet (TS-14, G-8). The alternative - "one global list" - was rejected: it would serialize the purchases, run into the account size limit and make it impossible to return the rent piecemeal.

| Field | Type | Meaning |
|---|---|---|
| `wallet` | Pubkey | The owner (binds the signer and the recipient). |
| `net_usdc_contributed` | u64 | net (I1). |
| `deposit_count` | u64 | Grows by 1 on every contribution and never decreases on a refund; it defines the next deposit's address (§6.2). |

**The size: 56 B.** The User pays the rent (`init_aggregate`); `claim` closes the aggregate unconditionally, otherwise the `close_aggregates` instruction does it after the deadline - **the rent always goes back to the User** (TS-43).

On a full refund (`net == 0`) the aggregate lives until the deadline and is **not** closed: closing it early would lose `deposit_count`, and a new `init_aggregate` would give deposit #0 an already-used address (address reuse, §6.2). Closing it via a claim is safe: a purchase in the Claim phase is structurally unreachable.

### 5.3 Deposit (a PDA per contribution, seeds `["deposit", wallet, deposit_count_le]`)

One contribution = one account with a frozen amount - **the unit of AML granularity** (TS-20). The cost of storing a deposit: ~0.0015 SOL of rent per contribution (paid by the User, returned when it is closed); spam is bounded by `min_per_tx`.

| Field | Type | Meaning |
|---|---|---|
| `wallet` | Pubkey | The owner (`has_one = wallet`). |
| `amount` | u64 | The amount in USDC units. |
| `index` | u64 | Equal to `deposit_count` at creation time; it is part of the seed and is checked on a refund (§6.4). |

**The size: 56 B.** Closed by `refund` (before the fix) or by `close_deposits` (in a batch, in the ClaimClosed phase; from the pool fix onwards a deposit is an immutable archive, TS-43); **the rent always goes back to the User**: on a refund it follows the USDC into the wallet's refund account (§5.7, §9.8), on a batch close it is credited to the wallet.

### 5.4 Blacklist (a PDA per wallet, seeds `["blacklist", wallet]`)

The existence of the account under the program id = the deposit path from that wallet is blocked (TS-15, TS-18): both instructions that create wallet state revert - `init_aggregate` (§9.5) and `purchase` (§9.6). Its only field is `wallet` (a Pubkey, duplicating the seed for the indexer's convenience). **The size: 40 B.** It is maintained by the Operator: it pays the rent when adding and gets it back when removing.

### 5.5 The vaults and `vault_authority`

Funds are held in SPL token accounts (165 B each) under literal seeds. On the USDC side there are up to three vaults: `deposit_vault` (created at `initialize`), `proceeds_vault` (always created at `activate_claim`) and `refund_vault` (created at `activate_claim` **only on oversubscription** - when undersubscribed there are no refunds and the vault is not needed). On the token side there is `token_vault` (created at `initialize`). The authority over every vault is a single `vault_authority` PDA with no data of its own: signing transfers and closes is moved into a narrow PDA and is not mixed with mutable state. The refund accounts of §5.7 are not vaults: the program creates them, but they belong to the wallets.

- **What the USDC vaults are for:** `deposit_vault` - taking contributions and AML refunds before the pool is fixed; `proceeds_vault` - the project's proceeds; `refund_vault` - the participant refunds and the rounding reserve. Which instruction is bound to which vault is in §7; the split mechanism at activation is in §9.9.
- **The creation rent:** the rent for `deposit_vault` / `token_vault` is paid by Owner MS at initialization; the rent for `proceeds_vault` (and for `refund_vault`, when it is created - on oversubscription) is paid by whoever calls `activate_claim` (Admin), at their `init`.
- **The closing rent goes to the address supplied as an account to the instruction.** When a vault is closed the rent is returned to the address passed as an account to the closing instruction.

### 5.6 Graylist (a PDA per wallet, seeds `["graylist", wallet]`)

A wallet's individual contribution limits. The existence of a record under the program id = the wallet is subject to **individual** limits instead of the global ones. It is built like the Blacklist (a PDA per wallet) but holds data that `purchase` reads. The records are maintained by Admin (§8).

| Field | Type | Meaning |
|---|---|---|
| `wallet` | Pubkey | The owner (duplicates the seed for the indexer; the binding is by seeds). |
| `min_per_tx` | u64 | The individual per-transaction minimum (USDC units). Replaces the global one. |
| `max_per_tx` | u64 | The individual per-transaction maximum (USDC units). Replaces the global one. |
| `max_total` | u64 | The maximum total contribution across the whole sale (USDC units): `aggregate.net + amount ≤ max_total`. |

**The size: 8 + 32 + 8×3 = 64 B** (with the discriminator). The records live until the end of the sale; after the pool is fixed Admin may close them (the limits are no longer read - there are no purchases) and the rent goes back to the payer (the project).

**Validation on write (`graylist_set`):** `0 < min_per_tx ≤ max_per_tx ≤ max_total`; otherwise `InvalidParams` (for instance `min > max` would structurally block any purchase). A `max_total` below what has already been contributed is allowed: it only forbids further purchases (`purchase` → `WalletTotalExceeded`), while the contribution already made and its allocation in Claim are computed from `net` and are unaffected. The field is not checked against `aggregate.net` (the aggregate may not exist yet, and reading it in `graylist_set` is not required).

### 5.7 The refund account (a token account per refunded deposit, seeds `["refund_account", wallet, index]`)

The destination of a forced refund (§9.8): a fresh USDC token account (165 B) that `refund` creates at a program-derived address with the wallet as its SPL authority, transfers the deposit's USDC into, and closes the deposit into (the deposit's rent follows the USDC). From that moment the account is the wallet's and the wallet's alone: it moves the USDC or closes the account with its own signature, and the program neither reads nor closes it afterwards.

- **Why a fresh account and not the wallet's ATA.** A forced action must not depend on any account the counterparty controls. The ATA's address is fixed, but its SPL owner is a field the wallet rewrites with `SetAuthority(AccountOwner)` on its own signature: the account then still occupies the canonical address while no longer belonging to the wallet, and a refund bound to it would be rejected on that field - the participant would block the refund of their own deposit at no cost. The same goes for a closed ATA, an ATA frozen by the issuer, or a wallet turned into an executable account (which refuses any lamport credit). The refund account exists only from the refund itself: only the Sale program can create an account with data at its address, an address pre-funded with lamports is completed to the token account (allocate + assign), and there is nothing to rotate, close or freeze in advance. Nothing of the wallet's own account is read or written either.
- **One per deposit, used once.** The index is in the seeds and the deposit is closed in the same instruction; `deposit_count` never decreases (§6.2), so the slot is never reused. The account is created with `init`, not `init_if_needed`: an existing account at that address is an anomaly and fails the instruction.
- **The rent** (165 B) is paid by the operator on every refund - an operational cost of the project, not returned to it: the wallet may close the account and take the rent, together with the deposit's rent that landed there (TS-43). The program never closes the account: unlike the program's own records, it belongs to a participant.
- **Finding it.** The address is derived from `(wallet, index)` - the pair the `Refunded` event carries - so the backend, the indexer and the wallet's owner compute it without any stored field.

---

## 6. The PDAs and the seeds

### 6.1 The PDA schemes

| PDA | Seeds | Components | Bump |
|---|---|---|---|
| `SaleState` | `[b"sale_state"]` | a literal | not stored - the canonical one is recomputed |
| `WalletAggregate` | `[b"aggregate", wallet]` | a literal + 32 B | not stored - the canonical one is recomputed |
| `Deposit` | `[b"deposit", wallet, deposit_count.to_le_bytes()]` | a literal + 32 B + 8 B (u64 LE) | not stored - the canonical one is recomputed |
| `Blacklist` | `[b"blacklist", wallet]` | a literal + 32 B | not stored - the canonical one is recomputed |
| `Graylist` | `[b"graylist", wallet]` | a literal + 32 B | not stored - the canonical one is recomputed |
| the refund account | `[b"refund_account", wallet, index.to_le_bytes()]` | a literal + 32 B + 8 B (u64 LE) | not stored - the canonical one is recomputed + `token::mint` / `token::authority` |
| `deposit_vault` / `token_vault` | `[b"deposit_vault"]` / `[b"token_vault"]` | a literal | not stored - bound by the address from `SaleState` |
| `proceeds_vault` / `refund_vault` | `[b"proceeds_vault"]` / `[b"refund_vault"]` | a literal | not stored - the canonical one is recomputed + `token::mint` / `token::authority` |
| `vault_authority` | `[b"vault_authority"]` | a literal | stored: `vault_authority_bump` in `SaleState` |
| `event_authority` | `[b"__event_authority"]` | a literal (Anchor's `#[event_cpi]`) | managed by Anchor |

Every component has a fixed length, and the literal prefixes are pairwise not prefixes of one another, so the PDA spaces do not intersect (a collision of the form `[b"ab", b"cd"] == [b"abcd"]` is excluded).

### 6.2 The deposit PDA via a counter

`purchase`: (1) read `deposit_count = N` from the aggregate; (2) create (`init`) the deposit at the seed `[…, N_le]` and write `index = N`; (3) increment `deposit_count = N + 1` (overflow-checked). The hard implementation requirements:

- The seed **must** reference the account field `aggregate.deposit_count`: Anchor computes the seed for `init` while deserializing the context, **before** the handler body - the address is deterministic and there is no race.
- The increment happens **only in the handler body**, after the deposit has been created successfully.
- `index` is **never passed as an argument** to `purchase` (otherwise a client could name an occupied or a future slot).
- `deposit_count` **never decreases** on a refund/close → a closed deposit's address is never reused (protection against address reuse, §13). The same holds for the deposit's refund account, whose seeds carry the same index (§5.7).

### 6.3 The bump policy

The same seeds admit several valid bumps, and each yields a valid PDA address of its own. The canonical one is what `find_program_address` returns. The requirement: **a PDA's address is always proven by recomputing the canonical bump, never by a value taken from the account being checked.**

- Creation (`init`) - `bump` with no value: Anchor puts the canonical one into `ctx.bumps`.
- Checking an existing PDA - also `bump` with no value: Anchor recomputes the canonical address and compares it with the one supplied. This makes canonicity a property of the check itself. The construction `bump = X.bump`, where the bump is taken from the body of the account being checked, is forbidden: it only confirms that the address agrees with that value, not that the value is canonical - and a record at a non-canonical address would pass the check.
- A manual address comparison (batch closing, §9.13) - `find_program_address(seeds).0`, not `create_program_address(seeds, bump_from_the_body)`.
- A bump never arrives as an instruction argument.

The only stored bump is `vault_authority_bump` in `SaleState`: it is needed as a seed byte for signing CPIs (`[b"vault_authority", &[bump]]`), it is fixed to the canonical value at `initialize`, and it is taken from another, already structurally verified account. The remaining bumps are not stored in the state: no instruction reads them.

`deposit_vault` and `token_vault` are bound by the address stored in `SaleState` plus `token::authority` / `token::mint`; no seed derivation is performed for them. `proceeds_vault` / `refund_vault` are created only at activation and their addresses are not stored in the state - the binding is by the literal seeds (the canonical bump is recomputed) plus `token::authority` / `token::mint`. A refund account (§5.7) is created by `refund` at the seeds `[b"refund_account", wallet, index]` (the canonical bump is recomputed) with `token::mint = usdc_mint` and `token::authority = wallet`.

The cost of recomputing is 1500 CU per search iteration, on average two iterations per PDA. On single instructions that is a few percent of the budget; it only becomes noticeable in the batch close, where a PDA is derived for every pair (the measurements are in the `cu_bench` benchmark).

### 6.4 Addressing a deposit (`refund(index)`, the wallet is an account)

An existing deposit is addressed by the seeds `[b"deposit", wallet.key(), index]`: the contributing wallet is passed as an account (`UncheckedAccount`, read-only: it is an address, and nothing of the account is read or written - the USDC and the rent of the deposit being closed arrive at the deposit's refund account, §5.7), and the index is the instruction's only argument (`index: u64`, declared in `#[instruction(...)]` before it is used in `seeds`); `has_one = wallet` pins `deposit.wallet == wallet`.

The wallet is needed here as **the address the refund belongs to**, not as an account of a particular kind, which is why its owning program is not checked. The address's authenticity is proven structurally without that anyway: the seeds of the deposit and of the aggregate are derived from `wallet`, `has_one = wallet` checks the address against the deposit's body, the refund account is bound by the seeds `[b"refund_account", wallet, index]` plus `token::authority = wallet` / `token::mint = usdc_mint`, and the amount is taken from `deposit.amount`. A foreign address cannot be substituted - not one of those slots would line up under it, and a foreign account in the refund slot fails the seeds. Requiring `owner == System Program` (the `SystemAccount` type) would not be a tightening here but a denial of service: an account's owner is a mutable field that a participant rewrites with a signature from their own wallet (§6.5), and the deposit would become unrefundable by their own decision.

### 6.5 The type of a participant's wallet account

Only an address with a private key of its own - one that lies on the Ed25519 curve - can take part in the sale (TS-12a). Program wallets (a multisig, a custody service, a smart wallet) are not supported: their address is derived programmatically and does not lie on the curve.

The check stands at the entry of the deposit path - in both instructions that create wallet state (`init_aggregate` §9.5 and `purchase` §9.6). In the context it is a constraint on the `user` slot:

```rust
#[account(mut, constraint = bytes_are_curve_point(user.key()) @ SaleError::WalletNotOnCurve)]
pub user: Signer<'info>,
```

`bytes_are_curve_point` comes from `anchor_lang::solana_program::pubkey`; on SBF it is the `sol_curve_validate_point` syscall, off SBF it is `curve25519-dalek`. The same code serves both builds and the program needs no extra dependency.

Neither the signature check nor the account-owner check substitutes for this one. `Signer` confirms only the fact of a signature, and a program wallet signs through a call from its own program and passes just like an ordinary one. The `SystemAccount` type (the account's owner is the System Program) rejects program-owned accounts that already exist, but an uncreated program address is also attributed to the System Program - and gets through.

**The check stands only at the entry, because that is the only place where it holds.** Whether an address lies on the curve is a property of its own bytes: the address does not change, so the result of the check does not change over time either. An account's owner, by contrast, is a mutable field: the system `assign` instruction rewrites it on the account's own signature, and a participant can hand their wallet over to an arbitrary program. The wallet's signature cannot bring it back - only the owning program may do that, and only while the account's data is empty; and if the recipient program does not exist (the System Program does not check that), the move is irreversible. Two consequences follow, and both are expressed in the instruction contexts:

- Checking the owner in the accompanying instructions is pointless as protection: a participant who wants to keep buying keeps the wallet under a program they control and returns ownership for the duration of the purchase with a single instruction inside the same transaction - no window opens for the operator to react.
- Such a check is harmful as a restriction: a wallet moved under a foreign program would stop being reachable for a ban, for individual limits and for a refund, whereas a purchase and a claim never read the account owner at all. In the irreversible case the deposit would become permanently unrefundable.

That is why **the wallet's type is pinned once, at the entry of the deposit path, and from then on the wallet is used everywhere as an address**: in `refund` (§9.8) it is passed as a read-only account with no owner check and is neither read nor written (`UncheckedAccount`, the binding is in §6.4; the refund lands in a fresh account of the wallet's, §5.7), and in the Blacklist/Graylist lists (§9.7, §9.7a) it is not an account at all but an instruction argument. None of them carries a separate curve check and none needs one: after the check at the entry, a deposit and an aggregate can only exist for a wallet that is on the curve. The same holds in `claim` (§9.10) - without an aggregate the instruction does not execute.

---

## 7. The vaults and fund isolation

| Vault | Where it is topped up from | Outflows |
|---|---|---|
| `deposit_vault` | `purchase` | `refund` |
| `proceeds_vault` | `activate_claim` | `withdraw_proceeds` |
| `refund_vault` | `activate_claim` | `claim` (when the refund is > 0); `sweep_unclaimed` |
| `token_vault` | the one-off emission mint by Owner MS **before activation** | `claim`; `sweep_unclaimed` |

- **Signing transfers and closes.** Every vault is owned by the single `vault_authority` PDA; the program signs a transfer and a close through `CpiContext::new_with_signer` with the seeds `[b"vault_authority", &[bump]]`, where the bump is `sale_state.vault_authority_bump` (§6.3). Users have no direct access to the vaults.
- **`proceeds_vault` and `refund_vault` are context-incompatible.** No instruction holds both as `mut` at once (except `activate_claim`, which creates them), so the proceeds cannot flow into the refunds or vice versa. The vaults' addresses differ (different seed literals), so a debit from one cannot be redirected to the other's address.
- **Minting happens outside the program.** The program does not hold the mint authority (T-4): even with a bug in the code it does not print tokens, it only redistributes what has already been minted (I3). The mint authority belongs to Owner; revoking it to `None` (T-6) is performed once after the whole token supply has been issued - the sale's emission and every vesting bucket (the $1WIN mint is shared) - normally in a single transaction that mints and then calls `SetAuthority(None)` (TRD §4, §7.7). The Sale program contributes its own link in that transaction: `assert_funded` (§9.9a) checks that the whole sale emission has been collected in `token_vault` and brings the transaction down before the revoke if it has not. The mint authority survives until the revoke - so on an operational under-mint the sale's emission can be topped up before the pool is fixed. The procedure for minting the sale's emission and its ordering relative to activation are in TRD §5.6 and §9.2.
- **The calculations never read a vault's balance.** The allocation and the USDC split are computed only from the stored `emission` / `price_usdc` / `pool_fixed_amount` / `participants`. The exception is `sweep_unclaimed` in the terminal phase: it deliberately withdraws the vault's entire remainder, which by then is no longer an accounted quantity.
- **Protection against substitution.** The vaults are checked by `token::mint == sale_state.usdc_mint` / `sale_state.token_mint`, `token::authority == vault_authority` and, additionally: `deposit_vault` / `token_vault` by the address from `SaleState`, `proceeds_vault` / `refund_vault` by the literal seeds. The recipients are never the signer: in `claim` the wallet's ATAs with `authority = aggregate.wallet`; in `refund` the deposit's refund account, created by the instruction with `authority = deposit.wallet` (§5.7).

---

## 8. The roles and authorization

The roles' sets of rights **do not intersect** (G-1): Owner does not inherit Admin's operations, and Admin does not inherit Operator's automation (the hot key exists for speed, and a multisig has no need for automation). Each role's powers are in the table below; Operator is the automation key and User is a participant.

| Instruction | Owner | Admin | Operator | User |
|---|---|---|---|---|
| `initialize` (= the upgrade authority) | ✓ | - | - | - |
| `set_price`, `set_tx_limits`, `set_sale_schedule`, `pause` / `unpause` | - | ✓ | - | - |
| `set_emission`, `set_claim_schedule` | - | ✓ | - | - |
| `propose_owner`, `cancel_owner_proposal`, `set_admin`, `set_operator` | ✓ | - | - | - |
| `accept_owner` | - | - | - | the nominated candidate |
| `activate_claim` | - | ✓ | - | - |
| `blacklist_add` / `blacklist_remove`, `refund`, `close_deposits` / `close_aggregates` | - | - | ✓ | - |
| `graylist_set` / `graylist_remove` | - | ✓ | - | - |
| `claim` (self-claim) | - | - | - | ✓ |
| `init_aggregate`, `purchase` | - | - | - | ✓ |
| `withdraw_proceeds`, `sweep_unclaimed` | ✓ | - | - | - |
| `assert_funded` | - | - | - | - |

- **`assert_funded` requires no role** (§9.9a): its context has no `Signer` slot, the instruction changes no state and grants no rights - an empty row in the matrix means "the signer is not checked", not "forbidden to everyone".
- The roles are checked on-chain: `signer.key() == sale_state.<role>` plus the Signer marker (through `has_one` or an explicit check); User is bound through `aggregate.wallet == signer` (a seed or `has_one`). The frontend does not check roles (TS-16).
- **Changing Owner (G-3):** the handover is two-step. `propose_owner(new)` requires `signer == sale_state.owner`, checks `new != Pubkey::default()` and records the candidate in `pending_owner`; `owner` itself does not change. `accept_owner()` is signed by the candidate: the instruction checks `pending_owner == signer`, moves the value into `owner` and clears `pending_owner`. `cancel_owner_proposal()` withdraws the nomination and returns the state to "no handover". A typo in the address never gives away control - `accept_owner` cannot be signed, the rights stay with the sitting Owner, and the nomination is replayed with another `propose_owner` or cancelled.
- The token's mint/freeze authority live outside the program and are changed with the `SetAuthority` instruction (Owner MS / Ops MS; see §7, TRD §4).

---

## 9. The instructions

**The general rules.** Phase preconditions are checked through `current_phase()` (§4.1). `MathOverflow` is possible in any money-handling instruction (all of the arithmetic is `checked_*`). Every error code lives in a **single** `#[error_code] enum` (an Anchor requirement). Touching a closed or non-existent `Account<T>` is rejected by Anchor with **`AccountNotInitialized` (3012) during deserialization, before the handler body** - the negative tests for a repeated claim/refund assert exactly that. There are no hardcoded business values (G-5); the defensive constants (not business parameters) are: `MAX_SALE_START_DELAY = 90 days` (the ceiling on how far the sale start may be deferred from `now`), `MAX_SALE_DURATION = 30 days` (the ceiling on the length of the intake window `end_date − start_date` against a planned ~2 weeks), `MAX_CLAIM_START_DELAY = 30 days` (the ceiling on how far `claim_start` may be deferred from the later of `end_date` and the edit, TS-27), `MAX_CLAIM_DURATION = 12 months` (the ceiling on `claim_duration` - insurance against an accidentally open-ended Claim; it coincides with the default) and `MIN_CLAIM_DURATION = 1 hour` (the floor on `claim_duration` - insurance against a window that closes before participants can land a transaction; once the pool is fixed nobody can widen it, so such a value must never reach the state). All five insure against an input mistake: they are deliberately wider than any planned schedule and cut off only values beyond reason (a start years away, a sale decades long, a claim window of seconds).

### 9.1 `initialize`

- **Who:** the holder of the program's **upgrade authority** (Owner MS). The binding to an on-chain fact: the context includes `program: Program<Sale>` plus `program_data: Account<ProgramData>` with the check `program_data.upgrade_authority_address == Some(signer.key())` - this rules out hijacking the singleton's initialization. The deploy chain "deploy → SetAuthority to the Owner MS vault PDA → init through a proposal" is in TRD §9.3.
- **The arguments:** owner, admin, operator, price_usdc, emission, min/max_per_tx, start_date, end_date, claim_duration, claim_start.
- **It creates:** `SaleState` (`paused = false`, `pool_fixed = false`, the `deposits_usdc_contributed` / `participants` counters = 0, `pool_fixed_amount = 0`), `deposit_vault`, `token_vault`, `vault_authority`; and it stores `vault_authority_bump`.
- **Validation:** `now < start_date < end_date`; `start_date ≤ now + MAX_SALE_START_DELAY` (90 days); `end_date − start_date ≤ MAX_SALE_DURATION` (30 days); `MIN_CLAIM_DURATION ≤ claim_duration ≤ MAX_CLAIM_DURATION` (1 hour … 12 months); `max(end_date, now) < claim_start ≤ max(end_date, now) + MAX_CLAIM_START_DELAY` (30 days; here `now < end_date`, so the anchor is `end_date`); `token_mint.decimals == 8` and `usdc_mint.decimals == 6` (otherwise `InvalidMintDecimals` - the multiplier `token_units = 10⁸` and the USDC scale `10⁶` are baked into the constant and into `price_usdc`, §10.1); `0 < price_usdc < token_units` (rounding protection, §10.7); `emission > 0`; `0 < min ≤ max`; the mints differ; the roles ≠ `Pubkey::default()`.
- **The errors:** `Unauthorized`, `InvalidParams`, `InvalidAuthority` (a zero pubkey for owner/admin/operator), `InvalidMint`, `InvalidMintDecimals`, `InvalidDeadline`, `StartDateTooFar`, `SaleDurationTooLong`, `ClaimStartInPast`, `ClaimStartTooFar`, `ClaimDurationTooShort`, `ClaimDurationTooLong`, `MathOverflow`. The `Initialized` event.

### 9.2 The parameter instructions

Narrow instructions instead of a single `set_params` branching on roles - this gives an explicit, testable access matrix. The default values (BRD §3.2) are stored in `SaleState`.

| Instruction | Who | Conditions |
|---|---|---|
| `set_price(price_usdc)` | Admin | the PreSale phase; `0 < price_usdc < token_units` (§10.7) |
| `set_emission(e)` | Admin | the PreSale phase; `e > 0` |
| `set_tx_limits(min, max)` | Admin | the PreSale phase; `0 < min ≤ max` |
| `set_sale_schedule(start_date, end_date)` | Admin | the PreSale phase; `now < start_date < end_date`; `start_date ≤ now + MAX_SALE_START_DELAY` (90 days); `end_date − start_date ≤ MAX_SALE_DURATION` (30 days); `max(end_date, now) < claim_start ≤ max(end_date, now) + MAX_CLAIM_START_DELAY` (30 days; here `now < end_date`, so the anchor is `end_date`) |
| `set_claim_schedule(claim_start, claim_duration)` | Admin | PreSale, Sale or SaleEnded; `!pool_fixed`; `max(end_date, now) < claim_start ≤ max(end_date, now) + MAX_CLAIM_START_DELAY` (30 days); `MIN_CLAIM_DURATION ≤ claim_duration ≤ MAX_CLAIM_DURATION` (1 hour … 12 months) |

- Freezing the economics and the sale schedule from the start of Sale, and the irreversibility of that, are in §4.3.
- **The effect:** each instruction emits a dedicated event carrying the parameter's new values: `set_price` → `PriceUpdated{price_usdc}`, `set_emission` → `EmissionUpdated{emission}`, `set_tx_limits` → `TxLimitsUpdated{min_per_tx, max_per_tx}`, `set_sale_schedule` → `SaleScheduleUpdated{start_date, end_date}`, `set_claim_schedule` → `ClaimScheduleUpdated{claim_start, claim_duration}`. A privileged edit is recorded by the very fact of the event (G-12).
- The paired setters (`set_tx_limits`, `set_sale_schedule`, `set_claim_schedule`) set both values at once: when one field changes, the other is passed at its current value, and the instruction fully revalidates the resulting set on every call.
- **The errors:** `Unauthorized`, `SaleAlreadyStarted` (the price/emission/limits/dates with `now ≥ start_date`), `WrongPhase` (`set_claim_schedule` outside PreSale/Sale/SaleEnded), `PoolAlreadyFixed`, `InvalidParams`, `InvalidDeadline`, `ClaimStartInPast` (`claim_start` not ahead of both `end_date` and the moment of the edit) / `ClaimStartTooFar` (more than 30 days past the later of the two), `ClaimDurationTooShort` / `ClaimDurationTooLong` (the duration outside 1 hour … 12 months).

### 9.3 `pause` / `unpause`

Admin. `pause` - only in the Sale phase (`current_phase() == Sale`); `unpause` - in any phase, so that the flag cannot be locked in the set position (a pause not lifted before `end_date` would otherwise stay forever). The `paused` flag blocks **only** the purchase path (`init_aggregate` + `purchase`); it does not move the schedule and does not affect refund/claim/withdraw/sweep/close (TS-10). The `Paused` / `Unpaused` events. The errors: `Unauthorized`, `WrongPhase` (for `pause` only).

### 9.4 Ownership and the roles

`set_admin` / `set_operator` - Owner, at any time, in one step (G-4, including moving the operator role onto a multisig): each requires `signer == sale_state.owner` and a new pubkey `!= Pubkey::default()`, and immediately rewrites the corresponding field. The errors: `Unauthorized`, `InvalidAuthority` (a zero pubkey). The `RoleUpdated{role, new_pubkey}` event (the `role` field names the role that changed). Being one-step is safe here: the sitting Owner fixes a wrong Admin or Operator assignment by calling again.

**The Owner handover is two-step (G-3).** A one-step overwrite is inadmissible here: Owner is the top point of control, and writing a wrong address would take the rights away from the sitting key with no way to get them back.

- `propose_owner(new)` - Owner. Requires `signer == sale_state.owner` and `new != Pubkey::default()`, and records `pending_owner = Some(new)`. The `owner` field does not change: until confirmation every owner instruction is still executed by the sitting key, and the candidate holds no owner rights. Calling again replaces the candidate - the previous nominee loses the ability to accept the role. The errors: `Unauthorized`, `InvalidAuthority`. The `OwnerProposed{pending_owner}` event.
- `accept_owner()` - the nominated candidate, who is also the signer. Requires a non-empty `pending_owner` (otherwise `NoPendingOwner`) and `pending_owner == signer` (otherwise `Unauthorized`); it moves the value into `owner` and clears `pending_owner` to `None`. There is no role gate on the current owner here, and there cannot be - it is precisely the new key that confirms, and its signature is the proof of ownership. The `RoleUpdated{role: Owner, new_pubkey}` event.
- `cancel_owner_proposal()` - Owner. Requires `signer == sale_state.owner` and a non-empty `pending_owner` (otherwise `NoPendingOwner`); it sets `pending_owner = None`. It returns the state to "no handover" - without this instruction that state is unreachable after the first `propose_owner`, and the nominated key could take the role at any later moment. The one who nominated cancels: the candidate cannot withdraw themselves (`Unauthorized`). A cancel with no handover under way is rejected rather than passing silently. The errors: `Unauthorized`, `NoPendingOwner`. The `OwnerProposalCancelled{}` event (with no fields - the change is expressed by the fact itself).
- The cancel is a separate instruction rather than `propose_owner(None)`: nominating and withdrawing have different checks, different errors and different events, and a `None` in the argument alongside the already-rejected `Pubkey::default()` would give two spellings of an empty value in one place.
- **What this closes off:** an address with no private key behind it (as well as someone else's address) will never pass the second step. The handover stays unfinished, `owner` is unchanged, and the mistake is fixed with another `propose_owner` or withdrawn with `cancel_owner_proposal`. `pending_owner` is an `Option` rather than a zero key: "no handover" and "a handover to the zero address" are different states, and the second is rejected at the entry.

### 9.5 `init_aggregate`

- **Who:** the User (signer == wallet, who also pays the rent). It is a separate instruction because a conditional `init_if_needed` on an account holding state is forbidden (§13) and Anchor cannot express a conditional init/mut. The context: `sale_state` (mut), `user` (signer, payer), `aggregate` (init, seeds `[b"aggregate", user]`), `blacklist` (a mandatory slot bound by seeds), the System Program, event-cpi.
- **The preconditions:** the Sale phase; `!paused`; the wallet is on the Ed25519 curve (§6.5); the wallet is not in the Blacklist. A repeat call fails at `init` (`AlreadyInitialized`).
- **The Blacklist check** is the same mandatory seeds slot and the same owner check as in `purchase` (§9.6): a blocked wallet never creates an aggregate and never raises `participants`. The slot is mandatory here as well - an optional one would let the client control whether the check happens.
- **The effects:** `aggregate = {wallet, net: 0, deposit_count: 0}`; `participants += 1` (checked). The `AggregateInitialized` event.
- For the client: on the first contribution, send `init_aggregate` and `purchase` in a single transaction. The errors: `WrongPhase`, `SalePaused`, `WalletNotOnCurve`, `WalletBlacklisted`.

### 9.6 `purchase(amount)`

- **Who:** the User (signer == wallet). The context: `sale_state` (mut), `aggregate` (mut, `has_one`, must exist), `deposit` (init per §6.2, payer = user), `blacklist` and `graylist` (mandatory slots bound by seeds), `user_usdc_ata`, `deposit_vault` (mut), the Token/System programs, event-cpi.
- **The preconditions:** the Sale phase; `!paused`; **a duplicate `!pool_fixed` check** (insurance against a date shift, §4.2 - not a business condition of its own, TS-12); the wallet is on the Ed25519 curve (§6.5); the limits per §9.6a (Graylist when present, the global ones otherwise); the wallet is not in the Blacklist.
- **The Blacklist check is a mandatory `UncheckedAccount` bound by seeds (not optional):**
  ```rust
  /// CHECK: the address is pinned by the seeds constraint; data under the program id = blocked
  #[account(seeds = [b"blacklist", wallet.key().as_ref()], bump)]
  pub blacklist: UncheckedAccount<'info>,
  ```
  In the body: `require_keys_neq!(*blacklist.owner, crate::ID, WalletBlacklisted)`. The owner of the account at that address can only ever be one of two things: the System Program (no record) or the program itself (a record exists) - only the program can create an account at its own PDA, and `blacklist_remove` returns the owner to the System Program through `close`. The owner is therefore the single sufficient marker for the record's presence, the same one applied to the Graylist slot (§9.6a). `seeds`+`bump` on an `UncheckedAccount` work for a non-existent account too (Anchor checks the address through `find_program_address`, no data is needed). `Option<Account<Blacklist>>` is **forbidden**: the client controls whether an optional slot is present, and a blocked wallet would bypass the check simply by not passing the account. The slot is mandatory and the address is always derived from `wallet`.
- **The effects:** a CPI transfer user → `deposit_vault`; `aggregate.net += amount`; `deposit = {wallet, amount, index}`; `deposit_count += 1`; `deposits_usdc_contributed += amount` (all checked). The `Purchased{wallet, index, amount, net, total}` event (`total` = `deposits_usdc_contributed` after the purchase).
- **The errors:** `WrongPhase`, `SalePaused`, `PoolAlreadyFixed`, `WalletNotOnCurve` (the wallet is not on the curve, §6.5), `BelowMin` / `AboveMax`, `WalletTotalExceeded` (the Graylist `max_total`), `WalletBlacklisted`, `AccountNotInitialized` (there is no aggregate).

### 9.6a Applying the limits: Graylist or the global ones (I13)

The Graylist slot in `purchase` is built exactly like the Blacklist slot - a **mandatory** `UncheckedAccount` bound by seeds; an optional one is forbidden for the same reason (the client must not control whether the check happens):

```rust
/// CHECK: the address is pinned by the seeds constraint; data under the program id means individual limits
#[account(seeds = [b"graylist", wallet.key().as_ref()], bump)]
pub graylist: UncheckedAccount<'info>,
```

The limit-selection logic in the body:

```
if owner == program_id (a record exists):
    deserialize the Graylist (the discriminator is checked during deserialization)
    min, max := graylist.min_per_tx, graylist.max_per_tx          // they fully replace the global ones
    require!(aggregate.net.checked_add(amount)? <= graylist.max_total, WalletTotalExceeded)
else (owner == system_program, no record):
    min, max := sale_state.min_per_tx, sale_state.max_per_tx      // the global ones
require!(min <= amount && amount <= max, BelowMin / AboveMax)
```

- With a record present the limits come **only** from it; with none, only the global ones apply; there is no mixing.
- `max_total` is checked against `aggregate.net` (not against the deposit history): the aggregate bookkeeping is already reconciled with the deposits (I1), so there is no need to walk them.
- With a live record the slot is deserialized as a typed account, so the owner and the discriminator are checked structurally - a foreign account cannot be substituted under the seeds.

### 9.7 `blacklist_add(wallet)` / `blacklist_remove(wallet)`

Operator, at any time (TS-17). add: `init` (payer = authority); remove: `close = authority`. The `Blacklisted` / `BlacklistRemoved` events. The errors: `Unauthorized`. A repeated `blacklist_add` of the same wallet, and a `blacklist_remove` of a missing record, are rejected structurally (`init` / `close`); there are no dedicated codes.

**The wallet is an instruction argument (`wallet: Pubkey`), not passed as an account.** It is needed here only as the source of the record's seeds `[b"blacklist", wallet]`, and it must be possible to block any address - regardless of who owns that account or whether it exists at all (§6.5). The context: `sale_state`, `operator` (signer; the payer on add, the rent recipient on remove), `blacklist`, the System Program (on add only), event-cpi. The record's contents are that same `wallet`, so there is no "argument versus seeds" divergence: the seeds are derived from the argument, and a wrong slot is rejected by `ConstraintSeeds`.

### 9.7a `graylist_set(...)` / `graylist_remove(wallet)`

Admin, at any time (TS-15b). Instead of `init_if_needed` (forbidden on accounts holding state, §13) there are two narrow instructions grouped under the common name `graylist_set`:

- **creation** `(wallet, min_per_tx, max_per_tx, max_total)`: `init` of the record (payer = authority). A repeat call on an existing record is rejected structurally at `init`; there is no dedicated code.
- **update** `(wallet, min_per_tx, max_per_tx, max_total)`: overwriting the limits on an **existing** record (Graylist stores data rather than a "presence = right", so editing a field is correct; the record carries no counters, so there is nothing to lose).

Both validate `0 < min_per_tx ≤ max_per_tx ≤ max_total` (§5.6). `graylist_remove`: `close = authority` - the wallet goes back under the global limits.

**In all three the wallet is the instruction's first argument (`wallet: Pubkey`) and is not passed as an account** (for the same reason as in §9.7: it is the source of the seeds `[b"graylist", wallet]`, and it must be possible to set limits for any address, §6.5). The context: `sale_state`, `admin` (signer; the payer on creation, the rent recipient on removal), `graylist`, the System Program (on creation only), event-cpi.

The rent is paid by Admin / the project (the payer on set, the recipient on remove): the record is operational, not a user's. The `GraylistSet{wallet, min_per_tx, max_per_tx, max_total}` / `GraylistRemoved{wallet}` events. The errors: `Unauthorized`, `InvalidParams` (the ordering of the limits is violated).

### 9.8 `refund(index)`

- **Who:** the Operator (on an off-chain compliance decision).
- **The preconditions:** `!pool_fixed` (TS-21) and the `Sale` or `SaleEnded` phase; the deposit exists, and the addressing and the checks are in §6.4.
- **The context:** `sale_state` (mut, `has_one = operator`), `operator` (signer; the payer of the refund account), `wallet` (read-only `UncheckedAccount` - an address, §6.4), `deposit` (mut, seeds `[b"deposit", wallet, index]`, `has_one = wallet`, `close = refund_account`), `aggregate` (mut, seeds), `refund_account` (`init`, seeds `[b"refund_account", wallet, index]`, `token::mint = usdc_mint`, `token::authority = wallet`, payer = operator, §5.7), `deposit_vault` (mut, the address from `SaleState`), `vault_authority`, `usdc_mint`, the Token/System programs, event-cpi. 13 accounts, 6 writable; the refund account's `init` (`create_account` + `InitializeAccount3`) and one more PDA derivation add roughly 10k CU (the measurements are in `cu_bench`).
- **The effects:** the refund account is created (during account resolution, before the body); a CPI transfer `deposit_vault` → `refund_account` for `deposit.amount` (signed by the PDA); `aggregate.net −= amount`; `deposits_usdc_contributed −= amount`; the deposit is closed into `refund_account` → **its rent follows the USDC into the wallet's account** (TS-43). The `Refunded{wallet, index, amount, net, total}` event; the refund account's address is derived from `(wallet, index)`.
- **Why the destination is a fresh account (§5.7):** a forced refund must not depend on the state of any account the participant - or the USDC issuer - controls. The wallet's ATA would: its SPL owner is rotated by the wallet's own signature (`SetAuthority(AccountOwner)`), the account can be closed, and an existing account can be frozen by the issuer; a refund bound to the ATA would be rejected before the handler, and the participant would keep a rejected deposit in the pool at no cost, while the pool has to be fixed within a bounded time (§4.3). The refund account is born inside the refund, only the program can create it, a pre-funded address is completed rather than rejected, and the wallet account itself is neither read nor written - so a wallet under a foreign program (§6.5) or turned executable is refunded like any other. `claim` (§9.10) keeps the ATA with `init_if_needed`: it is self-serve, the wallet signs and pays, and blocking it would harm only the wallet itself.
- **Protecting the hot key:** the amount and the recipient are taken from the deposit itself (`deposit.amount`, `deposit.wallet`) rather than from the arguments, and the destination is the deposit's own PDA - the operator decides only whether to refund, never where. Compromising the operator only enables sabotage (mass refunds to legitimate wallets, each costing the project the refund account's rent), never theft; the operator holds no right over the account it paid for.
- The aggregate is **not** closed even at `net == 0` (§5.2, §6.2); a full refund zeroes `net` (TS-22).
- **The errors:** `Unauthorized`, `PoolAlreadyFixed` (after the fix, whatever the phase), `WrongPhase` (in `PreSale`); a closed or non-existent deposit → `AccountNotInitialized`; any account but the deposit's own refund PDA in the refund slot → `ConstraintSeeds`; a foreign mint → `ConstraintAddress`.

### 9.9 `activate_claim`

Atomically (TS-26): fixing the pool + the funding check (I12) + creating `proceeds_vault` (and `refund_vault` on oversubscription) and physically splitting the USDC. A failure at any step rolls back everything. The instruction fits entirely into one legacy transaction: in the worst case (oversubscription) it is the `init` of two new vaults plus two transfers out of `deposit_vault` signed by `vault_authority` (~13 accounts, ~5 writable); when undersubscribed it is the `init` of one vault and one transfer. An `init` completes during account resolution (before the handler body), so transferring into the just-created vaults inside the same instruction is correct.

- **Who:** Admin (a button in the backend's console); Admin is also the payer for the `init` of the new vaults.
- **The context:** `sale_state` (mut), `deposit_vault` (mut), `proceeds_vault` (`init`, seeds `[b"proceeds_vault"]`, `token::mint = usdc_mint`, `token::authority = vault_authority`), `refund_vault` (optional; `init` only in the oversubscription branch, seeds `[b"refund_vault"]`, the same `token::*`), `token_vault` (read, the I12 check), `vault_authority`, `usdc_mint`, the Token/System programs, event-cpi.

**The steps (in execution order):**

1. **The preconditions on the phase, on being one-shot and on the Claim window:** the SaleEnded phase; `!pool_fixed`; `now < claim_start` (otherwise `ClaimStartInPast`). Being one-shot is additionally guaranteed by the `init` of the new vaults (a repeat call fails with `AlreadyInitialized` before the body). The `now < claim_start` check is re-asserted here: the setters enforce it at the moment of the edit (§4.3), but the pool is fixed later, and `now` in this instruction is the real moment of the fix; fixing at `claim_start` or later would push the sale straight into Claim (part of the window lost) or into ClaimClosed (no window at all, with `sweep_*` already open).
2. **Token funding (I12):** `token_vault.amount ≥ distributable`, where `distributable` is computed right here from the final pool: oversubscribed (`pool_fixed_amount × token_units > emission × price_usdc`, u128) → `emission`; undersubscribed → `floor(pool_fixed_amount × token_units / price_usdc)` (≤ emission by the branch's construction, §10.5). The "undersubscribed / oversubscribed" boundary is a cross-multiplied comparison in u128 with no intermediate rounding.
3. **USDC funding (I2):** `deposit_vault.amount ≥ deposits_usdc_contributed`; otherwise `DepositVaultUnderfunded` (a separate code from step 2's token `VaultNotFunded` - so that each of the two funding gates is covered by an independent negative test). This carries invariant I2 into the moment of the fix: once the Sale side is fixed there must be no less money than has been accounted for.
4. **Writing the frozen pool:** `pool_fixed_amount = deposits_usdc_contributed` (once). From here on all of the math runs off `pool_fixed_amount`.
5. **Computing the USDC split** (the reserve must go entirely to the refund side):
   - `soft_cap = floor(emission × price_usdc / token_units)` (the monetary Soft Cap, §10.1; rounding down - in favour of the obligations);
   - `reserve = participants × 1 smallest USDC unit` (the reserve for per-wallet rounding, §10.7 / TS-40);
   - **Oversubscribed** (`pool_fixed_amount × token_units > emission × price_usdc`): `proceeds = soft_cap − reserve` (through `saturating_sub`: if the reserve exceeded the soft cap - unreachable for any sane `participants` - `proceeds = 0` rather than a revert); `refund = pool_fixed_amount − proceeds` (through `checked_sub`; `proceeds ≤ pool_fixed_amount` by construction, proof below);
   - **Undersubscribed** (`pool_fixed_amount × token_units ≤ emission × price_usdc`): there are no participant refunds → `proceeds = pool_fixed_amount`, `refund = 0`; `refund_vault` is **not created** (there is no point creating an empty vault that would immediately go into the sweep);
   - the identity `proceeds + refund == pool_fixed_amount` holds in both branches (degenerately when undersubscribed, with `refund = 0`; I14).
   - **Why the reserve sits on the refund side:** on oversubscription the per-wallet retention rounds up (`spent_i = ceil(...)`), and the total shortfall against an ideal `soft_cap` is up to `participants` smallest units (§10.7). By physically putting `proceeds = soft_cap − reserve` into `proceeds_vault` and everything else (`refund`, the reserve included) into `refund_vault`, the program guarantees that everything the refunds need sits in `refund_vault`, which `withdraw_proceeds` cannot reach.
6. **Creating the new vaults:** `proceeds_vault` is created **always**, `refund_vault` **only in the oversubscription branch** (where `refund > 0`). The `init` carries two structural meanings: activation being one-shot (a repeat fails with `AlreadyInitialized` already on `proceeds_vault`, which is always created) and "claim/withdraw require activation" - `withdraw_proceeds` declares `proceeds_vault` as a mandatory `Account<TokenAccount>`; with no activation the vault does not exist → `AccountNotInitialized` before the body (a structural gate, not a require check). In `claim`, `refund_vault` is an optional account, mandatory to pass only when the refund is > 0; it exists only on oversubscription - exactly where a participant refund is needed (§9.10).
7. **Writing the flag:** `pool_fixed = true`.
8. **The USDC transfers** (a `transfer` CPI signed by `vault_authority`, as the instruction's last actions, after every state write): oversubscribed - `deposit_vault → proceeds_vault` for `proceeds` and `deposit_vault → refund_vault` for `refund`; undersubscribed - only `deposit_vault → proceeds_vault` for the whole pool (`= pool_fixed_amount`).
9. **The event:** `ClaimActivated{pool_fixed_amount, distributable, proceeds_vault_amount, refund_vault_amount, claim_start, claim_deadline, price_usdc, emission}` (`claim_deadline` = `claim_start + claim_duration`; `proceeds_vault_amount = proceeds`, `refund_vault_amount = refund` - audit data, not stored on-chain, §10.5). The fields are enough to reconcile the mint, the schedule and the split off-chain.

- The instruction does **not** write the Claim schedule (`claim_start`, the duration) - that is set before the fix (§4.3). If the fix was delayed and `claim_start` has already arrived, the activation reverts; the normal way out is for Admin to move `claim_start` forward with `set_claim_schedule` (available while `!pool_fixed`) to any moment within `MAX_CLAIM_START_DELAY` of the edit (§4.3) and activate again; the way out does not expire, because the ceiling follows the edit rather than the sale's end.
- **Bookkeeping by state (reload-after-CPI):** every decision (the split, the checks) is taken from the state's fields (`deposits_usdc_contributed` / `pool_fixed_amount` / `participants`), not from a vault's balance; the transfers come last, after every write. After a CPI, vault balances are never read for a decision.
- **The proof that `proceeds ≤ pool_fixed_amount`** (so that `refund = pool_fixed_amount − proceeds` cannot panic): undersubscribed → `proceeds = pool_fixed_amount`, `refund = 0`; oversubscribed → the branch is chosen when `pool_fixed_amount > soft_cap`, and `proceeds = soft_cap − reserve ≤ soft_cap < pool_fixed_amount`. In the code `refund` is computed with `checked_sub` and an explicit error - insurance for hypothetical edges.
- **Why `≥` and not `==`** (token funding): a check for exact balance equality would let anyone block the activation by sending 1 extra unit to the vault; with `≥` a stray transfer is harmless and the excess goes into the sweep. `distributable` is not stored - it is recomputed at claim time.
- **The Claim gates are `pool_fixed`:** the Claim phase arrives only with a fixed pool (`pool_fixed && claim_start ≤ now < deadline`). While the pool is not fixed the claim is unavailable even if `claim_start` has already arrived (§4.2). The same unavailability is also enforced by the non-existent vaults the claim instructions work with. It is precisely the claim activation that initializes the vault.
- **The errors:** `WrongPhase`, `PoolAlreadyFixed`, `ClaimStartInPast` (`now ≥ claim_start` at the moment of the fix, step 1), `VaultNotFunded` (tokens, step 2), `DepositVaultUnderfunded` (USDC, step 3), `RefundVaultMismatch` (`refund_vault` is passed exactly when the pool is oversubscribed and not passed when it is undersubscribed: `oversubscribed == refund_vault.is_some()`), `Unauthorized`.

### 9.9a `assert_funded`

A funding check on `token_vault`. The instruction changes nothing: it either passes or reverts the transaction it is part of.

- **Who:** anyone - the context has no `Signer` slot and no role is checked (§8). The instruction grants no new rights: it reads only the same public accounts as `claim`.
- **The condition:** none. The instruction checks neither the phase nor the time - the funding is verifiable at any moment, including before the sale starts.
- **The context, the slots in this order:** `sale_state` (seeds `[b"sale_state"]`, `bump`), `token_vault` (`address = sale_state.token_vault`). Not a single `mut`, not a single `Signer`, no `#[event_cpi]` helper slots (the instruction emits no events), and `remaining_accounts` are not read. The mint and the vault's owner are not supplied as separate slots: the vault's address is pinned by a state field, and those properties themselves were fixed when the vault was created at `initialize` (`token::mint = token_mint`, `token::authority = vault_authority`, §9.1).
- **The effects:** none - no state changes and no lamports move.
- **Validation:** `token_vault.amount ≥ sale_state.emission`, otherwise `VaultNotFunded`. The reference is taken from the stored field, so editing the emission (`set_emission`, available before the sale starts) moves the check's boundary along with it.
- **Why the reference is the whole emission and not `distributable`.** `distributable ≤ emission` in both branches (§10.5), and at the time of the issuance transaction the pool is not fixed yet and `distributable` is undefined. "The whole sale emission is in the vault" is the stronger statement, it does not depend on the pool, and it matches the volume that TRD §4 issues into `token_vault` in one go.
- **The window of applicability is before the first payouts.** Once the payouts begin, `token_vault` is drawn down by claims and the check stops passing on a fully funded sale: the program keeps no counter of tokens paid out (§5.1), so the undrawn remainder of the obligation is not derivable on-chain. This is a deliberate boundary of the instruction: the funding at activation is checked by `activate_claim` (I12, step 2), the sufficiency on each payout by the vault's physical balance (I3), and `assert_funded` closes the one moment where neither check stands - the revoking of the right to issue.
- **Its purpose:** the instruction is included in the atomic issuance transaction (TRD §4) between `mint_to` and the closing `SetAuthority(None)`, next to the checks on each vesting activity. An underfunded vault brings the whole transaction down, so the irreversible state "the right to issue was revoked while the sale's vault was underfunded" never arises: before the revoke an under-mint is fixable by topping up, after it is not.
- **The errors:** `VaultNotFunded`.
- **The event:** none - the instruction changes no state and the indexer has nothing to record (§12).

**`VaultNotFunded` serves two funding gates - the merging is deliberate.** The same variant is returned by the activation gate (`token_vault.amount ≥ distributable`, I12, §9.9) and by the issuance gate (`token_vault.amount ≥ emission`, §9.9a). The meaning of the violation is told apart by the instruction's context: in `activate_claim` it is "there is nothing to switch Claim on with", in `assert_funded` it is "the sale's emission has not been collected in the vault". Operationally they are told apart by the index of the failing instruction in the transaction.

### 9.10 `claim(wallet)`

The single claim instruction: atomically, in one transaction, it hands out the tokens, refunds the USDC (if any) and closes the aggregate. There are no other claim formats, and there is no way to finish a claim on the user's behalf.

- **Who:** only the User (`signer == aggregate.wallet`): the claim is made by the contribution's own owner and there is no instruction to complete it for them. **The recipient binding:** the ATAs are bound by `associated_token::authority = aggregate.wallet`; the rent for creating an ATA is paid and signed by the wallet itself (which is also the `payer`), and the rent from closing the aggregate goes back to `wallet`. The ATA with `init_if_needed` is admissible here precisely because the claim is self-serve: the wallet signs and pays, and a rotated or closed ATA blocks nobody but the wallet itself - the forced refund of §9.8 cannot rely on that and creates its own destination (§5.7).
- **The preconditions:** the Claim phase (`pool_fixed && claim_start ≤ now < claim_deadline`); `aggregate.net > 0` (otherwise `NothingToClaim`).
- **The accounts:** `sale_state`, `aggregate` (mut, `has_one = wallet`, `close = wallet`), `token_vault` (mut), the wallet's $1WIN ATA (mut, `init_if_needed`), `vault_authority`, the mints, the Token/ATA/System programs, event-cpi; `refund_vault` and the wallet's USDC ATA (mut, `init_if_needed`) are **optional** accounts, passed if and only if `refund > 0`. The instruction creates the wallet's ATAs itself if they do not exist (`init_if_needed`, payer = wallet), bound by `associated_token::authority = wallet`. The instruction does **not** create `refund_vault` (it is a program vault that exists from activation): it is bound by the seeds `[b"refund_vault"]` + `token::mint = usdc_mint` + `token::authority = vault_authority`; when undersubscribed it does not exist and cannot be passed.
- **The effects** (decisions from the pre-CPI state, transfers last, in order): `Allocation { tokens, refund } = allocate(net, pool_fixed_amount, emission, price_usdc)` (§10, a single function; the quantities are not stored). Then:
  1. a CPI transfer `token_vault` → the $1WIN ATA for `tokens` (a transfer of 0 is allowed);
  2. when `refund > 0`: `refund_vault` and the USDC ATA **must** be present (fail-closed: `RefundVaultMissing` / `RefundAtaMissing` if they are not), then a CPI transfer `refund_vault` → the USDC ATA for `refund`;
  3. the aggregate is closed **unconditionally** (`close = wallet`, the rent → `wallet`).
  `claim` does **not** touch the wallet's deposits (they are closed by the batch of §9.13). The closed aggregate is the only protection against a repeat (I5): a repeated `claim` finds no aggregate → `AccountNotInitialized`. The `Claimed{wallet, net, tokens, refund}` event.
- A refund not drawn before the deadline stays in `refund_vault` → `sweep_unclaimed` (TS-36); unclaimed aggregates are closed by `close_aggregates`.
- **A wallet under a foreign program** (§6.5) claims on the same terms as anyone else: a signature does not depend on the account's owner. The only limitation is paying the rent: `payer = wallet` works only for a System-owned account, so such a wallet needs its ATAs to exist already. Creating an ATA is permissionless and can be done by anyone in a separate transaction, so this is a question of ordering, not of losing access to the tokens.
- **The errors:** `WrongPhase` / `ClaimDeadlinePassed`, `NothingToClaim`, `RefundVaultMissing`, `RefundAtaMissing`; a closed aggregate → `AccountNotInitialized`.

### 9.12 `withdraw_proceeds`

- **Who:** Owner, only from the start of Claim: `pool_fixed && now ≥ claim_start` (TS-39). It is available after the deadline as well (in ClaimClosed): Owner takes the proceeds at any moment of Claim/ClaimClosed.
- **The context:** `sale_state`, `proceeds_vault` (mut, a mandatory `Account<TokenAccount>`, bound by the seeds `[b"proceeds_vault"]` + `token::mint` / `token::authority`; before activation it does not exist → `AccountNotInitialized`), Owner's ATA, `vault_authority`, the rent-collector address (the rent recipient when the vault is closed, passed as an account - §5.5), the Token Program, event-cpi. There is no amount argument.
- **The effect - one call, the whole balance, drain → close:** a CPI transfer of the entire `proceeds_vault` balance to Owner's ATA, then closing the vault with the SPL `CloseAccount` signed by `vault_authority` (the rent → the rent-collector address passed in). Limit I9 is purely **physical**: more than is in the vault cannot be withdrawn by construction; there is no separate withdrawn-amount counter and no arithmetic limit check.
- **What is in the vault:** oversubscribed → `soft_cap − reserve` (the reserve is held in `refund_vault`); undersubscribed → `pool_fixed_amount` (there are no refunds and no reserve is held). In both branches the proceeds are withdrawable in full.
- **A repeat is impossible:** after the close the vault does not exist → a repeat call fails with `AccountNotInitialized`.
- **`proceeds_vault` is never swept:** it holds no remainder that Owner cannot reach; by the deadline it is already closed (see §9.14).
- **The event:** `ProceedsWithdrawn{amount}` (`amount` = the balance withdrawn).
- **The errors:** `Unauthorized`, `PoolNotFixed`, `ClaimNotStarted` (`now < claim_start`).

### 9.13 `close_deposits` / `close_aggregates` (batch)

Returning the rent to its user-owners in batches (TS-43); executed by the Operator. Both instructions share one window - the ClaimClosed phase (`pool_fixed && now ≥ claim_deadline`); the check is the same one and a separate `pool_fixed` condition is unnecessary (ClaimClosed is reachable only with a fixed pool).

- **`close_deposits`** - from the pool fix onwards a deposit becomes an immutable archive (a `refund` is already impossible), but it is closed at the terminal stage, together with the other accounts.
- **`close_aggregates`** - before the deadline an aggregate is a live right to claim and must not be closed. By the deadline the only aggregates left for the batch are those of wallets that never claimed (an aggregate that did claim was already closed by `claim`). The deadline separates the windows strictly: `claim` requires `now < deadline` and the batch requires `now ≥ deadline`, so there is no race.

**The `SaleState` counters (`pool_fixed_amount`, `participants`) do not change on a close:** `pool_fixed_amount` has been frozen since activation and remains the allocation denominator for those who have not claimed yet; a batch close returns only the rent and does not touch the bookkeeping. After the fix, `pool_fixed_amount` is a snapshot of the pool, not a live counter.

The accounts being closed and their wallets are passed in `remaining_accounts` in **pairs** `[acc₁, wallet₁, …]`; Anchor's automatic checks do not apply to them and the validation is entirely manual. **6 mandatory checks per pair:**

1. `account.owner == program_id`;
2. the account's type (Anchor's 8-byte discriminator) - strictly `Deposit` or `WalletAggregate`; closing `SaleState`, `Blacklist` or `Graylist` is not allowed;
3. re-deriving the **canonical** PDA from the seeds (`wallet` / `index` are taken **from the account's body**) through `find_program_address` and comparing it with the address passed: this cuts off both a forgery with a correct discriminator and a record at a non-canonical address (§6.3);
4. the account is "alive" (lamports > 0, the data is not zeroed);
5. the paired recipient: `paired_wallet.key() == body.wallet` (the rent goes strictly to the owner, not to an account from untrusted input);
6. the manual close: the lamports → the paired wallet, the data zeroed, the account reassigned to the System Program.

The limit is ≤ 20–25 closes per call (bounded by CU and the transaction size; the pairs double the account count; the exact figure follows a measurement). The `AccountsClosed{count, rent_returned}` event. The errors (shared by both instructions): `Unauthorized`, `DeadlineNotPassed`, `InvalidAccountInBatch`, `BatchTooLarge`.

### 9.14 `sweep_unclaimed` (one vault at a time)

The sweep is a set of **separate idempotent instructions, one per vault** (`refund_vault`, `deposit_vault`, `token_vault`), not one monolith over every vault. The reason is idempotency: a monolith that also closes accounts is not idempotent - a repeat call would hit `AccountNotInitialized` on an already-closed vault and roll the whole batch back, jamming the remaining vaults after a partially successful first run.

- **Who:** Owner. **The preconditions (per instruction):** the ClaimClosed phase (`pool_fixed && now ≥ claim_deadline`). While Claim has not been activated the deadline does not exist → the instruction is structurally unavailable: withdrawing the funds without letting participants claim is impossible (TS-29).
- **The context (per instruction):** `sale_state`, its own vault (mut), Owner's ATA (for the remainder), `vault_authority`, the rent-collector address (the recipient of the closing rent - §5.5), the Token Program, event-cpi.
- **The effect (per vault): drain-if-nonzero → close.** A transfer of the non-zero remainder to Owner's ATA, then closing the vault with the SPL `CloseAccount` signed by `vault_authority` (the rent → the rent-collector address passed in).
- **The sweep's sources:** `refund_vault` (the refunds of wallets that never claimed, plus what is left of the rounding reserve - **only on oversubscription**; when undersubscribed it was never created and the `refund_vault` sweep instruction is inapplicable), `deposit_vault` (stray transfers/donations that the program never accounted for - the bookkeeping runs off `deposits_usdc_contributed`, not off the balance), `token_vault` (the tokens never handed out). **`proceeds_vault` is not swept** - Owner withdraws the proceeds in full themselves (§9.12), and by the deadline it is already closed.
- **Independent idempotency:** each instruction is gated by the ClaimClosed phase; a repeat call on one vault is a clean revert on the already-closed account and does not affect the other vaults.
- **The event:** `Swept{vault, amount}` per vault.
- **The errors:** `Unauthorized`, `DeadlineNotPassed`.

---

## 10. The allocation math

### 10.1 The scales and the price

- USDC has 6 decimal places (1 USDC = 10⁶ units); the `10⁶` scale is baked into `price_usdc`, which is why `initialize` checks `usdc_mint.decimals == 6`.
- $1WIN has 8 (the emission of 130M = 1.3×10¹⁶ token units).
- The price `price_usdc` (u64) is USDC units per ONE whole $1WIN token; for $0.03 → `price_usdc = 30_000`.
- The multiplier `token_units = 10⁸` (= 100,000,000) - the token units in one whole $1WIN - is a **fixed constant** ($1WIN has exactly 8 decimals). To keep the constant in step with the mint, `initialize` checks `token_mint.decimals == 8` (otherwise `InvalidMintDecimals`); the decimals themselves are not stored separately.

From which:
- The tokens for a contribution: `tokens_for(usdc) = floor(usdc × token_units / price_usdc)` (rounding **down**).
- The monetary Soft Cap - the ceiling on the protocol's proceeds: `soft_cap_usdc() = floor(emission × price_usdc / token_units)` (= $3.9M at the default values).

`token_units` is set by the mint's decimals (not a free parameter), so the price has exactly one tunable scalar, and its value is verifiable by the multisig (`30000 = $0.03 × 10⁶`).

### 10.2 The formulas

The values are u64 and the intermediates are u128. A single `allocate()` function is used by the `claim` instruction.

**Undersubscribed (`pool × token_units ≤ emission × price_usdc`):**
```
tokens = floor(net × token_units / price_usdc);   refund = 0
```

**Oversubscribed (`pool × token_units > emission × price_usdc`):**
```
tokens = floor(net × emission / pool)                 // rounding DOWN
spent  = ceil (tokens × price_usdc / token_units)     // the retention rounds UP
refund = net.checked_sub(spent)                       // spent ≤ net - the proof is below
```

**The proof that `spent ≤ net`** (the guarantee that `checked_sub` cannot panic and that tokens are always handed out): when oversubscribed, `emission/pool < token_units/price_usdc`, hence `tokens ≤ net × emission / pool < net × token_units / price_usdc`, from which `tokens × price_usdc / token_units < net`; and since `net` is an integer, `ceil(...) ≤ net`. The defensive check in the code is mandatory anyway: `refund = net.checked_sub(spent).unwrap_or(0)` - insurance for hypothetical edges, unreachable at the normal parameters.

### 10.3 The rounding directions - in the protocol's favour (TS-33)

| Quantity | Direction | Why |
|---|---|---|
| `tokens` | down (floor) | the payout ≤ the share; the vault ≥ 0 (I3) |
| `spent` | up (ceil) | the retention ≥ the cost; `refund ≤ net` (I4) |
| `soft_cap_usdc` | down (floor) | the proceeds are understated - in favour of the obligations (I9) |

The rounding remainder stays in the vaults → it is withdrawn by the sweep instruction after the deadline.

### 10.4 Overflow

The products (`net × emission`, `net × token_units`, `total × token_units`, `emission × price_usdc`) are computed in u128 (max ~10²⁹ ≪ the u128 limit); everything goes through `checked_*`; the u128 → u64 downcast is explicitly checked. Division by zero is ruled out by the preconditions: `pool > 0` when oversubscribed; `price_usdc > 0` from init; `token_units = 10⁸` is a non-zero constant.

### 10.5 The Soft Cap boundary - a cross-multiplied comparison

The branch is chosen by the comparison **`pool × token_units ⋛ emission × price_usdc`** (u128) - which is exactly `pool ⋛ emission × price` with no intermediate rounding, so there are no edge defects from a stored or rounded threshold. In the undersubscribed branch the condition implies `floor(pool × token_units / price_usdc) ≤ emission`, and `Σ floor(net_i × …) ≤ floor(pool × …)` (the sum of the floored shares does not exceed the floored sum) → the total payout never exceeds the emission **by construction**. The same comparison and the same `distributable` formula are used in `activate_claim` (§9.9).

**The USDC split invariant (I14).** At activation the USDC is physically split into `proceeds` and `refund` (§9.9, step 5) such that `proceeds + refund == pool_fixed_amount`. The quantities `proceeds_vault_amount` / `refund_vault_amount` are **not stored** on-chain: no instruction reads them to take a decision (`claim` / `withdraw_proceeds` compute from `pool_fixed_amount`, and `sweep_unclaimed` takes the vault's remainder in the terminal phase), so by the minimal-onchain-footprint rule they are emitted in the `ClaimActivated` event. The invariant is verified from the event plus the balances measured right after activation: oversubscribed - `proceeds_vault.amount == proceeds_vault_amount` and `refund_vault.amount == refund_vault_amount`; undersubscribed - `proceeds_vault.amount == pool_fixed_amount` and `refund_vault` does not exist (`refund_vault_amount == 0`). There are no stored fields; stray transfers to a vault's address are harmless and take no part in the calculation.

### 10.6 The edge cases

| Case | Behaviour |
|---|---|
| `net == 0` | `claim` → `NothingToClaim`; the aggregate lives until the deadline (§5.2). |
| `tokens == 0` (extreme oversubscription, `net × emission / pool < 1`) | `spent = 0`, `refund = net` - a full refund; a transfer of 0 tokens is allowed; `claim` returns the whole net and closes the aggregate. |
| a single participant, `net == pool` (oversubscribed) | `tokens = emission`, `spent = soft_cap`, `refund = net − soft_cap`. |
| the boundary `pool × token_units == emission × price_usdc` | the undersubscribed branch; Σ of the payouts ≤ emission, and the rounding remainder → the sweep. |
| `pool == 0` | activation is formally possible (`distributable = 0`, the `≥` check passes); there is no one to claim; a degenerate business scenario. |

The worked numeric examples are at the end of this document.

### 10.7 The sufficiency of the rounding reserve (I9)

**Where the remainder comes from.** Every quantity is an integer; a rounded division (`tokens` down, `spent` up, §10.3) discards the fractional part. Per wallet the discrepancy is < 1 smallest unit, and across N participants it is up to N units. The reserve compensates for exactly that accumulation.

**The claim.** A reserve of `participants × 1 smallest unit` guarantees that every refund is covered after Owner has withdrawn the full limit (TS-40). *Undersubscribed:* there are no refunds and any withdrawal ≤ pool is safe. *Oversubscribed* (`min = soft_cap`): against the "ideal" retention `s_i = net_i × soft_cap / pool` (whose sum is exactly soft_cap), each wallet loses < 1 token unit to rounding, and `spent_i > s_i − price_usdc/token_units`, from which the total shortfall is `soft_cap − Σ spent_i < N × price_usdc/token_units = N × 0.0003` USDC units at a price of $0.03 (`30000/10⁸ = 0.0003`). A reserve of `N × 1 unit` covers that with a **~3333×** margin (an example: 100,000 wallets → a reserve of 100,000 units = $0.1 against a need of < 100,000 × 0.0003 = 30 units = $0.00003). Every `claim` with a refund reduces both the vault and the obligation by the same amount → the invariant holds across the whole interval. A growing `participants` only increases the margin.

**The applicability condition of the "1 unit × wallet" reserve:** the per-wallet shortfall is < `price_usdc/token_units`, i.e. 1 unit suffices as long as 1 token unit costs < 1 USDC unit (`price_usdc < token_units`; for decimals 8/6 that is a price < $100 per token). The single scalar bound `0 < price_usdc < token_units` is validated in `initialize` / `set_price` (§9.1/§9.2), so the TS-40 formula stays exact at any admissible price.

**Where the reserve physically sits.** The reserve is physically allocated into `refund_vault`, which `withdraw_proceeds` cannot reach (the split mechanism is in §9.9, step 5). The maximum proceeds withdrawal from `proceeds_vault` therefore never reduces what is reserved for the refunds - and that is invariant I4 (§3); the `dust_reserve_sufficient` proptest confirms that the total rounding shortfall is strictly less than the `participants` reserve.

**The reserve is a conservative overestimate (a requirement).** The reserve is taken as `participants` rather than as the exact per-wallet shortfall. `participants` is monotonically non-decreasing by construction (§5.1): the counter grows in `init_aggregate` and decreases neither on a `refund` nor when aggregates are closed. The overestimate is admissible and mandatory: overstating the reserve only increases `refund_vault` (the refund side) and therefore preserves the refunds' solvency at any value of `participants`; the excess reserve left after every claim is taken for the project through `sweep_unclaimed` (§9.14), so no funds are lost. `init_aggregate` is open to any unblocked wallet and requires no paid contribution, so `participants` may be overstated relative to the number of wallets that actually claim. The only consequence of an overstated `participants` is that on oversubscription `proceeds = soft_cap − participants` (through `saturating_sub`) shrinks, down to `proceeds = 0` when `participants ≥ soft_cap` in the smallest USDC units. That consequence is accepted: the solvency of the refunds and the claims is preserved at any `participants`, the whole amount (including at `proceeds = 0`) stays in `refund_vault` and is retrieved through `sweep_unclaimed` after the deadline; a smaller amount in `proceeds_vault` delays the project's withdrawal, it does not lose funds. The value of `participants` at which `proceeds` shrinks noticeably is unreachable within any sane bounds: the soft cap is 3,900,000,000,000 smallest USDC units, and reducing `proceeds` by each unit requires a separate rent-paid aggregate.

---

## 11. Executing a claim

A claim is executed by **a single instruction**, `claim` (§9.10), in one legacy transaction: handing out the tokens, refunding the USDC (when the refund is > 0) and closing the aggregate - atomically. There is no separate execution format: the order "tokens → refund → close" is forced by construction inside the instruction, and the intermediate state "tokens handed out, refund not received" does not exist.

- **The account profile:** ~15 accounts (every data account is Boxed so as not to overflow the stack), including the two `init_if_needed` ATAs - $1WIN and USDC - which are paid for on a wallet's first claim. When the refund == 0 (always when undersubscribed; when oversubscribed, whenever `spent == net`) `refund_vault` and the USDC ATA are not passed. It fits inside a legacy transaction; should the account count grow, an Address Lookup Table or pre-created ATAs are the answer.
- **The compute budget:** a single instruction sits well below the 1.4M CU limit (a multiple-times margin). Re-entrancy does not exist as a class on Solana: a program cannot appear twice in the CPI stack.

---

## 12. The events

| Event | Instruction | Fields |
|---|---|---|
| `Initialized` | initialize | the roles, the mints, price_usdc, emission, min/max, start/end_date, claim_duration, claim_start |
| `PriceUpdated` | set_price | price_usdc |
| `EmissionUpdated` | set_emission | emission |
| `TxLimitsUpdated` | set_tx_limits | min_per_tx, max_per_tx |
| `SaleScheduleUpdated` | set_sale_schedule | start_date, end_date |
| `ClaimScheduleUpdated` | set_claim_schedule | claim_start, claim_duration |
| `OwnerProposed` | propose_owner | pending_owner |
| `OwnerProposalCancelled` | cancel_owner_proposal | (no fields) |
| `RoleUpdated` | accept_owner / set_admin / set_operator | role, new_pubkey |
| `Paused` / `Unpaused` | pause / unpause | (no fields) |
| `AggregateInitialized` | init_aggregate | wallet |
| `Purchased` | purchase | wallet, index, amount, net, total |
| `Refunded` | refund | wallet, index, amount, net, total |
| `Blacklisted` / `BlacklistRemoved` | blacklist_add / remove | wallet |
| `GraylistSet` / `GraylistRemoved` | graylist_set / remove | wallet, min_per_tx, max_per_tx, max_total (for `GraylistSet`) |
| `ClaimActivated` | activate_claim | pool_fixed_amount, distributable, proceeds_vault_amount, refund_vault_amount, claim_start, claim_deadline, price_usdc, emission |
| `Claimed` | claim | wallet, net, tokens, refund |
| `ProceedsWithdrawn` | withdraw_proceeds | amount |
| `Swept` | sweep_unclaimed (per balance) | vault, amount |
| `AccountsClosed` | close_deposits / aggregates | count, rent_returned |

The cumulative fields in the events (`net`, `total` in `Purchased` / `Refunded`) are the counters' values after the transaction. There is one fewer event than there are instructions: `assert_funded` (§9.9a) changes no state and therefore emits none.

The audit trail for the landing page's indexer is built from the events (TS-14, G-12). Use **`#[event_cpi]` + `emit_cpi!`** (the event is written as a nested instruction): providers truncate ordinary program logs but not nested instructions; the cost is ~1–2k CU. The events are an auxiliary layer: monetary correctness is held by the accounts and the invariants of §3, and losing an event does not break it.

---

## 13. Security

### 13.1 The map of defences

Every threat is closed structurally - by an invariant from §3 or by a mechanism in the section named; the details are behind the link.

| Threat | Where it is closed |
|---|---|
| A double claim / refund | I5 (the repeat protection is the already-closed account) |
| Handing out more than the emission / overdrawing `token_vault` | I3, I12, §9.9 |
| Withdrawing proceeds beyond the limit | I9, §9.12 (a physical boundary: the whole `proceeds_vault`, with no counter) |
| A bug in one money path touching another vault | §7, I4, I9, I14 (the physical USDC split; the proceeds and the refunds never meet in a single context) |
| Arithmetic overflow | I7 (u128, `checked_*`, the roundings of §10.3) |
| Bypassing the purchase limits (the Graylist `min` / `max` / `max_total`, not supplying the slot) | I13, §9.6a |
| Bypassing the Blacklist by not supplying the slot | §9.5, §9.6 - a mandatory seeds slot in both instructions of the deposit path; optional is forbidden |
| A program wallet (a multisig / custody) taking part in the sale | §6.5 - the check that the address lies on the Ed25519 curve at the entry of `init_aggregate` and `purchase`; a signature check and an account-owner check do not give this on their own |
| A wallet escaping a ban, the limits and a refund by changing its account's owner | §6.5 - the lists and the refund address the wallet as an address (an instruction argument / an `UncheckedAccount` bound by seeds) and never read the account's owner, so an `assign` on the wallet changes nothing for the operator; the wallet's type is pinned at the entry, where the property being checked is immutable; `refund` neither reads nor writes the wallet account at all (§9.8) |
| A participant blocking a forced refund through its destination: rotating the ATA's SPL owner (`SetAuthority(AccountOwner)`), closing the ATA, an issuer freeze of an existing account, pre-funding the refund account's address, turning the wallet into an executable account | §5.7, §9.8 - the refund creates a fresh account at its own PDA inside the instruction and sends the USDC and the deposit's rent there; no pre-existing account of the participant's is read or written, only the program can create an account at that address, and a pre-funded address is completed rather than rejected |
| Substituting a Blacklist / Graylist record | §9.6a - typed deserialization (owner + discriminator + bump) |
| Substituting a vault / an ATA / a refund account / an aggregate | §7 - `seeds`+`bump`, `token::mint` / `authority`, `has_one`, the address from `SaleState`; the recipients are not the signer, and the refund's destination is the deposit's own PDA (§5.7) |
| A missing signer / owner check | §8 - `signer == sale_state.<role>` + Signer; `initialize` - the upgrade authority only (§9.1) |
| Redirecting funds / rent | §9.8, §9.10 - the amounts and the recipients come from the program's accounts, not from the arguments; compromising the operator ⇒ sabotage, not theft. The rent from closing a vault goes to an address passed by the trusted Owner (§5.5) |
| Address reuse / a repeated init | §6.2 (`deposit_count` only grows), §5.2; `init_if_needed` is forbidden on accounts holding state; the refund account is created with `init` and its seeds carry the deposit's index (§5.7) |
| Attacks during the batch close | §9.13 - 6 manual checks; state accounts and vaults cannot be closed |
| Computed phases drifting out of sync | §4.1 - a single `current_phase()` |
| Manipulating a vault's balance | §7, §9.9 - the bookkeeping runs off the stored parameters; the activation checks `≥`, not `==` |
| Seizing an authority | §8, §9.4 - only the current Owner nominates a candidate, and the candidate takes the role with their own signature; mint/freeze live outside the program |
| DoS: spamming deposits / empty aggregates | `min_per_tx`; empty aggregates are closed in a batch and the reserve is recoverable through the sweep |
| DoS: an expensive batch | §9.13 (the close limit) |
| A takeover through a zeroed discriminator | Anchor 1.x forbids all-zero discriminators; state accounts are never initialized by hand |

### 13.2 The accepted residual risks

| Risk | What it is | Acceptance / mitigation |
|---|---|---|
| A premature activation inside the AML window | Activation requires only SaleEnded + a funded vault: an early mint and an activation right at `end_date` collapse the AML window - risky deposits are then frozen into the pool irreversibly. | An on-chain guard is impossible (the program does not know that AML has finished). Operationally: the mint is not done in advance (the recommendation is the mint + the activation in a single multisig proposal, §7), and the compliance settlement is confirmed off-chain before the activation. |
| Purchases serialized on `total` | Every `purchase` writes `SaleState.total` → concurrent purchases execute sequentially; at a peak influx retries or timeout failures are possible. | Accepted (§5): the sale's expected flow is far from the throughput ceiling of a hot account; a measurement under load belongs to a test iteration; sharding the counter only makes sense with a business-confirmed peak flow of hundreds of purchases per second. |
| A risky deposit after the fix | USDC cannot be recalled on-chain; the right to Claim is preserved. | TS-23: the settlement is final; the emergency measure is freezing the token account (outside the program); the reliance is on the AML window (TS-24). |
| Tight Graylist limits | Setting tight limits (a high `min` / a low `max_total`) blocks a wallet's legitimate purchases. | The Graylist is maintained by the Admin multisig (not a hot key). The validation `min ≤ max ≤ max_total` (§5.6) rules out an "unreadable" record; it is reversible through `graylist_remove`; Owner rotates Admin (`set_admin`). |
| An under-mint / over-mint of the emission | The exact mint amount is not verified. | I12 (`≥`) blocks an under-mint at activation; an over-mint is harmless (the excess → the sweep); the off-chain reconciliation goes off `ClaimActivated`. |
| An indefinite SaleEnded if the Admin key is lost | `activate_claim` / `pause` are Admin-only (there is no Owner fallback by design); with no activation the funds are locked for everyone. | This is an availability risk, not a theft risk. The recovery: Owner rotates Admin (`set_admin`) onto a working Ops MS key, after which the activation goes through. Owner cannot activate Claim directly - the separation of duties is preserved; this is covered by the operational procedure. |
| Compromising the operator | A mass blacklist (a reversible DoS), a mass refund before the fix (sabotage). | The structural bindings rule out theft (§9.8); Owner rotates the operator (G-4). |
| The rent of the refund accounts | Every forced refund costs the project the rent of a 165 B token account (~0.002 SOL, paid by the operator) - always, not only when the wallet's ATA is missing - and the program can never take it back: the account is the participant's, who alone may close it (§5.7). | Accepted: a per-refund operational cost bounded by the number of compliance refunds; the alternative - a destination that already exists - is exactly what a participant can sabotage (§9.8). |
| The USDC issuer (Circle) freezing funds | USDC is an administered stablecoin: the issuer can technically freeze any account, the Sale USDC vaults included (`deposit_vault` / `proceeds_vault` / `refund_vault`). This is outside the protocol's control - a property of the chosen asset. | Not structurally reducible (while the sale runs in USDC). Operationally: hold no more than necessary in a vault - the proceeds are withdrawn as Claim proceeds and the remainder is taken by `sweep_unclaimed`. Accepted as an external dependency. A forced refund is not affected by a freeze of the participant's existing accounts: its destination is created inside the refund (§5.7). |

---

## Worked numeric examples

The units: 1 USDC = 1,000,000 units (6 decimal places). 1 $1WIN = 100,000,000 units (8 places). A price of $0.03 per whole token is 30,000 USDC units. Every calculation runs in the smallest units.

A buyer's contribution = `net`, what is retained = `spent`, the refund = `refund`, the pool = the sum of every contribution, the emission = the whole volume of tokens being distributed. The default values: a price of $0.03; an emission of 130,000,000 $1WIN; a Soft Cap of $3.9M.

**E1. Oversubscribed: more was collected than the whole emission costs.** The whole emission costs $3.9M and $10M of contributions were collected. Take a wallet with a $10,000 contribution:
```
tokens   = contribution × emission / pool = 10,000 × 130,000,000 / 10,000,000 = 130,000.00000000 $1WIN
retained = the cost of the tokens         = 130,000 × $0.03                   = $3,900
refund   = contribution − retained        = $10,000 − $3,900                  = $6,100
```
Every division here is exact - no rounding remainder arises in this example.

**E2. Where the rounding remainder comes from: a share that does not divide evenly.** The pool = the Soft Cap = $3.9M.

A single buyer takes the whole pool and gets the whole emission exactly:
```
tokens = 130,000,000.00000000 $1WIN   (the whole emission, no remainder)
```
The same $3.9M, but with three buyers of $1.3M each. Each share is a third of the emission: 130,000,000 / 3 = 43,333,333.333… - a repeating fraction. The token divides only to the 8th place, so the fraction is cut off there:
```
each of them   = 43,333,333.33333333 $1WIN
the three      = 129,999,999.99999999 $1WIN
the emission   = 130,000,000.00000000 $1WIN
──────────────────────────────────────────
left over      =           0.00000001 $1WIN   (one smallest unit)
```
That one indivisible unit stays undistributed in the token vault and is withdrawn later by `sweep_unclaimed`. It cannot be split three ways, so in the division each wallet loses less than one unit, and across N wallets up to N units.

**E3. Why the cost of the tokens is retained rounded up.** On oversubscription a wallet is charged the cost of the tokens it received, and the remainder is refunded. That cost may not come out as a whole number of units, and the direction of rounding decides the size of the refund.

Take a wallet with a $10 contribution. The pool is just one unit above the Soft Cap (3,900,000,000,001 USDC units) - which is enough for oversubscription to begin. The wallet's share and the cost of its tokens:
```
tokens = contribution × emission / pool = 333.33333333 $1WIN            (rounded down)
cost   = 333.33333333 × $0.03           = 9,999,999.9999 USDC units    (not a whole number)
```
The fractional part of 0.9999 units has to be rounded. Two options:

| Rounding of the retention | Retained | Refund = contribution − retained | The result |
|---|---|---|---|
| **up (what will be implemented)** | 10,000,000 = $10 | $0 | the full cost of the tokens is retained |
| down (drop the fraction) | 9,999,999 | 0.000001 USDC | less than the cost is retained - the refund is one unit larger than it should be |

Rounding **up**: the indivisible fraction stays in the vault rather than going to the buyer. With rounding down, every such wallet would get one unit more refund than it is due; across many wallets that would reduce the proceeds collected, and after they were withdrawn the vault would have no money left for the refunds of whoever claims last.

In this example the retention is exactly equal to the contribution, so the refund is zero - the wallet receives only the tokens.
