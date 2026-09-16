# The Vesting program - architecture (Anchor / Solana)

This technical document describes the $1WIN system's Vesting program - the gradual unlock of the token for the emission buckets outside the sale - at the level of accounts, seeds, instructions, invariants and math. The program implements the business mechanics of [1WIN_Programs_Business_Requirements.md](./1WIN_Programs_Business_Requirements.md) (BRD) §5 and goes deeper into §7 of [1WIN_Programs_Technical_Requirements.md](./1WIN_Programs_Technical_Requirements.md) (TRD). Where the wordings diverge, the Business and Technical Requirements define the business truth and this document defines the technical truth about Vesting.

**The platform:** Anchor 1.x (the `event-cpi` event mechanism; a conditional `init_if_needed` on accounts holding state is forbidden, see §9.8, §12); classic **SPL Token** (not Token-2022); the standard ATA and System programs. The environment's limits: nested-call (CPI) depth - 4; the compute budget per transaction (CU) - 1,400,000; the transaction size - 1232 bytes.

**Reference notation.** `In` is invariant number n from the table in §3. `§X.Y` is a section of this document. `BRD §X` / `TRD §X` are sections of the Business / Technical Requirements. `G-n` / `T-n` / `VE-n` are requirement identifiers from the Business Requirements (G - the general ones and the roles, T - the token, VE - vesting); they are carried through for end-to-end traceability.

## Contents

- [Glossary](#glossary)
- [1. Purpose and boundaries](#1-purpose-and-boundaries)
- [2. The system context](#2-the-system-context)
- [3. The invariants](#3-the-invariants)
- [4. The time model and TGE](#4-the-time-model-and-tge)
- [5. The account model](#5-the-account-model)
- [6. The PDAs and the seeds](#6-the-pdas-and-the-seeds)
- [7. The vaults and fund isolation](#7-the-vaults-and-fund-isolation)
- [8. The roles and authorization](#8-the-roles-and-authorization)
- [9. The instructions](#9-the-instructions)
- [10. The unlock math](#10-the-unlock-math)
- [11. The events](#11-the-events)
- [12. Security](#12-security)
- [Worked numeric examples](#worked-numeric-examples)

---

## Glossary

| Term | Meaning |
|---|---|
| **an activity** | `Vesting` - the PDA of a single emission bucket: one recipient, one emission amount, one schedule, one isolated $1WIN storage (§5.2, VE-2). |
| **the reference amount** | `reference_amount` - the activity's emission amount in token units; **the base of the unlock calculation** (VE-4). The calculation runs off it, not off the storage's balance. |
| **TGE** | The unlock start moment shared by every activity - `config.tge_ts` (§4, VE-7). |
| **unlocked (`vested`)** | The amount unlocked by the moment `now` under the activity's schedule; a pure function of the activity's fields and `config.tge_ts` (§10). |
| **available to claim (`claimable`)** | `vested(now) − claimed` - what is unlocked but not yet paid out; a derived quantity, with no on-chain read instruction (§11, VE-16). |
| **a period** | `p = floor((now − tge_ts)/MONTH)` - the ordinal 30-day interval from TGE (§10.1). |
| **the project's withdrawal account** (`withdrawal_authority`) | The single destination address when an activity is cancelled or closed - an ordinary project wallet (an address on the Ed25519 curve with a private key of its own, §9.1), set at `initialize` and reassigned by Admin with `set_withdrawal_authority` (§5.1, §9.10). The token account for the $1WIN remainder must belong to it (as its SPL owner), and the SOL rent goes to that same address (§9.7, I12). It is not a role: it signs nothing and calls no instruction (§8). |

---

## 1. Purpose and boundaries

Vesting is a **separate** program (not part of Sale, G-13): it gradually unlocks $1WIN for the emission buckets outside the sale - Growth, Marketing, Ecosystem, Community Rewards, Treasury, Liquidity (VE-1). The token's full supply is 1,000,000,000 $1WIN: TokenSale distributes 130,000,000 and the remaining 870,000,000 belong to the vesting buckets (T-3). The program keeps independent **activities** (§5.2) with an unlock start shared by all of them - a **single TGE** (VE-2, VE-7).

| Inside Vesting | Outside Vesting |
|---|---|
| Holding $1WIN in a per-activity storage under a PDA; computing the unlock from `reference_amount` and `config.tge_ts`; paying out (a claim) strictly to the recipient; tracking `claimed` | Minting the emission into the storages; the token's mint/freeze authority (Owner MS / Ops MS) |
| The shared TGE; creating and cancelling activities **before TGE**; editing an activity's recipient at any point in its life; closing a fully drawn activity; returning the rent and the storage remainder to the project's account; checking a storage's funding on demand (`assert_funded`, §9.9) | Controlling the exact mint volume and tracking the storages' balances (off-chain, the backend); the program stores no "this activity is funded" flag (VE-6) |
| The Owner / Admin / Operator / recipient roles by public key | The sale's claim (TokenSale); the tokenomics (what `reference_amount` a bucket has); cross-activity aggregations (built by the indexer from the events) |

---

## 2. The system context

**The external inputs (they exist before `initialize`):**

- The $1WIN mint (decimals = 8; initially supply = 0). It is pinned in `VestingConfig` at initialization, where its decimals are also checked the one and only time (§9.1); in every other instruction the mint is supplied as an account bound by `address = config.token_mint`.
- The public keys of the three on-chain roles: owner, admin, operator (TRD §7.3). The owner candidate (`pending_owner`) is not an external input: it appears only through a `propose_owner` call (§9.4).
- The standard programs: Token, ATA, System Program.

**The shared mint is a cross-program invariant.** There is one $1WIN across all of the programs (T-3). Revoking the mint authority to `None` (T-6) is performed **once, after the whole distributable volume has been issued** - the sale's emission and the emission of every vesting bucket - normally in **a single atomic backend transaction before TGE arrives**: a sequence of `mint_to` into each storage (TokenSale's `token_vault` and the storages of every vesting activity), then `assert_funded` on each vesting activity (§9.9), then the closing `SetAuthority(None)` (T-8, TRD §4). An underfunded storage brings the whole transaction down on `assert_funded`, so the mint authority is not revoked on an under-mint. The mandatory property is a **complete** set of `assert_funded` calls in the same transaction as `SetAuthority(None)`; the `mint_to` calls themselves may be executed earlier in separate transactions, since `assert_funded` reads the state as of its own execution (the ceiling on the number of activities in one transaction is the transaction size; the check belongs in the pre-flight procedure). The activity storages must exist by then - the activities are created in advance (§9.5). After the revoke, topping up is impossible. The residual under-mint risk before the revoke is in §12.2.

**The deploy (TRD §9.3, G-13).** The $1WIN mint is deployed first (the mint authority with Owner MS, freeze with Ops MS, supply 0). Vesting is deployed independently of TokenSale/Burn and references only the $1WIN mint; the storages are created in `create_vesting` (§9.5), not at deploy time.

---

## 3. The invariants

These statements must hold across every state transition of the program. The "Test" column defines the mandatory minimum of coverage: a positive and a negative path per check.

| # | Invariant | How it is enforced | Test |
|---|---|---|---|
| **I1** | The payout bookkeeping balances and never exceeds the reference: after a claim `claimed == vested(now)`, and always `claimed ≤ reference_amount`. | `claim` computes `amount = vested(now) − claimed` from the pre-CPI state and writes `claimed += amount` in the same instruction; `vested` is bounded above by `reference_amount` by construction (the proof of the ceiling is in §10.3). | Random `claim` sequences at different moments → after each one `claimed == vested(now)` and `claimed ≤ reference_amount`. **Negative:** an attempt to get more than `vested` does not go through - `amount` is fixed by the formula. |
| **I2** | The activity storages are isolated: a claim on activity `A` moves only `A`'s vault; one activity's funds are unreachable for another (VE-5). | The vault is bound by the seeds `[b"vault", activity_id]` + `token::mint` + `token::authority = vault_authority`; the addresses of the vault and of the `Vesting` account are derived from the single `activity_id` argument (§6.2). On the remainder-withdrawal path the isolation is held by a pair of conditions - `beneficiary_token.owner == config.withdrawal_authority` (§9.7) and `config.withdrawal_authority != vault_authority` (§9.1): together they exclude every other activity's storage from the withdrawal slot, since those belong to `vault_authority`. | **Negative:** a `claim` on activity `A` with activity `B`'s vault passed → a binding error (`ConstraintSeeds`/`ConstraintTokenOwner`). **Positive:** a normal claim touches only its own vault. |
| **I3** | A claim hands out neither more than what is unlocked nor more than the storage's balance; an insufficient balance reverts everything, with no partial payout (VE-13). | `claim`: `amount = vested(now) − claimed`, `require amount > 0`, `require vault.amount ≥ amount` (the only balance read in `claim`, §9.8); on a shortfall, `InsufficientVaultBalance` and no ATA is created. | **Negative:** an underfunded vault → `InsufficientVaultBalance`, and `claimed` does not change. **Positive:** a funded vault → exactly `amount` is handed out. |
| **I4** | Funds go only to the recipient: the claim's destination is an ATA bound to the `recipient` slot, whose address is pinned by `address = vesting.recipient` (the **current** value of the stored field at call time, not the signer); the operator physically cannot redirect it (VE-12). The field is edited only by Admin with `set_recipient` (§9.6); what has already been paid out stays with the previous address and only the undrawn remainder is redirected. A zero `recipient` is inadmissible - the claim would go irretrievably to the ATA of an address with no private key; so is any off-curve address, since a program-derived one has no private key either: the recipient must lie on the Ed25519 curve. | A constraint derives the canonical ATA address from `recipient`; any other token account (even with `mint = token_mint` but a different authority) fails context resolution. `create_vesting` and `set_recipient` take the recipient as a `SystemAccount`, reject `Pubkey::default()` and require an on-curve address, otherwise `PayeeNotOnCurve` (§9.5, §9.6). | **Negative:** the operator passes their own ATA → a binding error before the transfer; `create_vesting` / `set_recipient` with the address `Pubkey::default()` → a rejection during context resolution (`AccountNotSystemOwned`), before the instruction body; with an off-curve address (a PDA of any program) → `PayeeNotOnCurve` in the body, and nothing is written. **Positive:** a claim by the operator credits the recipient. |
| **I5** | TGE is irreversible and strictly positive: it is editable only while `now < tge_ts` and immutable once it has arrived; an unset `tge_ts` - zero or a negative value - never opens the unlock (VE-7, §4.3). | `set_tge` requires `now < config.tge_ts` (otherwise `ScheduleStarted`); `claim` requires `tge_ts > 0` and `now ≥ tge_ts` (otherwise `ScheduleNotStarted`). | **Negative:** `set_tge` with `now ≥ tge_ts` → `ScheduleStarted`; `claim` with `tge_ts == 0` and with `tge_ts < 0` → `ScheduleNotStarted`. **Positive:** an edit before TGE goes through; a claim after TGE goes through. |
| **I6** | From TGE on, the set of obligations and their sizes are final: there are no new activities after TGE, the amounts and schedules of the existing ones are immutable, and an activity can be removed from the set only once the obligation has been discharged - `claimed ≥ reference_amount` (VE-8, VE-10, VE-17). The payout address is **not** part of what is frozen: `recipient` is edited by Admin at any point in the activity's life (VE-9, §9.6), and the size of the obligation does not change with it. | `create_vesting` and `cancel_vesting` require `now < config.tge_ts`, otherwise `ScheduleStarted` (§4.2); `cancel_vesting` additionally requires `claimed == 0` (§9.7); `close_vesting` requires a full draw (I9) and therefore removes an activity whose obligation has already been discharged. There is no instruction in the program at all that changes `reference_amount` or the schedule's shares - those fields are written only by `create_vesting`. The absence of a time gate on `close_vesting` is a derived property of the pair of instructions, not a gap: the justification is in §9.7. | **Negative:** `create_vesting` / `cancel_vesting` with `now ≥ tge_ts` → `ScheduleStarted`. **Positive:** they go through before TGE; `set_recipient` goes through after TGE too, changing neither the amount nor the payout bookkeeping. |
| **I7** | Money is counted without overflow: all of the amount and schedule arithmetic is `checked_*`, the intermediates are u128 with a downcast to u64 after the divisions; division by zero is ruled out by the preconditions. | u128 intermediates in `vested()` and in the share calculation; the division by `monthly_bps` happens only in the `monthly_bps > 0` branch; `MONTH` and `10000` are non-zero constants (§10.4). | Boundary values (`reference_amount = u64::MAX`, the maximum `M`, `cliff_months`). **Negative:** parameters that make the schedule overflow → `InvalidSchedule` / `MathOverflow`. |
| **I8** | `activity_id` is monotonic and never reused, not even after an activity is cancelled or closed (protection against collisions and against a replay after a close). | `create_vesting` takes `activity_id = activity_count`, then `activity_count += 1` (checked); `cancel_vesting` / `close_vesting` do **not** decrease the counter. | Create → cancel → create: the second id is strictly greater. **Negative:** re-deriving the address of a closed activity does not match the new one. |
| **I9** | An activity is closed only once it is fully drawn (VE-17). | `close_vesting` requires `claimed ≥ reference_amount`, otherwise `NotFullyClaimed` (on the `≥` form, see §10.3). It has no time gate and needs none: before TGE the condition is unsatisfiable for any activity (§9.7). | **Negative:** `close_vesting` with `claimed < reference_amount` → `NotFullyClaimed`. **Positive:** after the final claim it closes and the rent → the project's account. |
| **I10** | The vesting funds are unreachable from outside **while the bytecode is unchanged**: no other program or key holds a role or signs a storage transfer or close. The caveat "while the bytecode is unchanged" is load-bearing: the upgrade-authority holder may replace the logic (a super-role, §8, §12.1, §12.2). | Every storage is owned by the Vesting program's `vault_authority` PDA; only that program signs a transfer or a close, through `CpiContext::new_with_signer` (§7). | **Negative:** a transfer out of a storage or its close by a foreign key/program → an error. |
| **I11** | `vested(now)` is monotonically non-decreasing in time and lies in `[0, reference_amount]`: before TGE it is 0, and the terminal branches give exactly `reference_amount`. | The pure `vested()` function (§10.2): `months` is non-decreasing in `p` and bounded by `M`; the proof is in §10.3. | A property test: for random schedules and a growing `now`, `vested` is non-decreasing and `≤ reference_amount`; in the terminal period it `== reference_amount`. |
| **I12** | On a cancel or a close, the remainder and the rent go only to the project's single withdrawal account - the **current** value of `config.withdrawal_authority` at call time (set at `initialize`, reassigned by `set_withdrawal_authority`; §5.1, §9.1, §9.10): the $1WIN remainder goes to `beneficiary_token`, whose SPL owner == `config.withdrawal_authority`; the SOL rent from closing the storage and the `Vesting` record goes to `rent_collector`, whose address == `config.withdrawal_authority`; the remainder transfer precedes the close (§9.7). | `cancel_vesting`/`close_vesting`: on `beneficiary_token` there are three explicit conditions, in check order: the right mint (`mint == config.token_mint`), `owner == config.withdrawal_authority` (which excludes every other activity's storage and any third-party account), and `≠ this activity's storage`; `rent_collector` is typed `SystemAccount` with `address == config.withdrawal_authority` (so it cannot be a token account), and the stored address lies on the curve by construction (§9.1, §9.10); drain-if-nonzero → close. | **Negative:** a `beneficiary_token` with a foreign mint → `InvalidMint`; a `beneficiary_token` with owner ≠ `withdrawal_authority` → `InvalidBeneficiary`; a `rent_collector` ≠ `withdrawal_authority` → `InvalidRentCollector`; this activity's storage address → `ConstraintDuplicateMutableAccount` (2040): Anchor rejects a duplicate among the mutable serializable slots before any user condition. **Positive:** the $1WIN remainder arrives at `beneficiary_token` and the rent at `rent_collector`. |

---

## 4. The time model and TGE

Vesting has no multi-phase machine but a single one-way TGE gate; the unlock proceeds with time (VE-7, §10). There is no separate "activate the unlock" instruction.

### 4.1 The source of time

Time is `Clock::unix_timestamp` (the sysvar), not a caller's argument. A claim has no deadline; the only time boundary is `config.tge_ts`.

### 4.2 The shared TGE as a one-way gate

`config.tge_ts` is the one moment at which every activity's unlock begins. The date is set to a concrete value at `initialize`, and Admin edits it with `set_tge` **until TGE arrives** (`now < config.tge_ts`). Once `now ≥ config.tge_ts` the value is **immutable** (`set_tge` reverts with `ScheduleStarted`, I5).

That same boundary `now < config.tge_ts` is the **edit window** for two instructions: `create_vesting` and `cancel_vesting` (I6). When TGE arrives the set of activities and their sizes freeze: an activity can neither be created nor cancelled, and no instruction changes an existing one's amount or schedule either before or after TGE. The payout address is not covered by that freeze: `set_recipient` has no time condition and works throughout the activity's life (§9.6) - the obligation stays the same and only the wallet that receives the undrawn remainder changes. There is **no** revoke of the unlock, no claim pause and no sweep of what was never claimed (VE-14): a recipient's funds are never taken back.

**Why `create_vesting` must be gated by `now < tge_ts` (security-critical).** `vested()` is computed from the shared `tge_ts` (§10.2). An activity created after TGE would be **unlocked immediately (fully or partly)**, and it could no longer be cancelled (I6) - the storage would be stuck with an open obligation that the recipient may draw at once, bypassing the schedule. That is why every activity is created and funded **before TGE** (§2, §9.5).

### 4.3 A non-positive `tge_ts` means "TGE is not set"

`tge_ts == 0` is the marker "the start date is not set", not "the start is in 1970" (zero in Unix time is 1 January 1970). The difference matters: with a zero `tge_ts` the claim condition `now ≥ tge_ts` is always true, i.e. the program would consider TGE long past and `vested()` would hand out the whole emission at once. A negative `tge_ts` (a date before 1970) gives the same result and, on top of that, the period `p` is certainly greater than `s + M`, i.e. the terminal branch of `vested()` - the whole emission in a single transfer.

That is why `claim` requires `tge_ts > 0` as a separate check, otherwise `ScheduleNotStarted` (I5). The gate's form is a strict inequality against zero rather than `≠ 0`: it closes the whole class "the date is not set", including any negative value, rather than the single value `0`.

**A non-positive `tge_ts` makes the config unfixable.** `set_tge` requires `now < config.tge_ts` (§9.2): with a zero or negative value that condition is false for any `now`, and the edit reverts with `ScheduleStarted`. `create_vesting` and `cancel_vesting` are closed by the same gate (§4.2), so they are unavailable in such a config too: the date cannot be set and activities can neither be created nor cancelled. Editing the recipient (`set_recipient`) stays available - its gate does not depend on `tge_ts` - but there is nothing to pay out on such an activity: `claim` requires `tge_ts > 0`.

**Through the public interface this state is unreachable.** `initialize` and `set_tge` accept only `tge_ts > now` (§9.1, §9.2), and there is no other way to write the field. The check in `claim` is insurance against a refactor: it holds the rule "not set ≠ a point in time" regardless of how the config was filled in. Its negative test is written with a forged config - an account written directly with a non-positive `tge_ts` - not with a sequence of instructions.

---

## 5. The account model

The bookkeeping is per bucket: a singleton config + a PDA per activity + a storage per activity.

### 5.1 VestingConfig (a singleton, seeds `["config"]`)

| Field | Type | Meaning |
|---|---|---|
| `owner` | Pubkey | Owner MS - creating/cancelling/closing activities and changing the roles (§8). It is handed over in two steps: `propose_owner` names the candidate and `accept_owner` is signed by the candidate themselves (§9.4). |
| `pending_owner` | Option\<Pubkey\> | The named Owner candidate; `None` - no handover has begun. The "not set" marker is precisely `None` and cannot be a zero key: `propose_owner` rejects that (§9.4). |
| `admin` | Pubkey | The program's configuration: the TGE date (`set_tge`) and an activity's recipient (`set_recipient`), §8. Changed with `set_admin`. |
| `operator` | Pubkey | The automation account - it can **only** trigger `claim` (§8). Changed with `set_operator`. |
| `token_mint` | Pubkey | The $1WIN mint (a read-only reference); checked when an activity is created and when the storages are bound. |
| `withdrawal_authority` | Pubkey | The project's single withdrawal account (a system-owned wallet): when an activity is cancelled or closed, the $1WIN remainder goes to a token account with this SPL owner and the SOL rent goes to this same address (§9.7, I12). Set at `initialize`, reassigned by Admin with `set_withdrawal_authority` (§9.10). |
| `tge_ts` | i64 | The TGE shared by every activity (§4). `0` means "not set" (§4.3). Editable until it arrives. |
| `activity_count` | u64 | A monotonic activity-id counter: it defines the next activity's `activity_id` and **only grows** (I8). |
| `vault_authority_bump` | u8 | A cache of the canonical bump of the `vault_authority` PDA: used to sign the storage transfer/close CPIs (§6.3). |

**The size:** Pubkey 5×32 = 160 + Option\<Pubkey\> 33 + i64 8 + u64 8 + u8 1 = a 210 B body, ≈ **218 B** with the discriminator. The account is never closed.

### 5.2 Vesting (a PDA per activity, seeds `["vesting", activity_id_le]`)

| Field | Type | Meaning |
|---|---|---|
| `recipient` | Pubkey | The recipient wallet: a claim is only ever paid out to it (I4). Changed only by Admin, at any point in the activity's life (`set_recipient`, §9.6). |
| `reference_amount` | u64 | The activity's emission amount in token units (8 decimals) - the base of the unlock calculation (VE-4). Set at creation and never changed. |
| `claimed` | u64 | How much has already been paid out; `claimable = vested(now) − claimed`. It grows in `claim` (I1). |
| `tge_bps` | u16 | The share unlocked at TGE, in basis points (0-10000). |
| `monthly_bps` | u16 | The monthly share, in basis points (0-10000). |
| `cliff_months` | u16 | The cliff in 30-day months. |

**The size:** 32 + u64 2×8 + u16 3×2 = a 54 B body, ≈ **62 B** with the discriminator. Owner pays the rent (`create_vesting`), and when the activity is closed - `cancel_vesting` before TGE or `close_vesting` after a full claim - it goes **not to the payer** but to the project's withdrawal account `config.withdrawal_authority` (the `rent_collector` slot, §9.7, I12): Owner is a multisig and the withdrawal account is a separate address.

The number of monthly tranches `M` and the deadlines are **not stored** - they are derived from the fields (§10.2). The bucket label ("Treasury", "Liquidity", …) is **not stored** on-chain - it only goes into the creation event (§11): the logic never reads it.

`activity_id` is **not stored** in the record: it is the seed of its address (§6.2), so the record is addressed by it rather than the other way round. Off-chain reconstruction goes by derivation: for `id ∈ [0, config.activity_count)` the address `["vesting", id_le]` is derived; on top of that, every event of an activity carries `activity_id` as a field (§11).

### 5.3 An activity's storage and `vault_authority`

Funds are held in a $1WIN SPL token account (165 B) per activity, at the seeds `["vault", activity_id_le]`. Every storage is owned by a single dataless `vault_authority` PDA (seeds `["vault_authority"]`); the signing of transfers/closes is in §7. The shared `vault_authority` does not break the isolation: every outflow is bound by the seeds `["vault", activity_id]` and moves only its own activity's vault (I2, §6.2).

Topping up is a one-off emission mint by Owner (the mint authority) **before TGE**, off-chain (VE-6, T-8); the outflows are a `claim` to the recipient and the remainder withdrawal on `cancel_vesting`/`close_vesting` (§9.7). The storage is created in `create_vesting` and closed together with the activity.

---

## 6. The PDAs and the seeds

### 6.1 The PDA schemes

| PDA | Seeds | Components | Bump |
|---|---|---|---|
| `VestingConfig` | `[b"config"]` | a literal | not stored - the canonical one is recomputed on every access |
| `Vesting` | `[b"vesting", activity_id.to_le_bytes()]` | a literal + 8 B (u64 LE) | not stored - the canonical one is recomputed on every access |
| An activity's storage | `[b"vault", activity_id.to_le_bytes()]` | a literal + 8 B (u64 LE) | not stored - bound by the seeds + `token::mint` / `token::authority` |
| `vault_authority` | `[b"vault_authority"]` | a literal | cached as `vault_authority_bump` in `VestingConfig` |
| `event_authority` | `[b"__event_authority"]` | a literal (Anchor's `#[event_cpi]`) | managed by Anchor |

The PDA spaces do not intersect: each of the five schemes has its own concatenated seed material (the hash preimage) and its own address. The justification is the unit test `programs/vesting/src/math/tests.rs`: for `activity_id ∈ {0, 1, 2, u64::MAX}` it pairwise compares both the seed material and the derived addresses of every scheme, as well as the addresses of different activities against one another. The total length is not the distinguishing feature: the pair `["vesting", activity_id]` (7 + 8 = 15 bytes) and `["vault_authority"]` (15 bytes) are the same length, and the difference is only in the material's bytes.

### 6.2 The activity PDA via a counter, and addressing

`create_vesting`: (1) read `activity_id = activity_count` from the config; (2) create (`init`) the `Vesting` account and the storage at the seed `[…, activity_id_le]`; (3) increment `activity_count = activity_id + 1` (overflow-checked). The hard implementation requirements:

- The seed **must** reference the `config.activity_count` field: Anchor computes the seed for `init` while deserializing the context, before the handler body - the address is deterministic and there is no race.
- The increment happens **only in the handler body**, after the activity has been created successfully.
- `activity_id` is **never passed as an argument** to `create_vesting` (otherwise a client would name an occupied or a future slot).
- `activity_count` **never decreases** on a cancel/close → a closed activity's address is never reused (protection against address reuse and against a replay after a close, I8).

An existing activity (`claim`, `set_recipient`, `cancel_vesting`, `close_vesting`) is addressed by the single instruction argument `activity_id: u64` (declared in `#[instruction(...)]` before it is used in `seeds`): both the `Vesting` address (`["vesting", activity_id_le]`) and the storage address (`["vault", activity_id_le]`) are derived from it. One value fixes both bindings, so another activity's storage cannot be substituted (I2).

### 6.3 The bump policy

The canonical bump everywhere. The address of an account supplied to an instruction is proven by **recomputation**: the slot is declared as `bump` with no value, and Anchor derives the canonical bump through `find_program_address` and then compares the resulting address with the one supplied. All three of the program's accounts are checked this way - the singleton, the activity record and the storage. Comparing against a bump written inside the account being checked will not do: it proves only that the pair "address ↔ stored bump" is consistent, so an account at any other valid address for the same seeds would pass the check and canonicity would rest on creation discipline rather than on the check.

The only stored bump is `vault_authority_bump` in the singleton (§5.1): the program signs the storage transfer and close CPIs with it, and it was fixed to the canonical value at `initialize`. That is a different case: the bump is taken from an already-verified external account, not from the one whose address is being checked.

The cost of recomputing is one search iteration per bump seed tried; the measurements and the CU ceilings are in `tests/vesting/tests/cu_bench.rs`.

---

## 7. The vaults and fund isolation

- **Signing transfers and closes.** The program signs a transfer and a close on behalf of `vault_authority` (§5.3) through `CpiContext::new_with_signer` with the seeds `[b"vault_authority", &[bump]]`, where the bump comes from `config.vault_authority_bump` (§6.3). Users have no direct access to the storages and no other program holds a role - the vesting funds are unreachable from outside (I10).
- **Minting happens outside the program.** The program does not hold the mint authority (T-4, T-8): even with a bug in the code it does not print tokens, it only distributes what has already been minted; revoking the mint authority is in §2.
- **A storage's balance takes no part in the unlock calculation** (VE-4, §10). It is read by **four** instructions, in two different roles:
  - **as a funding gate** - `claim` (`vault.amount ≥ amount`, I3, §9.8) and `assert_funded` (`vault.amount + claimed ≥ reference_amount` - covering the undrawn remainder of the obligation, §9.9, which changes no state);
  - **as the source of the withdrawal amount** - `cancel_vesting` and `close_vesting`: the storage's whole balance is transferred to `beneficiary_token` before the close (§9.7). Hence a consequence that does not follow from the "gate" role: an inoperable storage (one frozen by the freeze-authority holder, say) makes both instructions unexecutable, because the remainder transfer precedes the close - §12.2.

  A stray transfer to a storage's address unlocks nothing, and on a close such an excess goes to the project (§9.7). The program stores no "funded" flag (VE-6) - the residual under-mint risk is in §12.2.

---

## 8. The roles and authorization

There are four roles - Owner, Admin, Operator and an activity's recipient - and their sets of rights **do not intersect** (G-1). What does not intersect is precisely the sets of rights: the program does not check the role keys for distinctness, and one key may hold any two of the config's roles or all three. All that is required of a role key is a non-zero address (§9.1/§9.3/§9.4); a role may be a program wallet - a multisig signs through its program - and the curve requirement on the two payout addresses (§9.5, §9.10) does not apply to it. An actual separation of duties comes from assigning different keys - an organizational decision of whoever owns the keys, not a gate in the program; Sale and Burn are built the same way. **There is no pause instruction** (VE-18): the program offers no separate lever to "block the recipient's claim". That is a property of the Vesting program itself: the freeze authority of the $1WIN mint is held by Ops MS (TRD §4) - an external lever, with the residual centralization noted in §12.2.

| Instruction | Owner | Admin | Operator | Recipient |
|---|:--:|:--:|:--:|:--:|
| `initialize` (= the upgrade authority) | ✓ | - | - | - |
| `set_tge`, `set_recipient`, `set_withdrawal_authority` | - | ✓ | - | - |
| `set_admin`, `set_operator`, `propose_owner`, `cancel_owner_proposal` | ✓ | - | - | - |
| `accept_owner` (signed by the named candidate) | - | - | - | - |
| `create_vesting`, `cancel_vesting`, `close_vesting` | ✓ | - | - | - |
| `claim` | - | - | ✓ | ✓ |
| `assert_funded` | - | - | - | - |

**Above the operational roles stands the program's upgrade authority - a super-role.** It is not in the matrix above (it calls none of the program's instructions) but it dominates it: the holder can replace the program's logic and thereby gain control over every storage. It is held by Owner MS (§9.1); the program remains upgradeable and is not made immutable after TGE - the trust boundary and the revoke procedure are recorded in the threat map and the accepted risks (§12.1, §12.2). `initialize` is bound precisely to the upgrade-authority holder (§9.1).

- **Owner** is the project multisig, Owner MS (TRD §9.2). It holds the roles and every instruction that moves tokens and rent: creating an activity (paying the rent of its accounts), cancelling it and closing it. All of its instructions check `signer == config.owner` structurally (`has_one = owner` on `VestingConfig` + the Signer marker), otherwise `NotOwner`. The Owner handover is **two-step**: `propose_owner` names the candidate and `accept_owner` is signed by the candidate themselves, thereby proving that the key is available to them; `cancel_owner_proposal` withdraws the proposal (§9.4). Until it takes effect the rights lie entirely with the sitting Owner. `accept_owner` is the program's only instruction signed by neither Owner, nor Admin, nor a party to an activity: its context has no `has_one`, and the signer's role is defined by the `config.pending_owner` field.
- **Admin** is the project multisig, Ops MS (TRD §2.2): the program's configuration - the TGE date (`set_tge`, §9.2), an activity's recipient (`set_recipient`, §9.6) and the project's withdrawal account (`set_withdrawal_authority`, §9.10). All three instructions check `signer == config.admin` structurally (`has_one = admin` on `VestingConfig` + the Signer marker), otherwise `NotAdmin`. Owner does not hold those rights: the separation is the same as in TokenSale, where only Admin edits the schedule and the parameters. Admin moves no tokens and no rent and neither creates nor closes accounts, but it sets two destination addresses: `set_recipient` works throughout the activity's life and therefore decides where the undrawn remainder will go, and `set_withdrawal_authority` decides where the remainder and the rent of activities being closed will go; both levers are permanent and are accepted as a risk in §12.2. Admin is appointed by Owner through `set_admin` (§9.3).
- **Operator** is the automation account: it can **only** trigger `claim`; it does not dispose of the funds, which go to the recipient (I4). It is appointed by Owner through `set_operator` (§9.3).
- **The recipient (`recipient`)** is the bucket's project wallet: it claims its own activity; it is changed only by Admin and at any point in the activity's life (`set_recipient`, §9.6). The recipient has no veto over a change: the right lies entirely with Admin, and the observability is the `RecipientUpdated` event (§12.2). The recipient is an ordinary wallet - an address on the Ed25519 curve with a private key of its own: both write paths reject an off-curve address (§9.5, §9.6), so a multisig vault or any other program-derived address cannot be a recipient.
- **The `claim` authorization:** `signer == vesting.recipient || signer == config.operator`, otherwise `NotAuthorized`. Both keys are always set - `recipient` is mandatory on an activity and `operator` in the config - so no zero value arises that an arbitrary signer would match.
- **`assert_funded` requires no role** (§9.9): its context has no `Signer` slot, the instruction changes no state and grants no rights - an empty row in the matrix means "the signer is not checked", not "forbidden to everyone".
- **The token's mint/freeze authority live outside the program** (§2, §7).
- **`withdrawal_authority` is not a role but a withdrawal target.** Both at `initialize` and on a change it is supplied as an account (a `SystemAccount` slot, §9.1, §9.10), but it signs nothing and calls no instruction; it is the address of an ordinary project wallet - on the Ed25519 curve, with a private key of its own (§9.1) - that receives the $1WIN remainder (as the SPL owner of `beneficiary_token`) and the SOL rent (`rent_collector`) when an activity is cancelled or closed. It is reassigned by **Admin** (`set_withdrawal_authority`, §9.10), not by Owner: the destination address is a configuration parameter of the program, like the TGE date and an activity's recipient. The cancel and the close themselves stay with Owner (§9.7): Admin decides where the funds go, Owner decides when and for which activity (§9.1, §9.7, I12).

---

## 9. The instructions

There are **fourteen** instructions: §9.1-§9.10, where §9.3 describes two (`set_admin`, `set_operator`), §9.4 describes three (`propose_owner`, `cancel_owner_proposal`, `accept_owner`) and §9.7 describes two (`cancel_vesting` and `close_vesting`). There are **thirteen** events (§11) - `assert_funded` (§9.9) has none, as it changes no state.

**The general rules.** In **every** context the singleton is bound by the pair `seeds = [b"config"]` + a valueless `bump` (§6.3) rather than by `has_one = owner` / `has_one = admin` alone: `has_one` compares a field of an account that has already been loaded, so an account belonging to this program with a correct `VestingConfig` discriminator and a role field equal to the signer would pass the role check - binding the address to the canonical PDA closes that structurally. In every context that involves an activity's storage (`create_vesting`, `claim`, `cancel_vesting` / `close_vesting`, `assert_funded`) the mint is supplied as a `token_mint` account with `address = config.token_mint`, the storage is bound by the triple `seeds` (§6.2) + `token::mint = token_mint` + `token::authority = vault_authority`, and `vault_authority` itself by `seeds = [b"vault_authority"]` + `bump = config.vault_authority_bump` (the cache of §5.1). In the contexts without a storage (`set_tge`, `set_admin`, `set_operator`, `propose_owner`, `cancel_owner_proposal`, `accept_owner`, `set_recipient`, `set_withdrawal_authority`) there are no mint or storage slots: the logic never reads the mint; `set_recipient` and `set_withdrawal_authority` do carry `vault_authority` (the same seeds + the bump from the config) - its address is what the payout-target check compares against (§9.6, §9.10). The storage is bound to the `token_mint` slot rather than to the `config.token_mint` field: an `init` of a token account requires an account, not a `Pubkey`, and the binding takes one form in every instruction. The time preconditions are checked against `config.tge_ts` through `Clock::unix_timestamp` (§4). All of the arithmetic is `checked_*`; `MathOverflow` arises wherever an instruction computes amounts, a schedule or an input ceiling, and it is named explicitly in that instruction's error list (§9.1-§9.9); instructions with no calculation do not list it. Every error code lives in a **single** `#[error_code] enum`. Touching a closed or non-existent `Account<T>` is rejected by Anchor during context resolution, before the handler body, and the contract of that rejection is defined by a **class rather than a single code**: `AccountNotInitialized` (3012) while the account belongs to the System Program and its balance is zero, or `AccountOwnedByWrongProgram` (3007) if lamports have been transferred to a closed activity's address - the address is predictable (§6.2), so such a transfer is available to anyone. The "a repeat is impossible" property holds in both cases: both codes revert the transaction before the body. The negative tests for a repeat after a close assert a rejection of that class, not one specific code. There are no hardcoded business values. Three constants are **sanity ceilings on input**: they bound the instruction arguments at the entry and take no part in the calculations of §10.

| Constant | Value | Bounds |
|---|---|---|
| `MAX_TGE_DELAY` | `365 × 86400` = 31,536,000 s | `tge_ts ≤ now + MAX_TGE_DELAY` in `initialize` and `set_tge` (§9.1, §9.2) |
| `MAX_SCHEDULE_MONTHS` | `120` | `cliff_months + M ≤ MAX_SCHEDULE_MONTHS` in `create_vesting` (§9.5) |
| `MAX_LABEL_LEN` | `32` bytes | `label.len() ≤ MAX_LABEL_LEN` in `create_vesting` (§9.5) |

Each ceiling has a paired test "the boundary passes / the boundary + 1 is rejected". `InvalidParams` (the length of `label`) is a variant of its own by the errors-explicit rule.

**`InvalidAuthority` (code 6004) serves five invariants - the merging is deliberate.** Reduced to this variant are: a zero `owner`, `admin` or `operator` (§9.1, §9.3, §9.4); a zero `withdrawal_authority` (§9.1, §9.10); `withdrawal_authority == vault_authority` (§9.1, §9.10); a zero recipient (§9.5, §9.6); `recipient == vault_authority` (§9.5, §9.6). The reason for merging: the order of the variants in the single `#[error_code] enum` is frozen and defines the codes 6000-6021 (TRD §7.8), so splitting one would shift the codes of errors that have already been reviewed - a price higher than the diagnostic gain. Extending the list is admissible append-only only: a new variant goes last and gets the next free code, and the codes of the existing ones do not shift. The specific violation is told apart by the instruction and by the set of accounts it supplied; each of the five checks has a negative test of its own. The curve check on the payout addresses is **not** merged into this variant: `PayeeNotOnCurve` (code 6021) was added append-only for an off-curve recipient or withdrawal target (§9.1, §9.5, §9.6, §9.10) - a different class of failure with a different remedy, and one predicate on four write paths rather than a sixth reading of one code.

**Two withdrawal accounts - two error variants.** `InvalidBeneficiary` (code 6012) is attached to the `beneficiary_token` slot and serves **one** invariant, "the $1WIN remainder goes only to the project's withdrawal account and never into a storage", through two conditions on one slot: a foreign SPL owner and coinciding with this activity's storage (§9.7). A wrong `rent_collector` carries a variant of its own, `InvalidRentCollector` (code 6018, §9.7): it is a separate slot and a separate invariant, so the code tells you which of the two withdrawal targets the caller got wrong. Neither belongs to the list of deliberately merged variants - `InvalidAuthority` above and `InsufficientVaultBalance` (§9.9).

**The roles are set by `Pubkey` arguments and the recipients by accounts: the asymmetry is deliberate.** `admin`, `operator` and the `owner` candidate arrive as arguments and are gated only against the zero address and mutual inequality; `recipient` and `withdrawal_authority` are supplied as typed accounts and are additionally gated against `!= vault_authority`. The weaker side has no exploitation path: the program signs as `vault_authority` only on a storage transfer and close and never signs a role instruction for anyone, so "a role set to a PDA address" grants no privileges. A PDA address does not take ownership at all: taking over is signed by the candidate themselves (§9.4), and nobody signs role instructions on a PDA's behalf - the proposal simply stays unexecutable and `config.owner` does not change. A wrong admin or operator address is fixable with another `set_admin` / `set_operator` (§12.2). Symmetry is not introduced in code: it would require a `vault_authority` slot in the role-change contexts for the sake of a check that closes no exploitation path.

### 9.1 `initialize`

- **Who:** the holder of the program's **upgrade authority** (Owner MS), otherwise `NotUpgradeAuthority`. Binding to an on-chain fact requires **two** conditions, both mandatory:
  1. `vesting_program.programdata_address()? == Some(program_data.key())` - the `program_data` supplied belongs to **this** program;
  2. `program_data.upgrade_authority_address == Some(authority.key())` - the signer is the upgrade-authority holder.

  Only together do they rule out hijacking the singleton's initialization: the second condition alone is not enough - the upgrade-authority holder of any other upgradeable program would supply that program's `ProgramData` and create the config with roles of their own before the legitimate call, and the singleton is created once, with recovery possible only by redeploying under a new program id. The deploy chain is "deploy → init through a proposal" (TRD §9.3); there is no separate step to hand over the storages' authority: they are created later in `create_vesting` already with `authority = vault_authority`.
- **The arguments:** `owner`, `admin`, `operator`, `tge_ts` - **four**, in that order. `token_mint` and `withdrawal_authority` are **not** passed as arguments: both are supplied as context accounts and their addresses are written into the config.
- **The context:** the table below. The slot names and their order matter - the client assembles the account metadata positionally.
- **It creates:** `VestingConfig` (`owner`, `admin`, `operator`, `token_mint`, `withdrawal_authority`, `tge_ts`, `activity_count = 0`); and caches `vault_authority_bump` (`ctx.bumps.vault_authority`).
- **The withdrawal target is supplied as an account, not as a `Pubkey` argument.** The `withdrawal_authority` slot is typed `SystemAccount` - the type *is* the check, the same construction as `recipient` in §9.5. The program requires the withdrawal target to be a system-owned account: the rent of the accounts being closed goes to that address (the `rent_collector` slot in §9.7 is typed `SystemAccount` with `address == config.withdrawal_authority`), so the check stands at the entry rather than surfacing as a failure on the first activity close. The consequence for the zero address is the same as for the recipient (§9.5): `Pubkey::default()` coincides with the System Program's address, an account at that address belongs to the loader, and so the slot's type rejects it during context resolution (`AccountNotSystemOwned`) - before the instruction body; the `withdrawal_authority != Pubkey::default()` check in the body **stays** as insurance against a refactor of the type (fail-closed), but it is unreachable through the public interface. The type does not prove that a key stands behind the address - an address with no account behind it (a program-derived address of any program, this program's own future activity PDAs included) is attributed to the System Program and passes it - so the body also requires the address to lie on the Ed25519 curve (`validate_payee`, otherwise `PayeeNotOnCurve`): a program-derived address is off-curve by construction and no private key can sign for it, so the remainder and the rent sent to it would be lost. The curve check covers the whole class of which the explicit `vault_authority` gate below is one member; the gate stays as a named guard with its own error, checked first. Changing the withdrawal target after `initialize` goes through a separate instruction, `set_withdrawal_authority` (§9.10) - the same slot, the same type and the same three checks.
- **Validation (in check order):** `tge_ts > now` and `tge_ts ≤ now + MAX_TGE_DELAY` (otherwise `InvalidTge`); `token_mint.decimals == 8` (otherwise `InvalidMintDecimals` - the token-unit multiplier is baked into the decimals); `owner`, `admin`, `operator` `≠ Pubkey::default()` (otherwise `InvalidAuthority`; the roles coinciding with one another is allowed - §8); `withdrawal_authority ≠ Pubkey::default()` (insurance, see above) **and** `withdrawal_authority ≠ vault_authority` (the address of the storages' PDA owner, from the context), otherwise `InvalidAuthority`; `withdrawal_authority` on the Ed25519 curve, otherwise `PayeeNotOnCurve` - checked last, so the vaults' PDA keeps its own error. The `withdrawal_authority != vault_authority` gate has **two** reasons, both mandatory:
  1. if they were equal, the $1WIN remainder and the rent from a cancel or a close would end up in the program's own custody, from which no instruction withdraws them (I12);
  2. if they were equal, the condition `beneficiary_token.owner == config.withdrawal_authority` (§9.7) would admit **any other** activity's storage - every storage belongs to `vault_authority` - and one activity's remainder would be withdrawn into another's storage. The gate is therefore part of the activity isolation (I2), not only of I12.

  The decimals are checked **only here**: from then on the mint is pinned by `address = config.token_mint`, and a mint's decimals are immutable, so a repeat check in another instruction would be unreachable.
- **The errors:** `NotUpgradeAuthority`, `InvalidAuthority` (a zero role address, a `withdrawal_authority` equal to `vault_authority`; a zero `withdrawal_authority` - insurance, unreachable through the public interface), `PayeeNotOnCurve` (an off-curve `withdrawal_authority`), `InvalidMintDecimals`, `InvalidTge`, `MathOverflow`.
- **The event:** `Initialized{owner, admin, operator, token_mint, withdrawal_authority, tge_ts}`.

| Account | Attributes |
|---|---|
| `authority: Signer` | `mut` - the `init` payer |
| `vesting_program: Program<Vesting>` | `constraint = vesting_program.programdata_address()? == Some(program_data.key()) @ NotUpgradeAuthority` |
| `program_data: Account<ProgramData>` | `constraint = program_data.upgrade_authority_address == Some(authority.key()) @ NotUpgradeAuthority` |
| `config: Account<VestingConfig>` | `init, seeds = [b"config"], bump, payer = authority, space = 8 + VestingConfig::INIT_SPACE` |
| `withdrawal_authority: SystemAccount` | no constraint - the type is the check, and the body adds the `validate_payee` gates (≠ `vault_authority`, on the curve); the address is written into the config |
| `vault_authority: UncheckedAccount` | `seeds = [b"vault_authority"], bump` |
| `token_mint: Account<Mint>` | no constraint - the address is the source of truth and is recorded in the config; the decimals are checked here too |
| `system_program: Program<System>` | - |

The program slot is called `vesting_program` rather than `program`: the name `program` is taken by the helper slot that `#[event_cpi]` adds; the helper slots come last, in the order `event_authority`, then `program`. `vault_authority` is neither created nor read during `initialize` (a dataless PDA serving as the storages' authority), but it is declared with `bump` so that Anchor computes the canonical bump for the `vault_authority_bump` cache (§6.3).

### 9.2 `set_tge(tge_ts)`

- **Who:** Admin (`has_one = admin`, otherwise `NotAdmin`). Owner does not hold this right.
- **The condition:** `now < config.tge_ts` (otherwise `ScheduleStarted`, I5). The new value: `tge_ts > now` and `tge_ts ≤ now + MAX_TGE_DELAY` (otherwise `InvalidTge`).
- **The context:** `config` (mut, seeds `[b"config"]`, `bump`, `has_one = admin`), admin (Signer), event-cpi.
- **The effects:** `config.tge_ts = tge_ts`.
- **`MAX_TGE_DELAY` = 31,536,000 s (365 × 86400)** is a sanity ceiling on the `tge_ts` input: it cuts off an obviously erroneous far-future date (in particular a timestamp in milliseconds, which pushes TGE tens of thousands of years out - such a TGE will never arrive, and activities can no longer be cancelled once it is set) and bounds the window in which Owner unilaterally cancels a funded activity - for one set term: repeated `set_tge` calls before TGE arrives extend that window (§12.2). No calculation in §10 is derived from the constant.
- **The errors:** `NotAdmin`, `ScheduleStarted`, `InvalidTge`, `MathOverflow` (the ceiling is computed as `now + MAX_TGE_DELAY` through `checked_add`).
- **The event:** `TgeUpdated{tge_ts}`.

### 9.3 `set_admin(admin)` / `set_operator(operator)`

The two instructions share the `OwnerUpdate` context and differ only in the field they write.

**`set_admin(admin)`**

- **Who:** Owner (`has_one = owner`). One-step, at any time (including moving the role onto a multisig): Admin moves no funds and creates no accounts, and the sitting Owner fixes a wrong assignment themselves, so confirmation by the new key is not required - the same rationale as for `set_operator`.
- **The condition:** `admin != Pubkey::default()` (otherwise `InvalidAuthority`); coinciding with Owner or Operator is allowed (§8).
- **The context:** `config` (mut, seeds `[b"config"]`, `bump`, `has_one = owner`), owner (Signer), event-cpi.
- **The effects:** `config.admin = admin`. The previous key loses the right to `set_tge`, `set_recipient` and `set_withdrawal_authority` with that same call.
- **The errors:** `NotOwner`, `InvalidAuthority` (a zero address).
- **The event:** `AdminUpdated{admin}`.

**`set_operator(operator)`**

- **Who:** Owner (`has_one = owner`). One-step, at any time (including moving the role onto a multisig): the operator is not the top point of control, and the sitting Owner fixes a wrong assignment themselves, so confirmation by the new key is not required.
- **The condition:** `operator != Pubkey::default()` (otherwise `InvalidAuthority`); coinciding with Owner or Admin is allowed (§8).
- **The context:** the same as `set_admin`'s.
- **The effects:** `config.operator = operator`.
- **The errors:** `NotOwner`, `InvalidAuthority` (a zero address).
- **The event:** `OperatorUpdated{operator}`.

### 9.4 The Owner handover: `propose_owner(new_owner)` / `cancel_owner_proposal()` / `accept_owner()`

The handover of ownership takes two steps, and the second is executed by the new key. Ownership passes only after the candidate has signed `accept_owner`: the signature *is* the proof that the key exists and is available. A wrong address therefore never takes control - there is nothing to take over with under such a proposal, the rights stay with the sitting Owner, and they name a candidate again.

**`propose_owner(new_owner)`**

- **Who:** Owner (`has_one = owner`), at any time.
- **The condition:** `new_owner != Pubkey::default()` (otherwise `InvalidAuthority`); the sitting Admin or Operator may be the candidate (§8).
- **The context:** `config` (mut, seeds `[b"config"]`, `bump`, `has_one = owner`), owner (Signer), event-cpi.
- **The effects:** `config.pending_owner = Some(new_owner)`. Calling again overwrites the candidate - the config never holds more than one proposal.
- **The errors:** `NotOwner`, `InvalidAuthority` (a zero address).
- **The event:** `OwnerProposed{pending_owner}`.

**`cancel_owner_proposal()`**

- **Who:** Owner (`has_one = owner`), at any time.
- **The condition:** `config.pending_owner` is set, otherwise `NoPendingOwner`. The absence of a proposal is a rejection, not a silent success.
- **The context:** the same as `propose_owner`'s.
- **The effects:** `config.pending_owner = None`.
- **The errors:** `NotOwner`, `NoPendingOwner`.
- **The event:** `OwnerProposalCancelled{}`.

**`accept_owner()`**

- **Who:** the named candidate - the signer of the `new_owner` slot. There is no `has_one = owner` in the context: the instruction is not signed by the sitting Owner, so the body checks the right against the `config.pending_owner` field.
- **The condition:** `config.pending_owner` is set (otherwise `NoPendingOwner`); the signer equals it (otherwise `NotOwner`). There are no other conditions: a candidate who became Admin or Operator between the two steps takes ownership and keeps the earlier role.
- **The context:** `config` (mut, seeds `[b"config"]`, `bump`), `new_owner` (Signer), event-cpi.
- **The effects:** `config.owner = pending_owner`, `config.pending_owner = None`. The `admin` and `operator` roles are untouched.
- **The errors:** `NoPendingOwner`, `NotOwner`.
- **The event:** `OwnerUpdated{new_owner}`.

### 9.5 `create_vesting(reference_amount, tge_bps, monthly_bps, cliff_months, label)`

- **Who:** Owner (`has_one = owner`; also the `init` payer).
- **The condition:** `now < config.tge_ts` (otherwise `ScheduleStarted`, I6; why this gate is load-bearing is in §4.2).
- **The arguments:** `reference_amount`, `tge_bps`, `monthly_bps`, `cliff_months`, `label`. The recipient is **not** passed as an argument - it is supplied as the `recipient` account.
- **The context:** the slots run in the order `config`, `owner`, `vesting`, the storage, `recipient`, `vault_authority`, `token_mint`, the Token Program, the System Program, `rent`, then the `#[event_cpi]` helper slots. `config` is declared **first**: Anchor resolves the derivations in declaration order, and the seeds of both `init`s read its `activity_count` (§6.2). The attributes: `config` - `mut` (the counter increment), seeds `[b"config"]`, `bump`, `has_one = owner`; `owner` - `Signer`, `mut` (the payer of both `init`s); `vesting` - `init`, seeds `[b"vesting", config.activity_count.to_le_bytes()]`, `bump`, `payer = owner`, `space = 8 + Vesting::INIT_SPACE`; the storage - `init`, seeds `[b"vault", config.activity_count.to_le_bytes()]`, `bump`, `payer = owner`, `token::mint = token_mint`, `token::authority = vault_authority`; `recipient` - a `SystemAccount` with no attributes (the type is the check); `vault_authority` - an `UncheckedAccount`, seeds `[b"vault_authority"]`, `bump = config.vault_authority_bump`; `token_mint` - `Account<Mint>`, `address = config.token_mint`; `rent` - `Sysvar<Rent>`, required by the `init` of a token account.
- **The recipient is supplied as an account, not as a `Pubkey` argument.** The `recipient` slot in `claim` is typed `SystemAccount` (§9.8): a recipient not belonging to the System Program (a token account, a PDA with data) would make the claim structurally impossible - the payout would stall until the recipient was reassigned, and after TGE the activity is also uncancellable (I6), i.e. `set_recipient` would be the only path to a fix (§9.6). The type is therefore placed on the earliest write path where the address enters the state: `create_vesting` and `set_recipient` take the recipient as a `SystemAccount`, and the stored `vesting.recipient` is guaranteed compatible with the `claim` slot (the accounts-untrusted rule: an address held in state is revalidated on every write path and supplied as a typed account). `recipient.key()` is what goes into the `VestingCreated` event (§11). The consequence for the zero-address check: `Pubkey::default()` coincides with the System Program's address, an account at that address belongs to the loader rather than to the System Program, and so the slot's type rejects it during context resolution (`AccountNotSystemOwned`) - before the instruction body. The `recipient.key() != Pubkey::default()` check in the body **stays** as insurance against a refactor of the type (fail-closed), but it is unreachable through the public interface: its negative test is written the same way as the one for the `tge_ts > 0` gate (§4.3). The type does not prove that a key stands behind the address - an address with no account behind it is attributed to the System Program - so both write paths also require the recipient to lie on the Ed25519 curve (`validate_payee`, otherwise `PayeeNotOnCurve`): a program-derived address is off-curve by construction and no private key can sign for it, so a claim to it would create an ATA nobody can move funds out of. It is the construction of the buyer check in Sale (`WalletNotOnCurve`), for the same reason: neither a signature (a program wallet signs through its program) nor the account's owner tells a program wallet from an ordinary one.
- **The effects:** `activity_id = config.activity_count`; `vesting = {recipient, reference_amount, claimed: 0, tge_bps, monthly_bps, cliff_months}`; an empty storage is created; `config.activity_count += 1` (checked, I8).
- **Validation (in check order; the quantities are in §10.1):** `recipient.key() != Pubkey::default()` (otherwise `InvalidAuthority`, I4); `recipient.key() != vault_authority` (the address of the storages' PDA owner, from the context), otherwise `InvalidAuthority` - the `SystemAccount` type lets an uncreated PDA through (a non-existent account's owner is the System Program), and `vault_authority` is gated explicitly because it is the signing PDA that custodies the storages: a recipient at its address would mean paying out into an ATA under the program's custody, from which no instruction withdraws it; `recipient.key()` on the Ed25519 curve, otherwise `PayeeNotOnCurve` - checked after the two above, so `vault_authority` keeps its own error; it cuts off every other program-derived address: the internal `__event_authority`, which is never created, the addresses of future activities `["vault", N]` / `["vesting", N]`, which are created together with activity `N`, a multisig vault, any PDA of a foreign program. An on-curve address with no key behind it (a typo) is not detectable on-chain and is covered by the operational check of the recipients: a registry of known project addresses **and** proof of control over each address before TGE (§12.2); `reference_amount > 0` (otherwise `InvalidReferenceAmount`); the length of `label` is at most `MAX_LABEL_LEN` **bytes** (not characters; otherwise `InvalidParams`): the argument is not stored on-chain but is bounded at the entry to keep the event and transaction size in check (the input-validation rule); `tge_bps ≤ 10000`, `monthly_bps ≤ 10000` (otherwise `InvalidBps`); the share pairing `monthly_bps == 0 ⟹ tge_bps == 10000` (a "100% at TGE" activity) and `monthly_bps > 0 ⟹ tge_bps < 10000`, otherwise `InvalidSchedule` - other combinations never unlock the amount in full; `monthly_bps == 0 ⟹ cliff_months == 0`, otherwise `InvalidSchedule`: a cliff without monthly tranches is read by no branch of `vested()` (§10.2), and a meaningful input must not be silently ignored; when `monthly_bps > 0` the value `cliff_months == 1` is likewise rejected with `InvalidSchedule` (`cliff_months == 0` and `cliff_months ≥ 2` are allowed): `vested()` treats a cliff of 0 and of 1 identically (`s = max(cliff_months, 1)`, §10.1), so a one-month cliff is meaningless and would be silently ignored - the same reading of an ambiguous input as `cliff_months != 0` with `monthly_bps == 0`. **The number of monthly tranches `M` and the schedule ceiling are computed only in the `monthly_bps > 0` branch:** `M = ceil((10000 − tge_bps)/monthly_bps)` and `cliff_months + M ≤ MAX_SCHEDULE_MONTHS` is required (otherwise `InvalidSchedule`); with `monthly_bps == 0` there are no monthly tranches, `M` is not computed - a division by zero cannot arise by construction and the schedule ceiling holds degenerately. `token_mint` is bound structurally - `address = config.token_mint`, with no separate check in the body; the decimals are **not** checked here - they are checked only in `initialize` (§9.1): the mint is pinned by the address from the config and its decimals are immutable, so a repeat check would be unreachable.
- **`MAX_SCHEDULE_MONTHS` = 120** is a sanity ceiling on the schedule input: `cliff_months + M ≤ MAX_SCHEDULE_MONTHS`, with the sum computed in u32 (it overflows u16). It cuts off schedules hundreds of years long: `monthly_bps = 1` gives `M = 10000` (≈833 years). The longest bucket in §10.5 is Treasury, `cliff 9 + M 25 = 34`. Neither an activity's deadline nor the terminal period's number is derived from the constant - `vested()` does not compute them (§10.2).
- **The errors:** `NotOwner`, `ScheduleStarted`, `InvalidAuthority` (a zero `recipient` - insurance, unreachable through the public interface; `recipient == vault_authority`), `PayeeNotOnCurve` (an off-curve `recipient`), `InvalidReferenceAmount`, `InvalidBps`, `InvalidSchedule`, `InvalidParams` (the length of `label`), `MathOverflow`.
- **The event:** `VestingCreated{activity_id, recipient, reference_amount, tge_bps, monthly_bps, cliff_months, label}`.

### 9.6 `set_recipient(activity_id)`

- **Who:** Admin (`has_one = admin`, otherwise `NotAdmin`). Owner does not hold this right.
- **The condition:** `new_recipient.key() != Pubkey::default()` (otherwise `InvalidAuthority`, I4); `new_recipient.key() != vault_authority` (otherwise `InvalidAuthority`) - the same gate as in `create_vesting` (§9.5): the `SystemAccount` type lets an uncreated PDA through, and a recipient at the `vault_authority` address would mean paying out into an ATA under the program's custody; `new_recipient.key()` on the Ed25519 curve, otherwise `PayeeNotOnCurve` - the same curve check as in `create_vesting` (§9.5), cutting off every other program-derived address.
- **There is no time condition** (VE-9): the change works before TGE, at TGE and after it, including on an activity part of whose reference has already been paid out. No address is committed to anywhere - every claim reads the current field (`address = vesting.recipient`, §9.8), so the next payout goes to the new wallet while what was paid out earlier stays with the previous one. The bookkeeping is untouched by the change: `claimed` belongs to the activity, not to a wallet, so the new recipient receives exactly the undrawn remainder and the ceiling `claimed ≤ reference_amount` (I1) is preserved. The freeze that arrives with TGE (I6) covers the set of activities and their amounts, but not the payout address. There is no "the new recipient differs from the current one" check: calling again with the same address has no effect and does no harm. The flip side is a permanent redirection lever in Admin's hands; it is accepted as a risk (§12.2), and the observability rests on the `RecipientUpdated` event.
- **The arguments:** `activity_id`. The new recipient is **not** passed as an argument - it is supplied as the `new_recipient` account (a `SystemAccount`), by the same rule as in `create_vesting` (§9.5): both write paths that set `vesting.recipient` place the same type and the same `validate_payee` gates, so an address incompatible with the `recipient` slot in `claim`, or one no key can sign for, never enters the state. The zero-address check behaves exactly as in §9.5: the type rejects `Pubkey::default()` before the body and the `require!` remains insurance against a refactor.
- **The context, the slots in this order:** `config` (seeds `[b"config"]`, `bump`, `has_one = admin`; not `mut` - the instruction does not change the config), `vesting` (mut, seeds `[b"vesting", activity_id_le]`, `bump`), `new_recipient` (a `SystemAccount` with no attributes), `vault_authority` (an `UncheckedAccount`, seeds `[b"vault_authority"]`, `bump = config.vault_authority_bump`; supplied for the sake of the `new_recipient != vault_authority` gate), admin (Signer), event-cpi.
- **The effects:** `vesting.recipient = new_recipient.key()`.
- **The errors:** `NotAdmin`, `InvalidAuthority` (a zero address - insurance, unreachable through the public interface; `new_recipient == vault_authority`), `PayeeNotOnCurve` (an off-curve `new_recipient`).
- **The event:** `RecipientUpdated{activity_id, new_recipient}`.

### 9.7 `cancel_vesting(activity_id)` / `close_vesting(activity_id)`

The two instructions share **one mechanism for withdrawing the remainder and closing** (described below). They differ in their condition and their window:

- **`cancel_vesting`** - Owner, `now < config.tge_ts` **and** `claimed == 0` (otherwise `ScheduleStarted`, I6; on the second check, see below). For an activity created in error; other pre-TGE corrections (the amount, the schedule) are made by cancelling and creating again (VE-10). `activity_count` is **not** rolled back (I8). **The actual boundary for fixing mistakes is the mint-authority revoke, not TGE.** A recreated activity can only be filled by minting, and minting into every storage ends with the atomic transaction `mint_to × N` + `assert_funded × N` + `SetAuthority(None)` (§2), which by procedure is executed before TGE. The `now < config.tge_ts` gate formally keeps the cancel/recreate window open right up to TGE, but a fix requiring a top-up is possible only while the right to issue has not been revoked - an earlier boundary. After the revoke, a recreated activity can only be filled by a direct transfer of existing $1WIN to the storage's address, and only if the project has free volume (§12.2); it is for the sake of that emergency path that a gate on "`create_vesting` while the mint authority is revoked" is deliberately not introduced (§12.2).
- **`close_vesting`** - Owner, `claimed ≥ reference_amount` (otherwise `NotFullyClaimed`, I9). For a fully drawn activity (VE-17). The `≥` form instead of `==` gives robustness to a refactor of the formula: the ceiling `claimed ≤ reference_amount` is proven (§10.3), so in practice the gate fires on equality. While `claimed < reference_amount` it cannot be closed - the close depends on the recipient's final claim, a deliberate property (§12.2).

**`close_vesting` has no time gates and needs none** - this is a derived property of the pair of instructions, not a gap in the requirement:

- Before TGE `vested()` returns 0 (§10.2), so `claimed == 0`, while `reference_amount > 0` was checked at creation (§9.5). Hence the `claimed ≥ reference_amount` gate admits no activity before TGE - a pre-TGE condition in `close_vesting` would not add a single rejection. Adding one is **forbidden**: a fully drawn activity has to be closed precisely after TGE, and such a gate would close the normal path entirely.
- Symmetrically, `cancel_vesting` with `claimed > 0` is unreachable: it is open only before TGE (`now < config.tge_ts`, I6), and before TGE there are no claims. The branch "cancelling a partly drawn activity" is absent from the requirement not by oversight - it does not exist among the reachable states. That conclusion, however, rests on a gate in a **different** instruction (`set_tge` freezes `tge_ts` once it arrives, §9.2), so the unreachability is not left as an inference: `cancel_vesting` carries the explicit insurance `claimed == 0`, otherwise `ScheduleStarted`. This is the same fail-closed rule behind the program's other checks that are unreachable through the public interface - `tge_ts > 0` in `claim` (§4.3), the zero `withdrawal_authority` (§9.1) and recipient (§9.5, §9.6), and `beneficiary_token != vault` (below).

**The context (of both), the slots in this order.** `config` (seeds `[b"config"]`, `bump`, `has_one = owner`), `owner` (Signer, with no `mut`: the rent of both accounts being closed goes to `rent_collector`, and the instruction moves no lamports with Owner - who pays the fee as the transaction's fee payer), `vesting` (mut, `close = rent_collector`, seeds `[b"vesting", activity_id_le]`, `bump`), the storage (mut, seeds `[b"vault", activity_id_le]`, `bump`, `token::mint = token_mint`, `token::authority = vault_authority`), `beneficiary_token` (mut, the three conditions below), `rent_collector` (mut, `SystemAccount`, `address = config.withdrawal_authority`), `vault_authority` (an `UncheckedAccount`, seeds `[b"vault_authority"]`, `bump = config.vault_authority_bump`), `token_mint` (`Account<Mint>`, `address = config.token_mint`), the Token Program, the `#[event_cpi]` helper slots. Both instructions address the activity by the single `activity_id` argument (§6.2); the storage is closed with the `CloseAccount` CPI, so the `close` attribute stands only on `vesting`. Their context is shared - the slot sets and the bindings coincide entirely, and only the gate in the body tells the instructions apart; hence the three `beneficiary_token` conditions below apply in **both**.

**The shared mechanism (drain-if-nonzero → close).** Both instructions take **two** separate recipient accounts:
   - `beneficiary_token` - the $1WIN token account for the storage remainder. The `Account<TokenAccount>` type proves that it is a genuine token account; on top of the type the slot carries **three** conditions (I12), in check order:
     - **the right mint:** `beneficiary_token.mint == config.token_mint`, otherwise `InvalidMint` - an explicit condition rather than a `token::mint` attribute: a domain invariant gets a domain-specific code, whereas the attribute would return a generic Anchor error;
     - **an account under the single `withdrawal_authority`:** `beneficiary_token.owner == config.withdrawal_authority`, otherwise `InvalidBeneficiary`. The $1WIN remainder goes only to a token account whose SPL owner is the project withdrawal account fixed at `initialize` (§9.1); this excludes every other activity's storage and any third-party account, and holds fund isolation (I2) by construction. The binding is **by the owner `W = config.withdrawal_authority` rather than by a specific ATA - deliberately:** a frozen or closed ATA of that owner does not block `cancel_vesting`/`close_vesting`, because any of its token accounts with the right mint may be supplied;
     - **not this activity's storage:** `beneficiary_token.key() != vault.key()`, otherwise `InvalidBeneficiary`. Anchor rejects coinciding addresses earlier - through its own duplicate check among the mutable serializable slots (`ConstraintDuplicateMutableAccount`, 2040), which runs before any user condition; the explicit inequality stays as insurance against a change of the slot's type;
   - `rent_collector` - the project's system account for the **SOL rent** from closing the storage and the `Vesting` record. The `SystemAccount` type (so it structurally cannot be a token account - and in particular cannot be `beneficiary_token` itself) plus `address == config.withdrawal_authority`, otherwise `InvalidRentCollector` - the slot's own error variant, separate from `InvalidBeneficiary` (§9). Both withdrawal slots are bound to the same address `W`: the $1WIN remainder and the rent both go to the project's single withdrawal account.

1. **The $1WIN remainder is transferred only when `vault.amount > 0`:** a CPI transfer of the storage's whole balance `vault → beneficiary_token` signed by `vault_authority`. At a zero balance the step is **skipped** and the instruction does not fail (VE-10, VE-17). The order is mandatory: SPL Token closes a non-native token account only at a zero balance, so the drain precedes the close.
2. **Closing the storage:** the SPL `CloseAccount` signed by `vault_authority`, the rent (SOL) → `rent_collector`.
3. **Closing `Vesting`:** `close = rent_collector` (the SOL rent → `rent_collector`). Zeroing the data and reassigning to the System Program is a one-shot guard against a repeat (I8): a repeat call will not find the activity and is rejected during context resolution - `AccountNotInitialized`, or `AccountOwnedByWrongProgram` if lamports have been transferred to its address (the rejection class is in §9).

- **The remainder at close** is an overflow beyond `reference_amount` (an accidental third-party transfer, VE-17): the recipient cannot claim it (the unlock is bounded by `reference_amount`, I11), so it safely goes to the project.
- **The errors (`cancel_vesting`):** `NotOwner`, `ScheduleStarted` (TGE has arrived; the `claimed == 0` insurance is unreachable through the public interface), `InvalidMint` (a foreign mint on `beneficiary_token`), `InvalidBeneficiary` (a `beneficiary_token` with owner ≠ `config.withdrawal_authority` or one coinciding with this activity's storage), `InvalidRentCollector` (`rent_collector` ≠ `config.withdrawal_authority`).
- **The errors (`close_vesting`):** `NotOwner`, `NotFullyClaimed`, `InvalidMint` (a foreign mint on `beneficiary_token`), `InvalidBeneficiary` (a `beneficiary_token` with owner ≠ `config.withdrawal_authority` or one coinciding with this activity's storage), `InvalidRentCollector` (`rent_collector` ≠ `config.withdrawal_authority`).
- **The events:** `VestingCancelled{activity_id, amount, beneficiary_token, rent_collector}` / `VestingClosed{activity_id, amount, beneficiary_token, rent_collector}`, where `amount` is the storage remainder withdrawn (a snapshot taken before the transfer; `0` for an empty storage), and `beneficiary_token` and `rent_collector` are the addresses of the withdrawal accounts.

### 9.8 `claim(activity_id)`

The single payout instruction, one legacy transaction: authorization and time → computing `amount` from the pre-CPI state → the funding check → the transfer → the `claimed` increment. There is no intermediate state "the tokens are out but `claimed` has not grown" - a rollback at any step rolls back everything.

```
claim(activity_id):
  require signer == vesting.recipient || signer == config.operator   // otherwise NotAuthorized
  require config.tge_ts > 0                                           // otherwise ScheduleNotStarted (TGE not set)
  require now >= config.tge_ts                                        // otherwise ScheduleNotStarted
  amount = vested(now) - claimed                                      // §10.2, from the pre-CPI fields; checked
  require amount > 0                                                  // otherwise NothingToClaim
  require vault.amount >= amount                                      // otherwise InsufficientVaultBalance (a full revert)
  transfer amount $1WIN: the storage -> the recipient's ATA (signed by vault_authority)
  claimed += amount                                                   // checked
  emit Claimed{ activity_id, recipient, amount, claimed }
```

- **Who:** the activity's recipient or `operator` (§8), otherwise `NotAuthorized`; the unlocked $1WIN goes only to `recipient` (I4).
- **The context, the slots in this order:** `config` (seeds `[b"config"]`, `bump`; with no `has_one` - the right to claim belongs to one of **two** keys, a choice no attribute expresses, so the comparison happens in the body), `vesting` (mut, seeds `[b"vesting", activity_id_le]`, `bump`), the storage (mut, seeds `[b"vault", activity_id_le]`, `bump`, `token::mint = token_mint`, `token::authority = vault_authority`), `recipient` (a `SystemAccount`, `address = vesting.recipient` - the address comes from the stored field; the type cuts off existing accounts of other programs, while an uncreated PDA belongs to the System Program and passes the type - but the stored address lies on the curve by construction, so no such PDA can be there: both write paths hold the curve check and the `vault_authority` gate, §9.5/§9.6), `recipient_ata` (`init_if_needed`, `payer = signer`, `associated_token::mint = token_mint`, `associated_token::authority = recipient`, `associated_token::token_program = token_program`), `vault_authority` (an `UncheckedAccount`, seeds `[b"vault_authority"]`, `bump = config.vault_authority_bump`), `token_mint` (`Account<Mint>`, `address = config.token_mint` - a binding to the stored mint, fail-fast instead of a revert on the transfer; a foreign mint in this slot is in fact rejected earlier by the storage's `token::mint` binding - Anchor resolves the slots in declaration order and the storage is declared above, so the observable code is `ConstraintTokenMint`), `signer` (Signer, mut - the payer of the ATA rent), the Token / ATA / System programs, the `#[event_cpi]` helper slots.
- **The recipient occupies a slot of its own rather than being substituted from a field.** Binding an ATA under `init_if_needed` requires an **account** from the context in `associated_token::authority`: in the init branch Anchor assembles the account-creation CPI from it, so the form `associated_token::authority = vesting.recipient` (a `Pubkey` field) does not compile. The `recipient` slot is that account, and the condition `address = vesting.recipient` binds it to the stored recipient - the caller has no choice of address (I4).
- **The effects:** a CPI transfer of `amount` $1WIN from `the storage → the recipient's ATA` signed by `vault_authority`; `claimed += amount` (I1). A claim does **not** close the activity (`close_vesting` does that after the full claim).
- **Atomicity (pre-CPI, reload-after-CPI).** `amount` and the funding check are taken before the transfer, and `claimed += amount` is written after; the transfer does not change `claimed` in `Vesting`, so no `reload` is needed.
- **The only read of the storage's balance in `claim` (I3).** The amount is computed from the stored fields, and `vault.amount ≥ amount` is only the gate "do not hand out more than is on hand" (VE-13, protection against an under-mint, §12.2). On a shortfall everything reverts, there is no partial payout and no ATA is created: `init_if_needed` executes during context resolution, i.e. **before** the body's checks, but a revert in the body rolls the transaction back together with the account creation.
- **The ATA rent.** `init_if_needed` creates the recipient's ATA if needed; the signer pays (`payer = signer`) - either the recipient for themselves or the operator for the recipient, and that rent is never refunded to the operator (it is the operator's operational cost, VE-12). `init_if_needed` is admissible here: an ATA is **not** an account of the program's state but the recipient's standard token account (the init-and-reinit rule forbids `init_if_needed` on business-state accounts, not on ATAs).
- **The claim is repeatable and has no time limit (VE-15):** there is no deadline, and a repeat with `claimable == 0` → `NothingToClaim`. The profile: 13 static accounts (the legacy-transaction limit is 38), with the data accounts Boxed.
- **The errors:** `NotAuthorized`, `ScheduleNotStarted` (`tge_ts ≤ 0` or TGE has not arrived yet), `NothingToClaim`, `InsufficientVaultBalance`, `InvalidBps` and `InvalidSchedule` (the `tranches_m` insurance on a forged record: `tge_bps > 10000` and a degenerate `M == 0`, §10.1 - both unreachable through the public interface), `MathOverflow`.
- **The event:** `Claimed{activity_id, recipient, amount, claimed}` (`claimed` is the value after the increment).

### 9.9 `assert_funded(activity_id)`

A funding check on an activity's storage. The instruction changes nothing: it either passes or reverts the transaction it is part of.

- **Who:** anyone - the context has no `Signer` slot and no role is checked (§8). The instruction grants no new rights: it reads only the same public accounts as `claim`.
- **The condition:** none. The instruction checks neither the time nor TGE - the funding is verifiable at any moment, including before TGE.
- **The context, the slots in this order:** `config` (seeds `[b"config"]`, `bump`), `vesting` (seeds `[b"vesting", activity_id_le]`, `bump`), the storage (seeds `[b"vault", activity_id_le]`, `bump`, `token::mint = token_mint`, `token::authority = vault_authority`), `vault_authority`, `token_mint` (`Account<Mint>`, `address = config.token_mint`). Not a single `mut`, not a single `Signer`, no `#[event_cpi]` helper slots (the instruction emits no events), and `remaining_accounts` are not read.
- **The effects:** none - no state changes and no lamports move.
- **Validation:** `vault.amount + vesting.claimed ≥ vesting.reference_amount`, otherwise `InsufficientVaultBalance`. The sum of two u64 values is computed in u128 through `checked_add`: its result always fits in u128, so the `MathOverflow` branch is unreachable through the public interface and stays as insurance against a refactor. Saturating arithmetic is forbidden in this place: on saturation the sum would clamp at the u128 ceiling and the coverage check would pass - the rejection must be fail-closed, because this is the only gate standing before the mint-authority revoke (§2).
- **Why what is covered is the remainder of the obligation and not the whole reference.** The property being checked is the activity's solvency: the storage must cover the **undrawn** part of the reference, i.e. `reference_amount − claimed`. The absolute form `vault.amount ≥ reference_amount` is correct only while `claimed == 0`: after the first claim the balance is reduced by what was paid out, and a fully funded activity would report itself as underfunded. Before the first claim both forms are identical (`claimed == 0`), so the mint transaction of §2 - where the activities exist but have never been claimed - is unaffected.
- **Its purpose:** the instruction is included in the atomic mint transaction (§2), one per activity, between `mint_to` and the closing `SetAuthority(None)`. An underfunded storage brings the whole transaction down, so the irreversible state "the mint authority was revoked while a storage was underfunded" never arises (§12.1, §12.2). Outside that transaction the instruction serves as an on-chain funding check at any moment - including after some of the claims.
- **The errors:** `InsufficientVaultBalance`, `MathOverflow` (the `checked_add` insurance on the coverage sum - unreachable through the public interface).
- **The event:** none - the instruction changes no state and the indexer has nothing to record (§11).

**`InsufficientVaultBalance` serves two invariants - the merging is deliberate.** The same variant is returned by the payout gate in `claim` (`vault.amount ≥ amount`, I3, §9.8) and by the solvency gate in `assert_funded` (`vault.amount + claimed ≥ reference_amount`, §9.9). Splitting it is impossible: the order of the variants in the single `#[error_code] enum` is frozen and defines the codes 6000-6018 (TRD §7.8). The meaning of the violation is told apart by the instruction's context: in `claim` it is "there is nothing to pay out right now", in `assert_funded` it is "the undrawn remainder of the obligation is not covered". Operationally they are told apart by the index of the failing instruction in the transaction: in the mint transaction (§2) the code comes from `assert_funded` on a specific activity, outside it - from `claim`.

### 9.10 `set_withdrawal_authority()`

Reassigning the project's single withdrawal account - the address that receives the $1WIN remainder and the SOL rent when an activity is cancelled or closed (§9.7, I12).

- **Who:** Admin (`has_one = admin`, otherwise `NotAdmin`). Owner does not hold this right: the withdrawal destination is a configuration parameter, like the TGE date and an activity's recipient (§8). A cancel and a close are still signed by Owner (§9.7) - Owner moves the funds and Admin sets where they go.
- **There are no arguments.** The new withdrawal target is supplied as an account - the `withdrawal_authority` slot is typed `SystemAccount`, the same construction, the same type and the same `validate_payee` gates as in `initialize` (§9.1).
- **The condition:** `withdrawal_authority ≠ Pubkey::default()` (insurance: the zero address is rejected by the slot's type during context resolution, `AccountNotSystemOwned`) **and** `withdrawal_authority ≠ vault_authority` (otherwise `InvalidAuthority`) **and** `withdrawal_authority` on the Ed25519 curve (otherwise `PayeeNotOnCurve`, checked last). All three checks are the ones from `initialize` (§9.1); the `vault_authority` gate stands for the same two reasons: at the storages' PDA-owner address the remainder and the rent would end up in the program's own custody, from which no instruction withdraws them (I12), and the condition `beneficiary_token.owner == config.withdrawal_authority` (§9.7) would admit any other activity's storage (I2).
- **There is no time condition.** The instruction is open both before TGE and after: the field's value is not committed to anything - every cancel and every close reads it at its current value (I12). A change affects no activity: the schedule, the recipient and the payout bookkeeping stay as they are.
- **The context:** `config` (mut, seeds `[b"config"]`, `bump`, `has_one = admin`), `withdrawal_authority` (a `SystemAccount`, with no constraint - the type is the check, the body adds the `validate_payee` gates), `vault_authority` (an `UncheckedAccount`, seeds `[b"vault_authority"]`, `bump = config.vault_authority_bump`), admin (Signer), event-cpi.
- **The effects:** `config.withdrawal_authority` = the slot's address. A token account created earlier for the previous target stops satisfying `beneficiary_token.owner == config.withdrawal_authority`, and the previous target itself stops satisfying `rent_collector.address == config.withdrawal_authority` (§9.7).
- **One-step**, with no `propose` / `accept`: a wrong address is fixable by calling again before the first activity close and, unlike an ownership handover, it does not take away the very possibility of a fix - the same rationale as for `set_admin` and `set_operator` (§9.3).
- **The errors:** `NotAdmin`, `InvalidAuthority` (the address equals `vault_authority`; a zero address - insurance, unreachable through the public interface), `PayeeNotOnCurve` (an off-curve address).
- **The event:** `WithdrawalAuthorityUpdated{withdrawal_authority}`.

| Account | Attributes |
|---|---|
| `config: Account<VestingConfig>` | `mut, seeds = [b"config"], bump, has_one = admin @ NotAdmin` |
| `withdrawal_authority: SystemAccount` | no constraint - the type is the check, and the body adds the `validate_payee` gates (≠ `vault_authority`, on the curve); the address is written into the config |
| `vault_authority: UncheckedAccount` | `seeds = [b"vault_authority"], bump = config.vault_authority_bump` |
| `admin: Signer` | - |

---

## 10. The unlock math

### 10.1 The scale, the shares and the number of tranches

- $1WIN has 8 decimal places (1 $1WIN = 10⁸ token units). `reference_amount` is stored in token units; the check `token_mint.decimals == 8` lives in `initialize` (§9.1), the one and only time in the program's life.
- The shares are in basis points (bps): `10000 = 100%`. `tge_bps` is the share at TGE and `monthly_bps` the monthly share.
- A month is fixed at **30 days**, `MONTH = 2,592,000 s`. Calendar months of differing lengths are deliberately not used: a 30-day month makes `vested()` a deterministic function of a single `now` with no calendar logic (the residual drift is in §12.2). Period `n` arrives at `tge_ts + n·MONTH`: `n = 0` is the TGE moment (day 1), `n = 1` is day 31.
- `s = max(cliff_months, 1)` is the period of the **first** monthly tranche. With no cliff, `s = 1` (the first tranche on day 31); a 9-month cliff → `s = 9` (the first tranche on day 271). `cliff_months == 0` and `cliff_months == 1` would give the **same** schedule (`s = 1`), so when `monthly_bps > 0` the value `cliff_months == 1` is rejected at the entry with `InvalidSchedule` (§9.5) as an ambiguous input: a one-month cliff is meaningless and would be silently ignored. The form `s = max(cliff_months, 1)` remains in `vested()` as insurance (fail-closed); it is only ever reached by the admissible `cliff_months == 0` and `cliff_months ≥ 2`. The identity of zero and one is pinned by the fixture test (`programs/vesting/src/math/tests.rs`) - the form `s = cliff_months + 1` would shift every cliffless schedule by a month.
- `M = ceil((10000 − tge_bps) / monthly_bps)` when `monthly_bps > 0` is the number of monthly tranches. When `monthly_bps == 0` it is a "100% at TGE" activity and `M` is undefined (the branch with no monthly tranches). The degenerate result `M == 0` (which means `tge_bps == 10000` with `monthly_bps > 0`) is rejected by the computation of `M` as `InvalidSchedule`: that combination is already forbidden at the entry (§9.5), and inside `vested()` with `M == 0` the condition `months == M` would be true immediately and would return the whole reference - the rejection must be fail-closed, so the insurance closes the class rather than a single combination.

The unlock `vested()` is a **pure function** of the activity's fields and `config.tge_ts`: it reads no accounts and no storage balance (VE-4), which is why it is unit-tested on boundary time values separately from the on-chain wiring. The off-chain indexer computes by the same formula (§11). Its readings agreeing with the program's payout is a **convention, not a structural property**: the indexer reimplements the formula in its own language, and a divergence in a boundary condition is not caught by the on-chain tests. The canonical implementation is available to Rust consumers as the public `vested` function of the program crate's `math` module; the maintenance requirement for the formula is in §11.

### 10.2 The `vested()` formula

The values are u64 and the intermediates u128, downcast to u64 after the divisions (§10.4):

```
vested(now):
  if now < config.tge_ts:  return 0                                // before TGE - nothing
  if monthly_bps == 0:     return reference_amount                 // a "100% at TGE" activity
  p = (now − config.tge_ts) / MONTH                                // floor, the period number
  months = 0 if p < s else min(p − s + 1, M)                       // the number of monthly tranches accrued
  if months == M:          return reference_amount                 // the last tranche tops up to the reference
  tge_amt     = reference_amount * tge_bps  / 10000                // floor
  monthly_amt = reference_amount * monthly_bps / 10000             // floor
  return tge_amt + months * monthly_amt

claimable(now) = vested(now) − claimed
```

- With `tge_ts ≤ 0` the formula never applies - the claim is cut off earlier (§4.3, I5).
- `p < s`: the cliff (or the time before the first tranche) - the monthly tranches have not accrued; only `tge_amt` has been handed out (when `tge_bps > 0`), otherwise 0.
- `months == M`: the terminal branch - it returns **exactly** `reference_amount` (§10.3).

### 10.3 The rounding directions and the proof of the ceiling

| Quantity | Direction | Why |
|---|---|---|
| `tge_amt`, `monthly_amt` | down (floor) | the payout ≤ the share; the storage is never overdrawn |
| the terminal amount (`months == M`, `monthly_bps == 0`) | exactly `reference_amount` | the remainders dropped by flooring (< 1 token unit per tranche) are given out whole in the last tranche, and the unlock reaches the reference exactly (example V4) |

**The ceiling `claimed ≤ reference_amount` (always).** A claim changes `claimed` only by the step `claimed += (vested(now) − claimed)`, i.e. after a claim `claimed = vested(now)` (I1). It is enough to show that `vested` is monotonically non-decreasing and bounded above by `reference_amount`:

- **Monotonicity.** `p` is non-decreasing in `now`; `months = min(max(p − s + 1, 0), M)` is non-decreasing in `p`; `tge_amt + months × monthly_amt` is non-decreasing in `months`; and the terminal branches return `reference_amount`, which is no less than the previous value. Hence `vested` is non-decreasing (I11).
- **The upper bound.** At a non-terminal step `months ≤ M − 1`, and `M = ceil((10000 − tge_bps)/monthly_bps)` gives `tge_bps + (M−1)·monthly_bps < 10000`. Then the exact share `tge_bps + months·monthly_bps < 10000`, and the floored sums do not exceed it: `tge_amt + months·monthly_amt ≤ floor(reference_amount × (tge_bps + months·monthly_bps) / 10000) < reference_amount` (strictly less). The terminal branches (`monthly_bps == 0` and `months == M`) give **exactly** `reference_amount`.

Hence `claimed ≤ reference_amount` always holds, and `claimed == reference_amount` is reachable by **exactly one** claim in the terminal period (`p ≥ s + M − 1`). That is what lets `close_vesting` gate on `≥` (fail-closed, equivalent to `==` given the proven ceiling, I9).

### 10.4 Overflow and division by zero

The products (`reference_amount × tge_bps`, `reference_amount × monthly_bps`, `months × monthly_amt`) are computed in u128 (max `reference_amount × 10000 ≈ 1.8·10²³ ≪ the u128 limit`), with the u128 → u64 downcast done by `u64::try_from` after the divisions. Every operation is `checked_*`; an overflow → `MathOverflow` (I7). Division by zero is ruled out: `10000` and `MONTH` are non-zero constants, and the division by `monthly_bps` (in `M`) happens only in the `monthly_bps > 0` branch. The difference `now − config.tge_ts` is computed only when `now ≥ config.tge_ts` (checked by `claim`), so it is non-negative; on reachable values it does not exceed `now`, because that same `claim` requires `tge_ts > 0` (§4.3). The class "a forged non-positive `tge_ts`" is closed by precisely that gate, not by the arithmetic: at a moderate negative value the difference fits in i64 and does not overflow. `checked_sub` remains insurance at the edge of the range - when the difference leaves i64 the result is `MathOverflow`, not a panic. The sanity ceilings on input (§9) take no part in the calculation. The difference `p − s + 1` is computed **only in the `p ≥ s` branch** (with `p < s`, `months = 0` immediately, with no subtraction), so an unsigned underflow of `p − s` cannot arise.

### 10.5 The bucket parameters

The periods are counted as `day = 1 + n·30` (§10.1). The schedule values come from VE-3 / TRD §7.5; each bucket's `reference_amount` is set by the tokenomics at creation (it is stored on-chain and is not reproduced in the table).

| Bucket | `tge_bps` | `monthly_bps` | `cliff_months` | `M` | 1st monthly tranche | Fully paid out |
|---|---|---|---|---|---|---|
| Marketing | 10000 | 0 | 0 | 0 | - | the start (day 1) |
| Ecosystem | 1000 | 500 | 0 | 18 | day 31 | day 541 |
| Community Rewards | 0 | 834 | 0 | 12 | day 31 | day 361 |
| Treasury | 0 | 400 | 9 | 25 | day 271 | day 991 |
| Liquidity | 1000 | 750 | 0 | 12 | day 31 | day 361 |

Checking `M` (when `monthly_bps > 0`): Ecosystem `ceil(9000/500)=18`; Community `ceil(10000/834)=12`; Treasury `ceil(10000/400)=25`; Liquidity `ceil(9000/750)=12`. The full payout falls in period `p = s + M − 1`, on day `1 + (s+M−1)·30`.

---

## 11. The events

`#[event_cpi]` + `emit_cpi!` (the event is written as a nested instruction): providers truncate ordinary program logs but not nested instructions; the cost is ~1-2k CU. The events are an auxiliary layer: monetary correctness is held by the accounts and the invariants of §3, and losing an event does not break it.

| Event | Instruction | Fields |
|---|---|---|
| `Initialized` | initialize | owner, admin, operator, token_mint, withdrawal_authority, tge_ts |
| `TgeUpdated` | set_tge | tge_ts |
| `AdminUpdated` | set_admin | admin |
| `OperatorUpdated` | set_operator | operator |
| `OwnerProposed` | propose_owner | pending_owner |
| `OwnerProposalCancelled` | cancel_owner_proposal | - |
| `OwnerUpdated` | accept_owner | new_owner |
| `WithdrawalAuthorityUpdated` | set_withdrawal_authority | withdrawal_authority |
| `VestingCreated` | create_vesting | activity_id, recipient, reference_amount, tge_bps, monthly_bps, cliff_months, label |
| `RecipientUpdated` | set_recipient | activity_id, new_recipient |
| `VestingCancelled` | cancel_vesting | activity_id, amount, beneficiary_token, rent_collector |
| `Claimed` | claim | activity_id, recipient, amount, claimed |
| `VestingClosed` | close_vesting | activity_id, amount, beneficiary_token, rent_collector |

There are thirteen events against fourteen instructions: `assert_funded` emits none, as it changes no state (§9.9). The ownership handover is observable through three events: the candidate is named, the candidate is withdrawn, the candidate takes over.

- **`claimable` is not read on-chain (VE-16).** There is no separate read instruction: the indexer computes `claimable(now) = vested(now) − claimed` by the formula of §10.2 from the public fields of `config.tge_ts` and of the activity (`reference_amount`, `claimed`, `tge_bps`, `monthly_bps`, `cliff_months`). Before TGE and when `claimed == vested` it shows 0 - consistently with `claim`, which reverts in those cases (`ScheduleNotStarted` / `NothingToClaim`).
- **The formula of §10.2 is part of the program's external contract on a par with the IDL.** Changing it - including editing a boundary condition - must be accompanied by notifying the consumers of the off-chain computation (the indexer and the admin console), because their implementation of the formula is independent and a divergence is not caught by the on-chain tests (§10.1). The requirement mirrors the one already in force for the IDL, whose immutability is watched by the vendored-file check in CI. Rust consumers have the canonical entry point available as the public `vested` function of the `math` module.

---

## 12. Security

### 12.1 The map of defences

Every threat is closed structurally - by an invariant from §3 or by a mechanism in the section named.

| Threat | Where it is closed |
|---|---|
| The operator redirects a claim to their own account | I4, §9.8 |
| An activity with a zero recipient (`recipient == Pubkey::default()`) → the claim goes irretrievably into an ATA with no private key | I4, §9.5 |
| Handing out more than what is unlocked / more than the storage's balance | I1, I3, §9.8 |
| The irreversible state: the mint authority revoked while a storage is underfunded | `assert_funded` in the same transaction as `mint_to` and `SetAuthority(None)` (§9.9, §2) |
| The recipient is set to an address belonging to another program, or to a program-derived address nobody holds a key for (a multisig vault, a PDA of any program, this program's own future activity PDAs) | The recipient is supplied as a `SystemAccount` on both write paths (`create_vesting` §9.5, `set_recipient` §9.6) and read with the same type in `claim` (§9.8): an existing program-owned account is cut off by the type; an address with no account behind it passes the type and is cut off by the curve check in `validate_payee` (`PayeeNotOnCurve`) - a program-derived address is off-curve by construction |
| A double claim / a repeat after a close | a closed account is rejected during context resolution (`AccountNotInitialized` or `AccountOwnedByWrongProgram`, §9); `activity_id` is never reused (I8) |
| An activity created after TGE → instantly unlocked and "stuck" | I6, §4.2 |
| An unset TGE (zero or a negative value) opening an instant unlock | I5, §4.3 |
| Moving TGE retroactively | I5, §4.2 |
| Cancelling an activity after TGE | I6, §4.2 |
| The Admin role holder redirecting the undrawn remainder and the withdrawal account | Not closed structurally - an accepted risk, §12.2 (the safeguard: the Ops MS quorum and monitoring the `RecipientUpdated` and `WithdrawalAuthorityUpdated` events) |
| Arithmetic overflow | I7, §10.4 |
| Substituting a storage / paying out from another activity | I2, §6.2 |
| The vesting funds being reachable by another program | I10, §7 (while the bytecode is unchanged) |
| The upgrade-authority holder replacing the logic and gaining control over every storage (a super-role) | Outside structural protection: I10 holds only **while the bytecode is unchanged**. Accepted operationally - the upgrade authority is with Owner MS (§8, §9.1) and immutability is not planned; the trust boundary and the procedure are in §12.2 |
| Redirecting the remainder/rent on a cancel or a close - including the remainder into another activity's storage (breaking isolation I2) or to a third-party account | I12, §9.7: both withdrawal slots are bound to `config.withdrawal_authority` (the remainder - `beneficiary_token.owner == withdrawal_authority` and `≠ the storage`; the rent - `rent_collector.address == withdrawal_authority`) |
| A withdrawal target under the program's custody, a token account or a program-derived address nobody holds a key for (`withdrawal_authority` = a program PDA, existing or not, an ATA, another program-owned account) | The `withdrawal_authority` slot is typed `SystemAccount` in both instructions that write it - `initialize` and `set_withdrawal_authority`: an existing program-owned account is cut off during context resolution, before the instruction body (§9.1, §9.10). The type closes only accounts that already exist - a non-existent one's owner is the System Program, so it lets an uncreated PDA through; those are closed by the curve check in `validate_payee` (`PayeeNotOnCurve`) - a program-derived address is off-curve by construction - with the storages' custody PDA additionally held by the explicit `withdrawal_authority != vault_authority` gate (I2, I12) |
| A missing signer / owner check | §8; `initialize` - the upgrade authority only (§9.1) |
| Seizing an authority | §8 (all three roles are changed only by the current Owner; mint/freeze live outside the program) |
| Address reuse / a repeated init | I8, §6.2; `init_if_needed` is forbidden on state accounts, with ATAs as the exception (§9.8) |
| Closing with an open obligation | I9, §9.7 |
| An arbitrary CPI / substituting the token program | Storage transfers and closes go through the typed `Program<Token>` (Anchor checks the program id); ATA creation goes through `Program<AssociatedToken>` - a foreign program cannot be substituted |
| A seed collision between two PDAs | §6.1 |
| The freeze-authority holder freezing a storage or the recipient's ATA | Outside the Vesting program (freeze is a property of the $1WIN mint, TRD §4); the residual centralization is in §12.2 |
| A takeover through a zeroed discriminator | Anchor 1.x forbids all-zero discriminators; state accounts are never initialized by hand |
| A Token-2022 mint in the `token_mint` slot at `initialize` | The slot is typed `anchor_spl::token::Mint`: the type checks the account's owning program against classic SPL Token, so a Token-2022 mint is rejected during context resolution, before the `decimals == 8` check (§9.1). Classic SPL is a system requirement (T-3, §5 "The platform"); a test pins the fact of the rejection |
| Blocking `create_vesting` by pre-funding lamports to a future storage's address | A storage's address is derived from `activity_id` and is predictable before creation (§6.2), so anyone can transfer lamports to it in advance. The attack does not work: with a non-empty account balance, Anchor's `init` takes the "transfer the shortfall" branch (a `transfer` up to the required rent + `allocate` + `assign`) instead of `create_account`, and `create_vesting` goes through normally. There is a regression test for it |
| Substituting a sysvar (a forged time) | Time is read only through a `Clock::get()?` call - a syscall rather than an account from the context, so there is no slot to substitute in any instruction (§4.1). The requirement: time is taken neither from an instruction argument nor from a context account. The program's only sysvar slot is `rent` in `create_vesting`, typed `Sysvar<'info, Rent>` (Anchor checks the address against the known sysvar) |
| One account supplied under two mutable roles of one instruction | Anchor rejects a duplicate among the mutable serializable slots itself - `ConstraintDuplicateMutableAccount` (2040), before any user condition; the negative test "`beneficiary_token` == the activity's storage" rests on that (I12). The explicit inequalities in the body and in the constraints (`beneficiary_token.key() != vault.key()` §9.7, `recipient != vault_authority` §9.5/§9.6) remain insurance against a change of the slot's type, not the primary defence |
| `remaining_accounts` as a channel of unaccounted input | **No Vesting instruction reads `remaining_accounts`** - this is a requirement on the program, not an observation: every account is declared as a context slot and bound structurally. Vesting has none of the batch instructions for whose sake a `remaining_accounts` walk is introduced; introducing such a walk would require an explicit check on every element (the accounts-untrusted rule) and a separate review of the map of defences |

### 12.2 The accepted residual risks

The third column carries a decision marker: **"accepted"** - the risk is left as is and the construction does not change; **"sanctioned"** - the behaviour described is a deliberate right of the role, not a defect.

| Risk | What it is | Acceptance / mitigation |
|---|---|---|
| An under-mint of the emission before the mint-authority revoke | The exact mint volume is not verified on-chain (VE-6). If a storage is underfunded by the time the mint authority is revoked (§2, T-6), topping up is impossible. A claim is all-or-nothing (§9.8) and there is no sweep: as soon as the accumulated `claimable` exceeds the storage's balance, **every** subsequent claim reverts forever (`claimable` only grows), locking up both the unlocked volume not yet paid out **and** the storage's remainder; the activity becomes permanently **unclosable** (`claimed < reference_amount` → `NotFullyClaimed`) and its account and rent remain indefinitely. | An **availability** risk, not a theft risk: the recipient gets less than due but never gets what belongs to someone else (I3). The irreversible combination "the mint authority revoked while a storage is underfunded" is closed structurally: the transaction of §2 contains an `assert_funded` per activity between `mint_to` and `SetAuthority(None)` (§9.9), and an under-mint brings it down entirely. What is left of the risk is operational: a transaction assembled without `assert_funded` (an activity created after the mint-authority revoke is a separate row below). Getting the volumes exactly right is the backend's responsibility (VE-6); the activities are created in advance so that their storages exist by mint time (§9.5). "Topping up" after the mint-authority revoke is no longer a **top-up mint** (the right is gone) but a direct transfer of existing $1WIN to the storage's address (the gate reads `vault.amount`, so the transfer resumes the claims), and only if the project has free volume. |
| An activity created after the mint-authority revoke cannot be filled by minting | `create_vesting` is open right up to TGE (`now < config.tge_ts`, I6), while the right to issue is revoked earlier - by the closing mint transaction (§2), which by procedure runs before TGE. In the window between the revoke and TGE an activity is created normally, but its storage can no longer be filled by minting: topping up is impossible. The mint's state is available to the program - the `token_mint` slot is in the `create_vesting` context (§9.5) - but there is no gate "reject if `mint_authority` is empty". | **Accepted: the gate is deliberately not introduced.** It would close the emergency path needed precisely in that window: cancelling an erroneous activity (`cancel_vesting`, before TGE) and recreating it, filling it by a direct transfer of existing $1WIN to the storage's address (the row above) - the only fix for a mistake discovered after the right to issue was revoked. The mitigation is operational: a pre-flight item - after the mint-authority revoke, an activity is created only together with a plan to fill it by direct transfer and a check of the project's free volume. |
| An over-mint / a misdirected mint | More than `reference_amount` was minted, or activity A's emission was minted into activity B's storage by mistake. An excess in a storage is not tracked on-chain (VE-6). | An over-mint is harmless for the bookkeeping: claiming beyond `reference_amount` is impossible (the I1/I11 ceiling), and the excess goes to `beneficiary_token` at the close (VE-17/§9.7). A mint directed into the wrong storage is invisible on-chain (the mint is the same one) - getting the distribution across storages right is entirely the backend's responsibility (VE-6); for B the consequence is an under-mint (the row above). |
| The close depends on the recipient's final claim | While `claimed < reference_amount` the activity and the storage cannot be closed (there is no sweep, VE-14). A recipient who never claims to the end keeps the account + the storage (and their rent) open indefinitely. | A deliberate property, not a defect (VE-14/VE-17): a recipient's funds are on principle never taken back. An activity's rent is small and is paid by the project; nobody else's funds are locked. |
| Withdrawing the emission before TGE: cancelling a funded activity | Before TGE, Owner may call `cancel_vesting` on an **already funded** activity: the mechanism of §9.7 transfers the storage's entire remainder to `beneficiary_token` (whose SPL owner is `config.withdrawal_authority`) and closes both accounts, i.e. the bucket's emission returns under the project's control. The recipient has no on-chain objection; the only observability is the `VestingCancelled` event. | **Sanctioned:** this is the same mechanism by which an erroneously created activity is fixed before TGE (VE-10, §9.7) - "fixing a mistake" and "withdrawing the emission" are indistinguishable on chain, and no separate right of revocation is introduced. The trust boundary is stated explicitly: **before TGE** the set of activities and their amounts belong to Owner, while the recipients, the TGE date and the withdrawal account belong to Admin (I6); **after TGE** both rights disappear structurally (`ScheduleStarted`), and there is no revocation of what has been unlocked (VE-14). Owner is a multisig with an M-of-N quorum; the window's length is covered by the "An indefinite TGE deferral" row below. |
| The operator paying an ATA's rent again | The rent of the recipient's ATA is paid by the claim's signer (`payer = signer`, `init_if_needed`, §9.8) and is never refunded. If the recipient closes their ATA between claims (the rent from the close goes to them), the operator's next claim recreates the account and pays the rent again; the program does not record how many times it has already been paid. | **Accepted:** the expense is bounded above by the number of **successful** claims on an activity, which is bounded by the number of unlock periods (`≤ MAX_SCHEDULE_MONTHS + 1`), because a claim with `claimable == 0` reverts with `NothingToClaim` - so no infinite "close the ATA → the operator pays the rent" loop arises. No funds are touched: the rent is the operator's SOL, not the storage's $1WIN (I2, I4). The mitigation is operational: the automation triggers a claim only when `claimable > 0`; and the recipient may claim themselves, paying their own rent. |
| Compromising `operator` | The operator can trigger claims (including prematurely or en masse); every claim pays a fee and an ATA's rent. The extreme form is burning the automation's SOL budget: repeated `claim` calls spend fees and, on activities where the recipient's ATA has been closed, rent as well (the row above). | **Accepted.** Theft is structurally excluded: the funds always go to the recipient (I4). The consequence is only operational cost and the creation of ATAs for recipients. Burning the budget is a **DoS of the automation, not of the program**: the unlock proceeds with time, and a recipient claims themselves, signing with their own key and paying for themselves (§8), so an empty operator SOL balance deprives nobody of funds. Owner rotates `operator` (`set_operator`). |
| What Admin does alone | Admin commands three configuration instructions with no second signature: `set_tge` while TGE has not arrived (each call moves the unlock start by up to `MAX_TGE_DELAY` from the current moment, §9.2), `set_recipient` at any point in an activity's life (changing a bucket's payee, §9.6) and `set_withdrawal_authority` at any time (changing the address that receives the remainder and the rent of activities being closed, §9.10). The first two change what a recipient considers theirs: the date from which the unlock runs and the address it goes to; the third changes the address to which the project's funds return. Admin moves no funds, creates and closes no accounts and hands out no roles - creating, cancelling and closing an activity stay with Owner (§8). | **Accepted:** Admin is Ops MS, a project multisig with an M-of-N quorum, just like Owner. `set_tge` closes structurally when TGE arrives (`ScheduleStarted`, I5); `set_recipient` and `set_withdrawal_authority` have no time limit - separate rows below. Every call is observable through an event (`TgeUpdated`, `RecipientUpdated`, `WithdrawalAuthorityUpdated`). Rotation is one `set_admin` call from Owner, with no time limit of its own (§9.3), and the previous key loses all three rights with that same call. Deferring TGE as a distinct risk is the "An indefinite TGE deferral" row below. |
| Admin redirecting the undrawn remainder indefinitely | `set_recipient` has no time condition (§9.6), so the Admin role holder may, at any point in any activity's life - including after TGE and after partial payouts - reassign the payee to an arbitrary system-owned wallet, and the entire undrawn remainder goes there. What was already paid out stays with the previous wallet and the change does not alter the obligation's amount (I1, I6), but the unpaid part is redirected in full and without a second role. The recipient has no on-chain objection: the instruction requires neither their signature nor their confirmation, and they cannot undo the change. | **Accepted:** the right to change the recipient runs for the activity's whole life - otherwise a wrong address discovered after TGE could not be fixed and the bucket would be locked forever. The safeguard is the Ops MS quorum (M-of-N) in the Admin role: a single signature cannot redirect the remainder. The observability is the `RecipientUpdated{activity_id, new_recipient}` event (§11), the operation's only trace: monitoring that event is mandatory and every change is checked against the registry of known project addresses. The role's holder dominates the recipient deliberately; bounding this on-chain (recipient consent, a timelock) is not planned. |
| Admin changing the withdrawal account alone | `set_withdrawal_authority` reassigns the address that receives the $1WIN remainder and the rent of the accounts being closed when an activity is cancelled or closed (§9.10): with no second signature, no time limit and in one step. Right after the change, `cancel_vesting` before TGE will send a funded activity's remainder to the new address, and `close_vesting` will send a storage's excess and the rent of both accounts there. The rights are split: the cancel and the close themselves are signed by Owner, so withdrawing funds requires both roles - Admin sets the destination address and Owner performs the operation. | **Sanctioned:** the withdrawal destination is a configuration parameter of the program, like the TGE date and an activity's recipient, and belongs to the same role. Admin is Ops MS, a project multisig with an M-of-N quorum: a single signature cannot change the address. Being changeable is mandatory: if the previous target's key were unavailable, the remainder and the rent would go to an address nobody controls, and no instruction withdraws them from there. Being one-step is safe: a wrong address is fixed by calling again before the first activity close. The observability is a `WithdrawalAuthorityUpdated` event per call; activity recipients are unaffected by the change, as their payout goes only to `vesting.recipient` (I4). |
| The one-step Admin change | A typo in a `set_admin` address hands the role to an address with no private key: nobody is left to edit the TGE date, the activity recipients or the withdrawal account. | **Accepted.** The mistake is fixable at any time: Owner reassigns Admin with that same `set_admin`, which has no time gate (§9.3). Funds are unaffected - the payout runs with time and a claim is signed by the recipient or the operator (I4). The Owner handover, where a mistake would be irreversible, takes two steps (§9.4). |
| The one-step Operator change | A typo in a `set_operator` address hands the operator role to an address with no private key: the automation stops triggering claims, and a call from the previous key is rejected with `NotAuthorized`. | **Accepted.** The mistake is fixable at any time: Owner reassigns the operator with that same `set_operator`, which has no time gate (§9.3). Funds are unaffected - the recipient claims themselves, signing with their own key (I4). The Owner handover, where a mistake would be irreversible, takes two steps (§9.4). |
| Losing the Owner MS keys | A typo in an ownership-handover address never takes control: `accept_owner` is signed by the candidate themselves, and there is nothing to take over with at an unreachable address (§9.4). What remains is losing the Owner MS keys themselves. The blast radius: **the claim keeps working** - it is signed by the recipient or the operator, Owner is not in the `claim` context, and `tge_ts` was set back at `initialize`, so the unlock proceeds with time and the funds are available to the recipients. What is lost irretrievably is only the Owner instructions: `create_vesting`, `cancel_vesting`, `close_vesting`, `set_admin`, `set_operator` and the ownership handover - i.e. no new activity can be created, no amount fixed before TGE, no Admin or Operator rotated, and no drawn activity closed (its rent stays unreturned). The TGE date, the recipient and the withdrawal account are edited by Admin, so those three rights survive the loss of the Owner keys; the remainder and the rent of activities being closed still cannot be withdrawn - the cancel and the close are signed by Owner. | **Accepted.** Owner is a multisig with an M-of-N quorum rather than a single key, and the backend only offers for signing the multisig addresses the system knows. Losing Owner does not block the payout of funds - that is what makes the risk acceptable; restoring manageability is possible only by upgrading the program (whose upgrade authority is that same Owner MS, §8), so losing Owner MS and the upgrade authority at the same time is irreversible. |
| A wrong `token_mint` in the config | The mint's address is pinned into the singleton at `initialize` with the single check `decimals == 8` (§9.1), and there is no path to a fix: there is no instruction to change the mint, and the singleton is created once. A similar SPL mint with eight decimals - the mint of an earlier test deploy, or a typo in the address - passes every guard, after which the whole program is bound to it: the activity storages are created under that mint, `claim` hands out its tokens, and `assert_funded` confirms funding in them. | Accepted: the only fix is a redeploy under a new program id (the config and the activities are recreated; the funds in the wrong storages are withdrawn with `cancel_vesting` before TGE). The mitigation is operational: the mint address in the `initialize` transaction is checked against the $1WIN address from the deploy package before the multisig signs - a pre-flight item. |
| TGE being immutable once it arrives | From `now ≥ tge_ts` the start cannot be moved (I5). If the mint is not finished by TGE, TGE can no longer be moved forward to "top up". | Before TGE, `set_tge` moves the date freely; the operational procedure finishes the mint and the balance check **before** TGE arrives (§2). After TGE an under-mint reduces to the "An under-mint of the emission" row above (availability, not theft). |
| An indefinite TGE deferral before it arrives | `MAX_TGE_DELAY` bounds each individual move, not the cumulative deferral: while `now < config.tge_ts`, Admin calls `set_tge` again and each call counts a year from the current `now`. This postpones the recipients' unlock indefinitely and they have no on-chain protection; along with the window, the other pre-TGE rights are extended too - Owner's withdrawal of a funded activity's emission (the "Withdrawing the emission before TGE" row above) and Admin's recipient change (§9.6). | **Accepted:** before TGE the set of activities belongs to Owner, and the date and the recipients to Admin, by design (I6, VE-8..VE-10); both are project multisigs with an M-of-N quorum. The trust boundary is recorded explicitly; the TGE date is announced off-chain and moving it is observable through the `TgeUpdated` event. |
| Collapsing the pre-TGE window early or by mistake | A single `set_tge` from Admin with a date at the current moment irreversibly closes `create_vesting`/`cancel_vesting`/`set_tge`; that is the trigger of two already-accepted risks (an under-mint, a wrong amount). A wrong recipient is not among them: `set_recipient` has no time condition and stays available (§9.6), though what was paid out to a wrong address before the change cannot be recovered. | The protection is the pre-flight procedure and the Ops MS quorum; there is no on-chain limiter. A lower bound on `tge_ts` (`MIN_TGE_NOTICE`) is deliberately not introduced: a notice floor on the scale of minutes gives no operational window in which to catch an under-mint, while one on the scale of days would needlessly restrict a legitimate move of TGE closer in the event of an early listing. The procedural item below: do not move `tge_ts` to the current date until the mint and the balance check on every activity are complete. |
| The finality of recipients' funds after TGE is operational, not structural | The promise "after TGE the set of activities and their amounts are final" (I6) rests on the Owner MS quorum rather than on the construction: that same Owner is the upgrade-authority holder (§8, §9.1) and may upgrade the program, and thus replace any of those guarantees. The payout address is not part of that promise at all: it is edited by a normal instruction (§9.6, the row "Admin redirecting the undrawn remainder indefinitely"). | Accepted: the program stays upgradeable, the upgrade authority is held by Owner MS, and it is not made immutable after TGE. Revoking the upgrade authority is possible only **after** `initialize` (which requires a non-empty `program_data.upgrade_authority_address`, §9.1), but no revoke is planned. |
| The freeze-authority holder freezing $1WIN (an internal Ops MS lever) | The $1WIN freeze authority is held by Ops MS - a project multisig (TRD §4), **not an external issuer**. Freezing an activity's storage (the transfer's source) or the recipient's ATA (its destination) would block a claim from outside the program: the "block the claim" lever that the program itself does not offer (§8) is de facto available to the freeze-authority holder. Beyond the claim, a freeze has two **equally valid** routes to the same terminal state - the activity locked up together with the remainder and the rent of both accounts: (1) **a frozen non-empty storage makes the activity uncancellable and unclosable** - `cancel_vesting` and `close_vesting` begin by transferring the remainder (`drain_and_close`, §9.7), and a transfer out of a frozen account reverts, so both instructions fail before the accounts are closed; (2) **a frozen recipient ATA after TGE** - a transfer into it reverts, so `claimed` never grows, `close_vesting` returns `NotFullyClaimed` forever (I9), and `cancel_vesting` is by then already closed by the TGE gate (I6). **`assert_funded` does not catch this:** the check reads only the volume of coverage (`vault.amount + claimed ≥ reference_amount`, §9.9) and does not check that the storage is operable - a frozen storage passes it successfully, so the mint transaction (§2) does not detect a freeze. | Not reducible within the Vesting program: freeze is a property of the mint. Accepted as **residual centralization**: the project's operational procedure is not to apply freeze to vesting storages and recipient ATAs outside a sanctioned AML scenario (the prohibition is an operational item); freeze sits with a multisig with an M-of-N quorum, not a hot key. "The claim is unblockable" is true as a property of the program, not of the whole system. A frozen-state check in `assert_funded` is **not** introduced: freeze is a sanctioned internal lever of the same project, there is no exploitation path "a third-party actor froze the storage", and so the code does not change. |
| A 30-day month vs the calendar | The unlock runs in 30-day months rather than calendar ones (§10.1): "12 months" is 360 days, a drift relative to the calendar. | Accepted: a 30-day month makes `vested()` deterministic with no calendar logic; that is acceptable for a tokenomics schedule. The validators' second-level drift is immaterial (the tranches are in months). |
| The values of the sanity ceilings on input | TRD §7 sets no values. Adopted: `MAX_TGE_DELAY` = 31,536,000 s, `MAX_SCHEDULE_MONTHS` = 120, `MAX_LABEL_LEN` = 32 bytes (§9). | An architectural decision: these are input ceilings rather than business parameters, and each is pinned by a paired boundary test. The normal buckets of §10.5 are covered with a margin (the longest schedule is 34 months). A schedule longer than 120 months, a TGE more than a year from the moment it is set, or a label longer than 32 bytes would require a code change and a redeploy. |
| There is no ceiling on `reference_amount` | `create_vesting` validates only `reference_amount > 0` (§9.5), so `InvalidReferenceAmount` is reachable only at zero: a typo off by orders of magnitude passes validation. An activity's amount is not editable after creation. | Accepted: there is no on-chain ceiling. Before TGE the mistake is fixed only by cancelling the activity and creating it again (§9.7, VE-10); after TGE it cannot be fixed at all (I6). Matching the amounts to the tokenomics is held by the backend's pre-flight check: the sum of every vesting `reference_amount` plus TokenSale's emission equals the token's full supply (§1). |
| An on-curve payout address with no key behind it | The recipient and the withdrawal account must lie on the Ed25519 curve (§9.5, §9.6, §9.1, §9.10), which cuts off every program-derived address; what the curve does not prove is that somebody holds the key. A mistyped or otherwise unowned on-curve address creates an ATA nobody controls: the claim goes through, the tokens land in an account with no private key and cannot be recovered from there - and the claim needs no participation of the recipient, since the operator triggers it (§9.8). | Accepted: the class is closed operationally - by checking the recipients of every activity against the registry of known project addresses **and** by proving control over each address before TGE; both procedures are operational. Changing the recipient is available after TGE too (§9.6), so an address mistake is **not irreversible for the remainder**: Admin reassigns the payee and the following payouts go to the right wallet. Only the part already paid out is irreversible - no instruction recalls it (VE-14). The preventive measures therefore stay mandatory: they close the very first payout, before which nobody would otherwise see the mistake - and for a bucket that unlocks in full at TGE (§10.5) that first payout is the whole bucket, so a smoke claim as a **detective** check (does the payout reach the declared address, at the price of that first payout) is no substitute for the proof of control before TGE. |

**The pre-flight TGE procedure (operational, not on-chain protection).** Some of the risks above are closed by procedure rather than by construction: checking the addresses before signing `initialize` - the mint and the withdrawal account; the completeness of the `assert_funded` set in the mint transaction; the exact equality of the storage balances to the references; checking the recipients against the registry of known addresses and proving control over their addresses before TGE; the order in which `tge_ts` is moved; the conditions for creating an activity after the mint-authority revoke; and a post-TGE smoke claim as a detective check. Those procedures and the justification of each are maintained operationally; this document does not duplicate them.

---

## Worked numeric examples

The units and the period counting are in §10.1 (1 $1WIN = 10⁸ token units, `10000` bps = 100%, a month = 30 days, `day = 1 + n·30`). The `reference_amount` values below are illustrative (the real ones are set by the tokenomics when an activity is created).

**V1. "100% at TGE" (Marketing).** `reference_amount = 50,000,000 $1WIN`, `tge_bps = 10000`, `monthly_bps = 0`, `cliff = 0`.
```
now < tge_ts:  vested = 0
now ≥ tge_ts:  vested = reference_amount = 50,000,000 $1WIN   (the monthly_bps == 0 branch)
```
A single claim at any moment from TGE hands out the whole amount; `claimed = reference_amount` and the activity becomes closable (`close_vesting`).

**V2. A step-by-step unlock (Ecosystem) on days 1, 31, 541.** `reference_amount = 100,000,000 $1WIN`, `tge_bps = 1000`, `monthly_bps = 500`, `cliff = 0`; `s = 1`, `M = ceil(9000/500) = 18`.
```
tge_amt     = floor(100,000,000 × 1000 / 10000) = 10,000,000 $1WIN   (10%)
monthly_amt = floor(100,000,000 ×  500 / 10000) =  5,000,000 $1WIN   ( 5%)

day 1   (p = 0):  p < s ⟹ months = 0            → vested = tge_amt          =  10,000,000  (10%)
day 31  (p = 1):  months = min(1−1+1,18) = 1     → vested = 10M + 1×5M       =  15,000,000  (15%)
day 541 (p = 18): months = min(18,18) = M        → vested = reference_amount = 100,000,000 (100%)
```

**V3. The cliff: the day 270 vs day 271 boundary (Treasury).** `reference_amount = 40,000,000 $1WIN`, `tge_bps = 0`, `monthly_bps = 400`, `cliff = 9`; `s = max(9,1) = 9`, `M = ceil(10000/400) = 25`.
```
monthly_amt = floor(40,000,000 × 400 / 10000) = 1,600,000 $1WIN   (4%)

day 270 (269 days elapsed, p = floor(269/30) = 8):  p < s = 9 ⟹ months = 0
        → vested = tge_amt = 0            (tge_bps = 0)
day 271 (270 days elapsed, p = floor(270/30) = 9):  p ≥ s ⟹ months = min(9−9+1,25) = 1
        → vested = 0 + 1 × 1,600,000 = 1,600,000 $1WIN   (4%)
```
Throughout the cliff 0 is unlocked; the first monthly tranche lands exactly on day 271 (`p = s`).

**V4. Flooring remainders and the top-up in the last tranche (Community Rewards).** For clarity, `reference_amount = 1000` token units, `tge_bps = 0`, `monthly_bps = 834`, `cliff = 0`; `s = 1`, `M = ceil(10000/834) = 12`.
```
monthly_amt = floor(1000 × 834 / 10000) = floor(83.4) = 83 token units   (0.4 lost per tranche)

11 months (p = 11): vested = 11 × 83 = 913                               (not 1000!)
12 months (p = 12 = s+M−1): months == M ⟹ vested = reference_amount = 1000
```
The last tranche hands out `1000 − 913 = 87` token units rather than the nominal 83: the terminal branch returns exactly the reference and, with it, everything the flooring dropped. Nominally `12 × 8.34% = 100.08%` of the reference - neither `min(·, M)` nor the terminal branch allows going beyond the reference.

**V5. Accumulation over skipped periods - one claim hands out the total (Ecosystem).** The V2 parameters (`reference_amount = 100,000,000`, `tge_amt = 10M`, `monthly_amt = 5M`, `M = 18`). The recipient does not claim until day 151.
```
day 151 (p = floor(150/30) = 5): months = min(5−1+1,18) = 5
        vested    = 10M + 5 × 5M = 35,000,000 $1WIN   (35%)
        claimed   = 0
        claimable = 35M − 0 = 35,000,000              → one claim hands out 35M (TGE + 5 tranches at once, VE-12)
        after: claimed = 35,000,000

day 541 (p = 18 = M): vested = reference_amount = 100,000,000
        claimable = 100M − 35M = 65,000,000           → one claim hands out the remaining 65M
        after: claimed = 100,000,000 = reference_amount  → the activity is closable (close_vesting)
```
Skipping periods loses nothing: what has accumulated is handed out together with the current amount in a single call.
