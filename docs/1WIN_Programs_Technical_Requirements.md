# Technical requirements for the 1WIN programs
## Smart-contract architecture: an oversubscription token sale + Claim ($1WIN, Solana)

## Contents

1. [Context and goals](#1-context-and-goals)
2. [The actors and ownership](#2-the-actors-and-ownership)
3. [The system's components](#3-the-systems-components)
4. [The $1WIN token](#4-the-1win-token)
5. [The Token Sale program](#5-the-token-sale-program)
   - 5.1 [The program's parameters](#51-the-programs-parameters)
   - 5.2 [The phases](#52-the-phases)
   - 5.3 [Purchasing (the Sale phase)](#53-purchasing-the-sale-phase)
   - 5.4 [The Blacklist, compliance and refunds](#54-the-blacklist-compliance-and-refunds)
   - 5.5 [The settlement window (the SaleEnded phase)](#55-the-settlement-window-the-saleended-phase)
   - 5.6 [Fixing the pool and activating Claim](#56-fixing-the-pool-and-activating-claim)
   - 5.7 [Claim - releasing tokens and refunding USDC](#57-claim---releasing-tokens-and-refunding-usdc)
   - 5.8 [Withdrawing the proceeds](#58-withdrawing-the-proceeds)
   - 5.9 [The deadline and the unclaimed funds (the ClaimClosed phase)](#59-the-deadline-and-the-unclaimed-funds-the-claimclosed-phase)
6. [Burn - burning tokens](#6-burn---burning-tokens)
7. [The Vesting program](#7-the-vesting-program)
8. [What is stored on-chain](#8-what-is-stored-on-chain)
9. [Governance and configuration](#9-governance-and-configuration)

---

## 1. Context and goals

The platform runs a token sale of its own $1WIN token on Solana with
**oversubscription** mechanics. Participants deposit USDC into a shared pool on an
external landing page, connecting a Solana wallet and signing the transactions
themselves.

- **The collection.** Over roughly 2 weeks participants deposit USDC into the pool.
  The price is fixed - **$0.03 USDC per 1 $1WIN** - and does not depend on the
  pool's size. Every transaction goes through a compliance review off-chain.
- **The settlement.** Once the Sale phase ends the pool is fixed. If more was
  collected than all 130,000,000 $1WIN cost at $0.03 (> the soft cap of $3.9M) it is
  oversubscribed: the tokens are distributed pro rata to the contributions and the
  excess USDC is returned. If less, everyone buys at $0.03 with no refund - the
  tokens are distributed pro rata to the contributions.
- **The payout (Claim).** After the settlement window (a short operational delay of
  about a day) the Claim phase opens (up to 12 months); the Claim start date and its
  duration are set at initialization together with the Sale schedule and are
  editable until the pool is fixed; the deadline = the start plus the duration.
  Claim is a single instruction: releasing the $1WIN and returning the excess USDC
  to the same wallet the participant contributed from, in one call.

The system additionally supports a deflationary mechanic: a daily burn.

**The architectural principles of this design:**

- The fewest contracts and the least coupling. A participant's whole lifecycle
  (contribution → compliance refund → claim → rent return) consists of local
  operations of one program.
- Only the wallet's public key and the amounts are stored on-chain.
- The token stays compatible with exchanges and DEXes.

Every parameter of the system (the price, the emission, the sale's limits, the burn
limits) is stored on-chain and updated by the authorized roles without redeploying
the contracts; the editing windows differ by parameter (§5.1 for the sale, §6.3 for
the burn).

---

## 2. The actors and ownership

### 2.1 The actors

| Actor | Who | Role |
|---|---|---|
| **Owner MS** | The project multisig | Deployment, upgrades (upgrade authority), the token's mint authority (freeze belongs to Ops MS, §4), minting the emission into the vault, revoking mint/metadata to None, withdrawing the sale's proceeds in USDC (`withdraw_proceeds`), withdrawing the unclaimed funds (`sweep_unclaimed`), role management (`propose_owner` / `cancel_owner_proposal` / `set_admin` / `set_operator`; the Owner handover is completed by the new key through `accept_owner`). |
| **Ops MS** | The administration team's multisig | Every sale parameter, the phase schedule, activating Claim (fixing the pool plus the Claim schedule), the burn parameters, the pause, per-wallet limits (Graylist), the token's Freeze Authority |
| **Operator** | The automation key | Automation: placing an address on the Blacklist and removing it, refunding risky deposits, batch-closing accounts; executing the daily burn (`burn`, §6); triggering a vesting claim (the tokens go to the recipient only, §7) |
| **User** | A participant | Deposits USDC, claims |
| **Recipient** | The project wallet of an emission bucket | Claims the unlocked $1WIN of its activity in the Vesting program (§7); it holds no administrative role |

Administrative operations are initiated through the backend and executed by a
multisig. Purchases, Claim and a vesting claim are signed by the signer itself (the
participant or the recipient).

### 2.2 Ownership per contract

Every contract has its own authorities; ownership of all three programs belongs to
Owner MS.

| Contract | Owner / Upgrade | Other authorities |
|---|---|---|
| The $1WIN mint | - (not a program) | mint_authority = Owner MS (revoked to `None` once the whole amount is issued - the sale plus every vesting bucket, §4) · freeze_authority = Ops MS (Admin) |
| TokenSale | Owner MS | admin = Ops MS · operator = the Operator key |
| Burn | Owner MS | admin = Ops MS (the parameters and limits) · operator = the Operator key - executing the burn (`burn`) and controlling the source account (§6.2) |
| Vesting | Owner MS | admin = Ops MS - the configuration: the TGE date (`set_tge`), an activity's recipient (`set_recipient`) and the withdrawal account (`set_withdrawal_authority`); the Owner handover is two-step (`propose_owner` → `accept_owner`, withdrawn by `cancel_owner_proposal`); operator = the Operator key - only the `claim` trigger (the tokens go to the recipient and it disposes of no funds); the recipient signs its own claim (§7) |

Each contract's concrete operations live in its own section (§4 the mint, §5
TokenSale, §6 Burn, §7 Vesting).

---

## 3. The system's components

The system consists of **one main program and two auxiliary ones**, plus the SPL
token itself.

| Contract | Purpose |
|---|---|
| **The $1WIN SPL mint** | A standard SPL token. Not a separate program - a mint account with the standard authorities |
| **The TokenSale program** | The core. USDC intake and contribution accounting, the compliance registries (Blacklist + Graylist), the USDC and $1WIN vaults, computing the allocation and paying out Claim. The phase machine and the details are in §5 |
| **The Burn program** | A daily burn of part of the tokens spent, subject to daily and weekly limits. Its own period accounting |
| **The Vesting program** | The gradual unlock of the $1WIN allocated to the emission buckets outside the sale. Independent activities, each with its own recipient, emission amount, schedule and isolated $1WIN vault; the unlock start is shared (a single TGE). The details are in §7 |

## 4. The $1WIN token

- **The standard:** classic SPL Token (not Token-2022). It provides maximum
  compatibility with centralized exchanges and DEXes - the token trades freely and
  no restricting extensions are used.
- **Precision:** 8 decimals (decimals = 8).
- **The total supply:** **1,000,000,000 $1WIN** - **130,000,000** through TokenSale
  (the sale's emission) and the remaining **870,000,000** through the vesting
  buckets (§7). Revoking the Mint Authority fixes the total supply for good.
- **The Mint Authority:** the owner multisig. The distributable amount is minted
  **once, into the TokenSale program's `token_vault`, before Claim is activated**
  (the minting procedure, the vault's coverage check and their order relative to the
  activation are in §5.6). The right to mint is not handed to the program - the
  program merely distributes out of the vault, and every computation runs off the
  stored parameters rather than off the vault's balance. The Mint Authority is
  revoked to `None` by the standard `SetAuthority` instruction only after the whole
  distributable amount has been issued - the sale's emission and every vesting
  bucket's (§7); after the revocation the total supply is fixed for good. In the
  regular flow the whole amount (1 billion) is minted and the Mint Authority revoked
  **in a single transaction before the vesting starts**: a sequence of `mint_to`
  into every vault (TokenSale's `token_vault` and the vaults of every vesting
  activity), then TokenSale's `assert_funded` (§5.6) and an `assert_funded` (§7.7)
  for every vesting activity, then the closing `SetAuthority(None)` - atomically. "A
  single transaction" here means one **multisig execution** transaction: the mint
  authority is held by a multisig (§9.2), the execution reaches the chain as
  the vault PDA's signature, and `mint_to`, `assert_funded` and
  `SetAuthority(None)` run inside that transaction as nested calls. The activities'
  vaults must exist by then (the activities are created in advance, §7.3) - after
  the revocation no further issuance is possible. **The `assert_funded` checks - for
  the sale's vault and for every activity - must be in the same transaction as
  `SetAuthority(None)`:** that is the only way the gate works, an underfunded vault
  failing the transaction before the irreversible revocation; moved into a separate
  transaction the same check does not provide that property. The TokenSale check
  requires the whole sale emission to be assembled in `token_vault` (§5.6): the
  sale's distributable amount is issued once and in full, and at the time of this
  transaction the pool is not fixed yet, so there is no other reference on-chain.
  The `mint_to` calls themselves may be executed earlier in separate transactions:
  `assert_funded` reads the vault's state at the moment of its own execution, so
  what is mandatory is one bundle - the full set of `assert_funded` together with
  `SetAuthority(None)`. The completeness of the `assert_funded` set is operational
  rather than structural: the program cannot force a transaction to contain a call
  for every activity, so the pre-flight procedure requires including `assert_funded(id)`
  for every `id ∈ [0, activity_count)` minus the cancelled ones and reconciling the
  number of those instructions with the number of live activities **before** the
  multisig signs. Creating an activity (`create_vesting`) is open right up to TGE,
  so an activity created **after** the minting transaction is assembled is not
  covered by this gate - its funding is checked separately. The assembled
  transaction - in its regular form every `mint_to`, every `assert_funded` and
  `SetAuthority(None)` - must fit the 1232-byte limit; that limit is what bounds the
  number of activities in a single transaction. The budget is computed on the
  multisig's execution transaction rather than on the bare legacy form of the same
  instruction set: the multisig's form adds accounts of its own and rearranges the
  nested instructions' accounts, changing both the constant part and the per-activity
  cost. The measurement procedure, the maximum number of activities and the splitting
  rules live in the vesting pre-flight procedure. The token's Metadata Update Authority
  is likewise set to `None` once the metadata is final - the metadata is immutable.
- **The Freeze Authority:** held by **Ops MS (Admin)**.

### The token's metadata

So that $1WIN shows up with a name, a ticker and a logo in wallets, explorers and on
exchanges, a standard **Metaplex Token Metadata** account is created for the mint - a
separate account bound to the mint's address. That is the usual way to give a classic
SPL token a name and a logo; the metadata does not affect the sale's logic - the Sale
program neither reads it nor depends on it.

- **The contents:** the name, the ticker (symbol) 1WIN, and a URI pointing to an
  off-chain file with the logo and the description. Only the link is stored on-chain;
  the logo itself is off-chain.
- **Who creates it:** Owner MS while preparing the token (before the listings).
- **Finalization:** once the contents are verified, Owner MS sets the metadata's
  Update Authority to `None`. After that the name, ticker and logo cannot be changed -
  just as the supply is fixed by revoking the Mint Authority.
- **The order:** creating and finalizing the metadata is not tied to the sale's phases
  and is done independently.

---

## 5. The Token Sale program

Token Sale is the system's core. The program accepts USDC and accounts for the
contributions, keeps the minimal compliance registries (Blacklist + Graylist), holds
the USDC and $1WIN in vaults under PDAs, fixes the pool once the collection is over,
computes the allocation and pays out the tokens together with the excess USDC
(Claim). The program itself is the only source of truth about participation and
allocation.

The subsections follow the lifecycle: the parameters (5.1) → the phases and what is
permitted in each (5.2) → the per-stage requirements: purchasing (5.3), the
Blacklist / compliance / refunds (5.4), the settlement window (5.5), fixing the pool
and activating Claim (5.6), Claim (5.7), withdrawing the proceeds (5.8) and the
deadline (5.9).

### 5.1 The program's parameters

| Parameter | Reference value | Editability |
|---|---|---|
| Token price | $0.03 USDC | Admin (before the Sale phase starts) |
| Emission amount | 130,000,000 $1WIN | Admin (before the Sale phase starts) |
| Soft cap | Computed: `emission × price` | Neither stored nor edited separately |
| Minimum purchase | 10 USDC | Admin (before the Sale phase starts) |
| Maximum purchase | 250,000 USDC | Admin (before the Sale phase starts). A per-purchase limit; the number of purchases from a wallet is unlimited |
| Sale start date (`start_date`) | - | Admin (before the start); `now < start_date ≤ now + 90 days` (`MAX_SALE_START_DELAY`) - protection against a mistakenly distant start |
| Sale end date (`end_date`) | - | Admin (only before the Sale phase starts; afterwards the sale's schedule is immutable); `end_date − start_date ≤ 30 days` (`MAX_SALE_DURATION`) - protection against a mistakenly long intake window |
| Claim start date (`claim_start`) | - | Admin (until the pool is fixed, `!pool_fixed`); `end_date (Sale) < claim_start ≤ end_date + 30 days` (`MAX_CLAIM_START_DELAY`) - protection against a mistakenly distant schedule; at the moment the pool is fixed `claim_start` must still lie ahead (`now < claim_start`, §5.6) |
| Claim duration | 12 months | Admin (until the pool is fixed, `!pool_fixed`; at most 12 months, `MAX_CLAIM_DURATION`) |

**The soft cap is a derived quantity.** Two parameters are stored - the price and
the emission; the oversubscription threshold is always computed as
`emission × price`. That rules out three independent fields drifting apart.

**The sale window.** `start_date` and `end_date` are stored as independent guards
with no derived recomputation. The sale's duration (~2 weeks) is the derived
`end_date − start_date`, computed off-chain for display on the landing page. Both
dates are bounded from above: the start no further than 90 days from the edit
(`MAX_SALE_START_DELAY`) and the intake window no longer than 30 days
(`MAX_SALE_DURATION`). The ceilings are checked identically in `initialize` and
`set_sale_schedule`; they are deliberately wider than the planned schedule and serve
as a safeguard against input error rather than as a business parameter.

**The Claim schedule.** `claim_start` and the Claim duration are set at `initialize`
together with the Sale schedule and are editable until the pool is fixed
(`!pool_fixed`). The Claim deadline (`claim_start + the duration`) is computed on
demand and not stored as a field of its own. The bound
`end_date < claim_start ≤ end_date + 30 days` is checked on every edit of
`claim_start`, the duration or `end_date`: a shift of `end_date` that violates it
reverts. Beyond that bound, fixing the pool requires `claim_start` to still lie
ahead at the moment of the fixation itself (`now < claim_start`, §5.6).

**The price and the emission are immutable from the start of the Sale phase.** They
are editable only before the Sale phase starts; from its beginning both parameters
are frozen for good. Since purchases are possible only during the Sale phase, no
contribution can be accepted on terms different from the final ones - a retroactive
change of the contribution terms is structurally impossible, and every wallet's
computation in Claim runs off the same parameters.

**Re-running the phases is impossible in a production build** - the transitions are
one-way and the schedule is immutable once the Sale phase starts. For e2e tests in
shared infrastructure a run is repeated by re-initializing the state or by a
separate deployment into a test environment (the no-test-code-in-production rule: no
test hooks in the production program).

### 5.2 The phases

The program's lifecycle is five phases: **PreSale → Sale → SaleEnded → Claim →
ClaimClosed**.

- **The phase is not stored as a field of its own** - it is derived on demand from
  the time and the configuration on every instruction. The source of time is
  `Clock::unix_timestamp` (the sysvar).
- The phases do not overlap in time and the transitions are one-way.
- **The pause is a flag, not a phase.** It blocks purchases only and does not move
  the schedule. There is no separate "end the sale manually" instruction - the pause
  serves as the mechanism for suspending the intake.
- **`pool_fixed` is set only in `activate_claim`** (§5.6), which requires the
  SaleEnded phase (`now ≥ end_date`). A purchase is possible only in the Sale phase
  (`start_date ≤ now < end_date`).

```
PreSale    : now < start_date                                   - intake closed, parameters editable
Sale       : start_date ≤ now < end_date                        - USDC intake open (unless paused)
SaleEnded  : now ≥ end_date, Claim not activated / not started  - the settlement window (§5.5); purchase reverts
Claim      : pool_fixed && claim_start ≤ now < deadline         - tokens are released and USDC refunded
ClaimClosed: pool_fixed && now ≥ deadline                       - the unclaimed funds are withdrawn to the project

end_date    = the Sale phase's end date (a stored parameter, editable only before the start)
claim_start = set at initialize, editable until the pool is fixed (§5.6); end_date < claim_start ≤ end_date + 30 days
deadline    = claim_start + the Claim duration - computed on demand, not stored
```

**What is permitted in which phase** (operations and parameter setting). "✓" means
available, "—" means unavailable (it reverts).

| Operation / parameter | PreSale | Sale | SaleEnded | Claim | ClaimClosed |
|---|:--:|:--:|:--:|:--:|:--:|
| `purchase` | — | ✓ | — | — | — |
| `refund` | — | ✓ | ✓† | — | — |
| `activate_claim` | — | — | ✓† | — | — |
| `assert_funded` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `claim` | — | — | — | ✓ | — |
| `withdraw_proceeds` | — | — | — | ✓ | ✓ |
| `sweep_unclaimed` | — | — | — | — | ✓ |
| Batch-closing deposits and aggregates | — | — | — | — | ✓ |
| Blacklist add/remove | ✓ | ✓ | ✓ | ✓ | ✓ |
| Graylist set/remove | ✓ | ✓ | ✓ | ✓ | ✓ |
| **Setting parameters (Admin):** | | | | | |
| Price, emission, min/max transaction limits | ✓ | — | — | — | — |
| The Sale start and end dates | ✓ | — | — | — | — |
| The Claim schedule (`claim_start`, the duration) | ✓ | ✓ | ✓† | — | — |
| The pause (`pause`) | — | ✓ | — | — | — |
| Resuming (`unpause`) | ✓ | ✓ | ✓ | ✓ | ✓ |

† In the SaleEnded phase the operations split into "before" and "after"
`activate_claim`. Once `activate_claim` has set `pool_fixed` the phase stays
SaleEnded (while `now < claim_start`), but `refund`, changing the Claim schedule
(`set_claim_schedule`) and a repeated `activate_claim` all revert on `pool_fixed`.
That is, the phase table is a necessary but not a sufficient condition: those
operations carry an additional `!pool_fixed` invariant.

### 5.3 Purchasing (the Sale phase)

A user deposits USDC with a signed transaction. The transaction goes through when:

- The phase is **Sale** (the time is inside the Sale phase's window),
- The pause is off,
- The buyer's wallet is an address with a private key of its own (see below),
- The amount is within the per-transaction limits: **if the wallet has a Graylist
  record, its `min`/`max`; otherwise the global `min`/`max`**,
- With a Graylist present, the total contribution is within `max_total`:
  `aggregate.net + amount ≤ max_total`,
- The wallet is not on the **Blacklist** (§5.4).

**The buyer's wallet type (TS-12a).** Only an address with a private key of its own -
that is, an address lying on the Ed25519 curve - may be a buyer. Program wallets (a
multisig, a custody service, a smart wallet) are not supported as buyers: their
address is derived programmatically, has no private key and does not lie on the
curve. A signature check alone is not enough for this - a program wallet signs the
transaction through a call from its own program and passes the signature check just
like an ordinary one. Checking the account's owner is not enough either: an
uncreated program address has the System Program as its owner, exactly like an
ordinary wallet. So the program checks the address's membership of the curve, and it
does so **at the entry of the deposit path** - in both instructions that create a
wallet's state: creating the aggregate and the purchase itself. The rejection is the
`WalletNotOnCurve` error.

No separate check is needed in the refund (§5.4) or in Claim (§5.7): membership of
the curve is a property of the address's own bytes and does not change over time, so
a program wallet can obtain neither a deposit nor an aggregate - and without those,
neither a refund nor a Claim ever executes.

**The wallet type is fixed once, at the entry, and afterwards the wallet is used as
an address everywhere.** The program checks the wallet account's owner in no
instruction, and that is a deliberate requirement rather than an omission. The owner
is a mutable field: the system `assign` instruction rewrites it on the account's own
signature, and a participant may at any moment hand its wallet over to an arbitrary
program. The reverse move by the wallet's signature is then impossible - only the
program the account was handed to may return it under the System Program, and only
while the account's data is empty; if the participant handed the wallet to an address
with no program behind it, the move is irreversible forever (the System Program does
not check that the recipient exists).

A "the owner is the System Program" requirement in the refund or in the lists would
therefore protect nothing: a participant who needs to keep buying holds ownership
with a program under its own control and returns it for the duration of the purchase
in a single instruction within the same transaction - no state the operator could
react to ever appears on-chain. What such a requirement would do is make the
participant unreachable for the operator: blocking an address, giving it individual
limits and refunding its deposit must always work, regardless of who owns the wallet
account at that moment - and in the irreversible case the deposit would become
permanently non-refundable, by the participant's own decision. The curve check has
no such weakness: the address does not change, so a check at the entry holds for the
whole lifecycle of participation.

One wallet is unlimited in the number of purchases and the contributions add up.
Every contribution is recorded as a **separate deposit** (so that an individual
purchase can be refunded, §5.4), and the wallet's total contribution in a per-wallet
aggregate.

**Displaying a share on the landing page.** A wallet's total contribution is one read
of its aggregate from the contract (the contract being the only source of truth).
The "total pool size / number of holders" figures and the "top participants" block
are built by a backend indexer from the contract's events (cross-wallet aggregations
are not computed on-chain).

### 5.4 The Blacklist, compliance and refunds

#### The principle

Risk assessment is performed off-chain.
Only the **outcomes** are stored and applied on-chain: a Blacklist record (its
presence means blocked) and a wallet's individual limits (Graylist). No reasons,
notes or risk scores live on-chain. At the entry of the deposit path - both when a
wallet's aggregate is created and on every purchase - the contract checks the wallet
by pubkey against the Blacklist.

Two minimal lists are supported on-chain:

- **The Blacklist** (`Blacklist{wallet}`) - the record's presence means the wallet is
  blocked. It stores only the wallet's public key, with no reasons, notes or risk
  assessment.
- **The Graylist** (`Graylist{wallet, min_per_tx, max_per_tx, max_total}`) - a
  wallet's individual contribution limits.

A `Blacklist{wallet}` record (its presence meaning blocked) **closes the deposit path
only**: new investments from the wallet revert - both creating the wallet's aggregate
and the purchase itself. By itself the block affects neither the contributions
already made nor Claim.

Blocking and refunding are **two independent operations**:

- **The Blacklist** is a ban on further purchases. The scenario "the wallet started
  sending risky crypto → ban it from all purchases" equals adding it to the
  Blacklist.
- **A refund** returns the funds of a specific purchase. It is an off-chain compliance
  decision: if a blocked wallet's funds must be returned, its transactions are
  returned by per-purchase refunds (available only until the pool is fixed). A full
  refund zeroes `net` - there is nothing left to claim.

If Claim has already been executed the computation is final (the payout is
irreversible); an individual token account can be frozen if needed through the token
mint's Freeze Authority.

#### The Graylist: a wallet's individual limits (TS-15a)

A `Graylist{wallet, min_per_tx, max_per_tx, max_total}` record (its presence meaning
the wallet has individual limits instead of the global ones). In `purchase`:

- if a Graylist record **exists** for the wallet, its `min_per_tx`/`max_per_tx`
  **fully replace** the global `SaleState.min_per_tx`/`max_per_tx` limits, and the
  total contribution is checked as well: `aggregate.net + amount ≤ max_total`;
- if there is **no** record, the global limits apply (§5.1).

The record is kept by **Admin** (the Ops MS multisig), at any time. Its structure
mirrors the Blacklist's (a PDA per wallet), but it is kept by Admin rather than by
the Operator; the Graylist and the Blacklist are independent, and while a block is
active a purchase is rejected regardless of the Graylist.

#### How an off-chain decision reaches the chain

The assessment is made off-chain and only its outcome is applied: the backend calls the
corresponding program instructions from the operator role. A wallet that must be
stopped goes on the Blacklist → `purchase` reverts; the decision to refund it or to
take the address off the Blacklist is likewise made off-chain.

#### A refund of an individual purchase

- **For a refund** the backend calls `refund(index)` with the deposit's number; the
  contributing wallet is passed as an account (a deposit is bound to a wallet and an
  index through its seeds) and `index` is the number of the purchase being refunded.
  The contract takes the amount from the deposit itself, returns the USDC to the
  wallet, decreases the wallet's total contribution and the pool's size, and **closes
  the deposit** (closing = returning the rent to the user owner and protecting
  against a repeated refund: a repeated call will not find the account and returns an
  error).
- If the recipient wallet's USDC account (ATA) does not exist (the user closed it),
  the instruction creates it in the same transaction (`init_if_needed` with full mint
  + authority constraints; paid for by the project / the operator).
- **A refund is available only until the pool is fixed:** `refund` reverts when
  `pool_fixed` is set. After the fixation the pool and the allocations are immutable.
- The "transaction → deposit number" mapping is kept by the backend; the on-chain key
  is the deposit's number.
- **When adding to the Blacklist and removing from it** the backend calls the program
  passing the wallet's pubkey as an instruction argument; the wallet is not passed to
  the lists as an account. The Graylist records work the same way. Those instructions
  need the wallet only as a record key, and blocking an address or giving it limits
  must work in any case - including when no account exists at that address yet or
  when it belongs to a third-party program (§5.3).

### 5.5 The settlement window (the SaleEnded phase)

Between the end of the Sale phase (SaleEnded) and fixing the pool in preparation for
the Claim phase there is a delay - an operational window of about a day. The window
is needed to finalize compliance:

- A manual review of the last deposits, so that every alert is worked through and
  every potentially risky deposit is subtracted from the pool's size **before it is
  fixed** (by a refund per §5.4).
- After the fixation the pool is immutable - the allocations are computed off the net
  amounts.

That is why no separate isolated vault for high-risk funds is needed: everything is
settled before the pool is fixed.

"About a day" is an operational norm for closing compliance rather than a constraint
of the program. The program's ceiling on `claim_start` (`the sale's end_date + 30
days`, §5.1/§5.6) is a separate safeguard against a mistakenly distant schedule and
is deliberately wider than this window.

### 5.6 Fixing the pool and activating Claim

Fixing the pool is the boundary between the collection and the payout. It is **not a
phase** but a single `activate_claim` instruction that irreversibly moves the program
from SaleEnded into Claim. After the activation the landing page shows a countdown to
the start of Claim.

1. **Minting the emission into the vault - before the activation.** Owner MS
   assembles a transaction minting the distributable amount of $1WIN into
   `token_vault`. The mint must happen before Claim is activated: with no emission in
   the vault the activation reverts. The distributable amount: under
   oversubscription the whole emission (130M); when undersubscribed
   `floor(pool / price)` of the final pool. The mint and the activation are
   recommended to go in one transaction; the exact execution order is an operational
   matter.
   - **The on-chain vault coverage check (`assert_funded`).** The program provides a
     separate check instruction: `token_vault.amount ≥ the sale's emission`,
     otherwise it reverts (`VaultNotFunded`). It has no signer - no role is checked,
     it changes no state, moves no lamports and emits no events; it is gated by
     neither the phase nor time. The reference is the whole sale emission rather than
     the distributable amount: the distributable amount never exceeds the emission,
     and at the moment of issuance the pool is not yet fixed and therefore undefined.
     The check's purpose is to be a link in the issuance transaction before the right
     to mint is revoked (§4): an underfunded vault fails the whole transaction, so
     the irreversible state "the right to mint is revoked with an underfunded vault"
     never arises. It applies before the first payouts: once Claim begins the vault
     is spent by claims and the program keeps no counter of the tokens released, so
     the unreleased part of the obligation cannot be derived on-chain. Coverage at
     the activation is checked by the activation itself (step 2), and sufficiency on
     every payout by the vault's balance.
2. **Activating Claim is a single instruction.** Admin invokes the activation, which
   atomically:
   - (a) fixes the pool's size in a separate immutable field;
   - (b) checks the token coverage (`token_vault.amount ≥ the distributable amount`)
     and the USDC coverage (the deposit vault holds at least the accounted pool);
   - (c) creates the proceeds vault `proceeds_vault` (always) and physically
     distributes the USDC out of the deposit vault. Under oversubscription the refund
     vault `refund_vault` is additionally created (the participants' refunds plus the
     rounding reserve): the soft cap minus the reserve goes into `proceeds_vault` and
     everything else (the refunds plus the reserve) into `refund_vault`. When
     undersubscribed there are no refunds: the whole pool goes into `proceeds_vault`
     and `refund_vault` is **not created**. After the activation every money
     instruction works only with its own vault (Arch §7, §9.9).
   - The instruction **asserts the SaleEnded phase** (`now ≥ end_date`) and is
     **one-shot**: a repeated call with `pool_fixed` set reverts.
   - The instruction also requires **the start of Claim to still lie ahead**
     (`now < claim_start`); otherwise the activation reverts (`ClaimStartInPast`). The
     Claim schedule is set before the sale while the fixation happens after the whole
     sale, so the window is checked at the moment of the fixation itself: fixing the
     pool at `claim_start` or later would give participants a shortened claim window
     or none at all, while opening the withdrawal and the vault closing to Owner. If
     the fixation is late, Admin first moves `claim_start` forward
     (`set_claim_schedule`, available while the pool is not fixed, within
     `end_date + 30 days`) and then activates.
   - Fixing the pool is the only gate on Claim: while the pool is not fixed, payouts
     and refunds are unavailable (the phase availability is in §5.2).
   - Before the activation the claim instructions and `sweep_unclaimed` revert, and
     `withdraw_proceeds` is unavailable until Claim begins: while participants cannot
     actually claim, Owner cannot receive the proceeds.
3. **Revoking the Mint Authority does not happen at this activation.** Activating
   Claim by itself does not revoke the Mint Authority: the $1WIN mint is shared by
   the sale and the vesting, so the revocation happens once, after the whole amount
   is issued (the sale plus every vesting bucket) - in the regular flow in a single
   transaction of the mint plus `SetAuthority(None)` before the vesting starts (§4,
   §7.7). By the time of the revocation the whole distributable sale amount is
   already minted into `token_vault` (step 1), so the revocation does not affect the
   Claim payouts. The issuance transaction must verify that on-chain: TokenSale's
   `assert_funded` (step 1) is included in it alongside the checks for every vesting
   activity.

### 5.7 Claim - releasing tokens and refunding USDC

Claim is the phase of releasing the $1WIN and returning the excess USDC to the same
wallet the participant contributed from. This program is the only source of truth
about participation and allocation.

#### Access conditions

- The Claim phase: Claim is activated (`pool_fixed`) and the time is inside the
  window (`claim_start ≤ now < deadline`);
- The wallet has a counted contribution (`net > 0`);
- A second payout is structurally impossible: on success the `claim` instruction
  always closes the wallet's aggregate - both when `refund == 0` and when
  `refund > 0`. The closed aggregate is the replay protection: a repeated call will
  not find the account and reverts.

#### Computing the allocation

The allocation is computed **at the moment Claim executes** from two numbers: the
wallet's net contribution and the fixed pool size.

```
net      = the wallet's net contribution (after the compliance refunds)
pool     = the fixed pool size
soft_cap = emission × price - the oversubscription threshold (by default 130M × $0.03 = 3,900,000 USDC, §5.1)

Undersubscribed (pool ≤ soft_cap):
    $1WIN       = net / price
    USDC refund = 0

Oversubscribed (pool > soft_cap):
    $1WIN       = (net / pool) × emission
    USDC refund = net − ($1WIN × price)
```

An example (2.56x oversubscription): a pool of $10M and a contribution of $10,000 →
a share of 0.1% → **130,000 $1WIN** and a refund of **$6,100 USDC**.

**Integer arithmetic.** Every computation is checked arithmetic on u64 with the
intermediates in **u128** (narrowed back to u64 after the divisions); the
multiplication always happens before the division. Rounding favours the protocol: the
token count is rounded **down** (floor) and the cost of the released tokens in the
refund formula **up** (ceil), so a refund never exceeds what is due. The rounding
remainder stays in the vaults and is withdrawn at the deadline.

#### Claim as a single instruction

Claim is **a single instruction** `claim(wallet)`, invoked only by the wallet's owner
itself (`signer == aggregate.wallet`). It atomically releases the tokens, returns the
excess USDC (if any) and closes the aggregate.

```
claim(wallet):
  - compute the allocation per "Computing the allocation" - {tokens, refund} from net and the fixed pool (not stored)
  - transfer the $1WIN from token_vault to the wallet's $1WIN ATA (a transfer of 0 tokens is allowed)
  - if the refund > 0:
      refund_vault and the wallet's USDC ATA must be passed (fail-closed: if they are not, revert)
      transfer the USDC refund from refund_vault to the wallet's USDC ATA
  - close the wallet's aggregate unconditionally (the rent goes back to the wallet)
  - the Claimed{wallet, net, tokens, refund} event
```

- The instruction creates the wallet's $1WIN and USDC ATAs itself if the wallet does
  not have them yet (`init_if_needed`, with the claiming wallet paying the creation
  rent); the ATA binding runs through `authority = wallet`. The $1WIN ATA is always
  passed, the USDC ATA only when there is a refund.
- `refund_vault` and the USDC ATA are passed when `refund > 0`: with `refund > 0`
  their absence reverts (fail-closed). When undersubscribed (`refund == 0`) they need
  not be passed. `refund_vault` is bound by the seeds `[b"refund_vault"]` plus
  `mint = usdc_mint` and `authority = vault_authority`.

### 5.8 Withdrawing the proceeds

The proceeds: when undersubscribed, the whole pool; under oversubscription, the soft
cap (by default $3.9M) **minus the rounding reserve**.

- The `withdraw_proceeds` instruction is available to **Owner** only **from the start
  of Claim** (`now ≥ claim_start`; the pool is fixed by then) and onwards, including
  the phase after the deadline.
- The withdrawal is one-shot: `withdraw_proceeds` moves the **whole balance** of
  `proceeds_vault` to Owner's account and closes the vault. The limit on the proceeds
  is `proceeds_vault`'s balance.

### 5.9 The deadline and the unclaimed funds (the ClaimClosed phase)

Once the deadline passes (`now ≥ deadline`) the ClaimClosed phase arrives
automatically - the claim instructions check `now < deadline` and there is no
separate transition instruction. Owner withdraws the unclaimed remainders to the
project with the `sweep_unclaimed` instructions (each of which checks the deadline
itself). The sweep is performed **per vault, by its own idempotent instruction**:
`refund_vault` (the refunds not drawn down plus the remainder of the reserve - only
under oversubscription), `deposit_vault` (stray transfers) and `token_vault` (the
tokens not released). Each instruction moves its vault's remainder to the given
account and closes the vault; the closing rent goes to the address passed into the
instruction (Arch §5.5, §9.14). The deadline exists only after Claim is activated:
while Claim is not activated `sweep_unclaimed` is unavailable - the funds cannot be
withdrawn without giving participants the chance to claim.

The service accounts are closed with their rent returned to their user owners - in
batches, without an on-chain scan (the backend enumerates the accounts and passes
them into the closing instruction):

- **The deposits** become an immutable archive once the pool is fixed (a refund is no
  longer possible) and are closed in a batch after the deadline - regardless of
  whether the wallet claimed.
- **The aggregates** are closed by the claim itself; the aggregates a claim did not
  close (the wallets that never claimed) are closed in a batch after the deadline.

---

## 6. Burn - burning tokens

### 6.1 Purpose

Burning $1WIN on a daily schedule - permanently reducing the total supply.

### 6.2 The execution model

1. The amount to burn for the day is computed off-chain.
2. The backend assembles the burn transaction and signs it with the **operator**
   key → execution. The program checks the signer against the `operator` in its own
   state, checks the limits (§6.3) and burns the tokens; the Burn program needs no
   vault of its own.

The source token account is a dedicated $1WIN SPL account under the project's
control; its SPL authority is that same `operator` key (or `operator` is its
delegate: SPL Token itself checks the right to debit during the CPI). The source
account has no separate signer: the daily operation is executed by a single
signature and does not require assembling a multisig.

The burn amount is a trusted off-chain input; what is controlled on-chain is the role
and the limits. The result of every burn is published as an event and is not stored
on-chain.

### 6.3 The rules

- **The roles are disjoint (BRD G-1).** `burn` is executed **only** by the operator
  (a structural `has_one = operator` check against the program's state); the limits
  are changed **only** by Admin (`set_limits`). The Owner handover is two-step
  (`propose_owner` / `accept_owner`, cancelled with `cancel_owner_proposal`; §9.1);
  Admin and Operator are changed by Owner in one step (`set_admin` / `set_operator`).
  A role transfer emits a `RoleUpdated` event. Rotating `operator` instantly takes
  the right to burn away from the previous key, even if that key still owns the
  source account.
- **The daily and weekly burn limits** are mutable parameters (Admin), set at
  initialization. The limits are the only on-chain ceiling on the hot operator key:
  a burn beyond them is rejected.
- Every burn is published as an event: the period indices, the burnt amount, and the
  amounts accumulated over the day and over the week. None of it is stored on-chain.
  The instruction's only argument is the amount: an argument that does not affect
  state is not accepted by the instruction (the planned amount stays off-chain, in
  the backend).
- **The amount limits.** The burnt amount accumulates over the UTC day
  (`unix_timestamp / 86400`) and over the UTC week (`unix_timestamp / 604800`) and is
  checked against the daily and the weekly limit on every burn; an excess is
  rejected.
- If the burn amount is zero, no burn is performed.

---

## 7. The Vesting program

Vesting is a separate program that gradually unlocks $1WIN for the emission buckets
outside the sale (BRD §5). The program keeps independent **activities**: each has its
own recipient, its own emission amount, its own schedule and its own isolated $1WIN
storage, while the unlock start is shared by all of them (a single TGE).

### 7.1 An activity's parameters

An activity is created before the shared TGE and stores:

| Field | Type | Purpose |
|---|---|---|
| `recipient` | Pubkey | The recipient wallet: a claim is only ever paid out to it. Editable by Admin only, at any point in the activity's life |
| `reference_amount` | u64 | The activity's $1WIN emission amount (token units, 8 decimals) - the base of the unlock calculation |
| `claimed` | u64 | How much has already been paid out; `claimable = vested(now) − claimed` |
| `tge_bps` | u16 | The share unlocked at TGE, in bps (0-10000) |
| `monthly_bps` | u16 | The monthly share, in bps (0-10000) |
| `cliff_months` | u16 | The cliff in months (a month = 30 days) |

An activity does NOT store a start date of its own - the unlock is counted from the
shared `config.tge_ts` (§7.5). The number of monthly tranches `M` and the deadlines
are not stored - they are derived from the fields. The bucket label ("Treasury",
"Liquidity", …) is not stored on-chain - it only goes into the creation event.

### 7.2 The accounts and the PDAs

| Account | Seeds | Contents |
|---|---|---|
| `VestingConfig` (singleton) | `[b"config"]` | `owner`, `pending_owner` (the Owner candidate, `None` until a handover has begun - §7.3), `admin` (configuration: the TGE date, an activity's recipient and the withdrawal account), `operator`, `token_mint` ($1WIN), `withdrawal_authority` (the project's single withdrawal account - a system-owned wallet that receives the $1WIN remainder and the SOL rent when an activity is cancelled or closed, §7.7; reassigned by Admin with `set_withdrawal_authority`), `tge_ts` (the TGE shared by every activity), `activity_count` (a monotonic id counter), `vault_authority_bump` (a cache of the canonical bump of the `vault_authority` PDA - the program signs the transfer and vault-close CPIs with it; the addresses of the accounts themselves are derived by recomputing the canonical bump, not from the cache) |
| `Vesting` (per activity) | `[b"vesting", activity_id.to_le_bytes()]` | the fields of §7.1 |
| An activity's storage | `[b"vault", activity_id.to_le_bytes()]` | a $1WIN token account under the shared `vault_authority` PDA (`[b"vault_authority"]`), bound by seeds, with `mint = token_mint` and `authority = vault_authority` |

`activity_id` is the sequence number taken from `activity_count`; the counter only
grows and ids are never reused (protection against collisions and against a replay
after a close).

### 7.3 The instructions

| Instruction | Who | Condition | Effect |
|---|---|---|---|
| `initialize` | the holder of the program's upgrade authority (Owner MS, §9.2) | once | Creates `VestingConfig` (`owner`, `admin`, `operator`, `token_mint`, `withdrawal_authority`, `tge_ts`, `activity_count = 0`, `vault_authority_bump`); the arguments are `owner`, `admin`, `operator`, `tge_ts` (four, in that order); the mint and the withdrawal target are supplied as accounts (`token_mint`, `withdrawal_authority`); validation; an `Initialized` event |
| `set_tge` | Admin | `now < config.tge_ts` | Changes the shared `tge_ts`; validation; a `TgeUpdated` event |
| `set_admin` | Owner | `admin ≠ Pubkey::default()`, `admin ≠ config.owner`, `admin ≠ config.operator` | Changes `admin` (in one step); an `AdminUpdated` event |
| `set_operator` | Owner | `operator ≠ Pubkey::default()`, `operator ≠ config.owner`, `operator ≠ config.admin` | Changes `operator` (in one step); an `OperatorUpdated` event |
| `set_withdrawal_authority` | Admin | `withdrawal_authority ≠ vault_authority` | Changes the project's single withdrawal account (in one step, at any time); the new target is supplied as an account; a `WithdrawalAuthorityUpdated` event |
| `create_vesting` | Owner | `now < config.tge_ts` | Creates a `Vesting` + its storage (`activity_id`); the arguments are `reference_amount`, `tge_bps`, `monthly_bps`, `cliff_months`, `label`; the recipient is supplied as an account; validation |
| `set_recipient` | Admin | `new_recipient ≠ vault_authority` (there is no time condition - at any point in the activity's life) | Changes the activity's `recipient`; the new recipient is supplied as an account; a `RecipientUpdated` event |
| `cancel_vesting` | Owner | `now < config.tge_ts`, `claimed == 0` | Cancels an activity created in error; the storage remainder → the project's `beneficiary_token`, the rent → the project's `rent_collector`, the accounts are closed |
| `claim` | the recipient or `operator` | `tge_ts > 0`, `now ≥ tge_ts`, `claimable > 0`, `vault.amount ≥ claimable` | Pays `claimable` out to the recipient's ATA, `claimed += claimable` |
| `close_vesting` | Owner | `claimed ≥ reference_amount` | Closes a fully drawn activity; the storage remainder → the project's `beneficiary_token`, the rent → the project's `rent_collector`, the accounts are closed |
| `propose_owner` | Owner | `new_owner ≠ Pubkey::default()`, `new_owner ≠ config.admin`, `new_owner ≠ config.operator` | Records the candidate in `pending_owner`; an `OwnerProposed` event |
| `cancel_owner_proposal` | Owner | `pending_owner` is set | Withdraws the candidate (`pending_owner = None`); an `OwnerProposalCancelled` event |
| `accept_owner` | the candidate from `pending_owner` (signs it themselves) | `pending_owner` is set, the signer equals it, `pending_owner ≠ config.admin`, `pending_owner ≠ config.operator` | Transfers ownership: `owner = pending_owner`, `pending_owner = None`; an `OwnerUpdated` event |
| `assert_funded` | the signer is not checked - the context has no `Signer` slot | none | Checks that an activity's storage is funded (§7.7); changes no state and moves no lamports |

There are fourteen instructions and thirteen events (§7.8): `assert_funded` changes no
state, so it emits no event.

The roles are disjoint: Owner holds the roles and the instructions that move tokens
and rent (creating, cancelling and closing an activity), Admin holds the
configuration (`set_tge`, `set_recipient`, `set_withdrawal_authority`), Operator only
holds the claim trigger. Owner has none of Admin's rights: `set_tge`,
`set_recipient` and `set_withdrawal_authority` are signed only by `config.admin`,
otherwise `NotAdmin`. Withdrawing the remainder and the rent of an activity being
closed requires both roles: Admin sets the destination address, Owner signs the
cancel or the close itself. What is disjoint is the sets of rights; the program does
not check the role keys for distinctness, so an actual separation of duties comes
only from assigning different keys.

### 7.4 Validation

- **`initialize` (the right to initialize):** the signer is the holder of the Vesting
  program's upgrade authority (Owner MS, §9.2). Binding to an on-chain fact requires
  two conditions, both mandatory: the `program_data` supplied belongs to **this**
  program (`vesting_program.programdata_address() == Some(program_data)`) and its
  `upgrade_authority_address == Some(signer)`; violating either yields
  `NotUpgradeAuthority`. Only together do they rule out hijacking the singleton's
  initialization in the window between the deploy and the `initialize` call: without
  the first condition, the upgrade-authority holder of any other upgradeable program
  would supply that program's `program_data` and create the config with roles of
  their own.
- **`initialize` (the mint and the roles):** `token_mint.decimals == 8`, otherwise
  `InvalidMintDecimals`; the decimals are checked only here - in every other
  instruction the mint is pinned by the `config.token_mint` address, and a mint's
  decimals are immutable. `owner`, `admin` and `operator` are not equal to
  `Pubkey::default()`, otherwise `InvalidAuthority`. The program does not restrict the
  roles from coinciding with one another: a single key may hold any two of them or all
  three, as in Sale and Burn; separating the roles is an organizational decision of
  whoever owns the keys.
- **`initialize` / `set_withdrawal_authority` (the withdrawal target):**
  `withdrawal_authority` is supplied as an account of type `SystemAccount`, not as a
  `Pubkey` argument - the type *is* the check, the same construction as the recipient
  in `create_vesting` / `set_recipient`. The program requires the withdrawal target to
  be a system-owned account: the rent of the accounts being closed goes to that address
  (the `rent_collector` slot, §7.7), so the check stands at the entry rather than
  surfacing as a failure on the first activity close; an incompatible address (a token
  account, a PDA with data) is cut off during context resolution, before the
  instruction body. On top of the type, `withdrawal_authority ≠ vault_authority` is
  required (otherwise `InvalidAuthority`) for two reasons: if they were equal, the
  remainder and the rent would end up in the program's own custody, from which no
  instruction withdraws them, **and** the condition
  `beneficiary_token.owner == config.withdrawal_authority` (§7.4, the withdrawal
  accounts) would then admit any other activity's storage - every storage belongs to
  `vault_authority` - i.e. one activity's remainder would be withdrawn into another's
  storage. The gate is therefore also part of the isolation of activity funds. The
  `withdrawal_authority ≠ Pubkey::default()` check (otherwise `InvalidAuthority`) stays
  in the body as insurance against a change of the slot's type: through the public
  interface it is unreachable - the zero address coincides with the System Program's
  address, and the slot's type rejects it earlier. Both checks apply identically on
  the two write paths: `initialize` and `set_withdrawal_authority` (Admin, one step,
  no time condition - every cancel and every close reads the field's current value).
- **`initialize` / `set_tge` (the shared TGE):** `tge_ts > now` (otherwise
  `InvalidTge`) and `tge_ts ≤ now + MAX_TGE_DELAY` - the ceiling cuts off an obviously
  erroneous far-future date (in particular a timestamp in milliseconds, which pushes
  TGE tens of thousands of years out: such a TGE will never arrive, and activities can
  no longer be cancelled once it is set) and bounds the window in which Owner can
  unilaterally cancel a funded activity. The edit window for `set_tge` is until TGE
  arrives (`now < config.tge_ts`, otherwise `ScheduleStarted`).
- **`set_admin` / `set_operator` / `propose_owner`:** the new key ≠
  `Pubkey::default()`, otherwise `InvalidAuthority`; coinciding with another role is
  allowed. Changing Admin and Operator is one-step: the sitting Owner fixes a wrong
  assignment themselves, and neither role moves funds. For the same reason changing
  the withdrawal account (`set_withdrawal_authority`) is one-step as well: a wrong
  address is fixable by calling again before the first activity close.
- **The Owner handover is two-step.** `propose_owner(new_owner)` only names the
  candidate; ownership passes on `accept_owner`, which the candidate signs
  themselves - the signature *is* the proof that the key exists and is available.
  Until then the rights stay with the sitting Owner, and the nomination is replaced by
  another `propose_owner` or withdrawn with `cancel_owner_proposal`. `accept_owner`
  requires `pending_owner` to be set (otherwise `NoPendingOwner`) and the signer to
  equal it (otherwise `NotOwner`); there are no other conditions for taking over - a
  candidate who became the admin or the operator between the two steps still takes
  ownership. `cancel_owner_proposal` with an empty `pending_owner` reverts with
  `NoPendingOwner` rather than finishing silently.
- **`create_vesting` / `set_recipient` (the recipient):** the recipient is supplied as
  an account of type `SystemAccount`, not as a `Pubkey` argument; the `recipient` slot
  is read with the same type in `claim`. The type cuts off existing accounts that do
  not belong to the System Program (a token account, a PDA with data) during context
  resolution, before the instruction body: otherwise the claim would be structurally
  impossible until the recipient was reassigned, and after TGE the activity would also
  be uncancellable, i.e. `set_recipient` would be the only path to a fix. An
  uncreated PDA belongs to the System Program and the type lets it through, so both
  instructions require `recipient ≠ vault_authority`, otherwise `InvalidAuthority`:
  `vault_authority` is gated explicitly because it is the signing PDA that custodies
  the storages - a recipient at that address would mean paying out into an ATA under
  the program's custody, from which no instruction withdraws it. The program's other
  uncreated PDAs - the internal `__event_authority` and the addresses of future
  activities (`["vault", N]`, `["vesting", N]`, created together with activity `N`) -
  and any other erroneous addresses are covered by an operational check of the recipients:
  a registry of known project addresses plus a check that the address is not in the
  program's PDA space. The `recipient ≠ Pubkey::default()` check (otherwise
  `InvalidAuthority`) stays in the body as insurance against a change of the slot's
  type.
- **`create_vesting` (the schedule):** `token_mint` is bound structurally
  (`address = config.token_mint`), there is no separate check in the body;
  `reference_amount > 0` (otherwise `InvalidReferenceAmount`), with no upper ceiling on
  the amount; `tge_bps ≤ 10000`, `monthly_bps ≤ 10000` (otherwise `InvalidBps`);
  `monthly_bps == 0` ⟹ `tge_bps == 10000` and `monthly_bps > 0` ⟹ `tge_bps < 10000`
  (other combinations never unlock the amount in full); `monthly_bps == 0` ⟹
  `cliff_months == 0` (a cliff without monthly tranches is read by no branch of
  `vested()`, and a meaningful input is not ignored silently); when `monthly_bps > 0`
  the value `cliff_months == 1` is rejected (`cliff_months == 0` and
  `cliff_months ≥ 2` are allowed): `vested()` treats a cliff of 0 and of 1
  identically, so a one-month cliff is meaningless and would be silently ignored
  (§7.5); `cliff_months + M ≤ MAX_SCHEDULE_MONTHS`, where `M` is computed only in the
  `monthly_bps > 0` branch - these conditions yield `InvalidSchedule`; the length of
  `label` is at most `MAX_LABEL_LEN` **bytes** (not characters), otherwise
  `InvalidParams`.
- **`cancel_vesting` / `close_vesting` (the withdrawal accounts):** both slots are
  bound to the project's single withdrawal account `config.withdrawal_authority` - to
  its **current** value at the time of the call (set on `initialize`, reassigned by
  `set_withdrawal_authority`). `beneficiary_token.mint == config.token_mint`,
  otherwise `InvalidMint`; `beneficiary_token.owner == config.withdrawal_authority`
  and `beneficiary_token` does not coincide with the activity's storage, otherwise
  `InvalidBeneficiary` - otherwise the remainder would go into another activity's
  storage (breaking fund isolation) or to a third-party account; `rent_collector` is
  typed `SystemAccount` (so it cannot be a token account) with
  `address == config.withdrawal_authority`, otherwise `InvalidRentCollector`. The
  remainder is bound by the `withdrawal_authority` owner rather than by a specific ATA
  - deliberately, so that a frozen or closed ATA of that owner cannot block a cancel or
  a close.
- **`assert_funded`:** `vault.amount + claimed ≥ reference_amount`, otherwise
  `InsufficientVaultBalance` (§7.7).

Three constants are sanity ceilings on input: they bound the instruction arguments at
the entry and take no part in the calculations of §7.5.

| Constant | Value | Bounds |
|---|---|---|
| `MAX_TGE_DELAY` | 365 days (31,536,000 s) | `tge_ts ≤ now + MAX_TGE_DELAY` in `initialize` and `set_tge` |
| `MAX_SCHEDULE_MONTHS` | 120 | `cliff_months + M ≤ MAX_SCHEDULE_MONTHS` in `create_vesting` |
| `MAX_LABEL_LEN` | 32 bytes | the length of `label` in `create_vesting` |

### 7.5 The shared TGE, the unlock math and the timeline

**The shared TGE.** The unlock of every activity starts at one shared moment,
`config.tge_ts`. The date is set to a concrete value on `initialize`, and Admin edits
it with `set_tge` **until TGE arrives** (`now < config.tge_ts`); once
`now ≥ config.tge_ts` the value is immutable (`set_tge` reverts with
`ScheduleStarted`) - the start cannot be moved in either direction after it has
arrived. That same boundary `now < config.tge_ts` is the window for `create_vesting`
and `cancel_vesting`: after TGE the set of activities, their amounts and their
schedules are final, and cancelling is impossible. The payout address is not covered
by that freeze: `set_recipient` has no time condition and works at any point in the
activity's life (§7.3) - a change does not alter the size of the obligation, it only
redirects the part not yet drawn. The value `tge_ts == 0` is a reserved "not set"
marker: `claim` is fail-closed and requires `tge_ts > 0` (otherwise
`ScheduleNotStarted`), so that an uninitialized TGE cannot open an instant unlock. The
gate is written as strict positivity rather than as "not equal to zero" because it
closes the whole class "the start moment is not a real date": with `tge_ts == 0` the
comparison `now ≥ 0` is true and the reference point would become 1970, while with any
negative value the number of elapsed periods is certainly greater than the number of
tranches and the entire amount would unlock at once.

The gate `create_vesting = now < config.tge_ts` is mandatory: `vested()` is computed
from the shared `tge_ts`, so an activity created after TGE would be unlocked (fully or
partly) immediately, and it could no longer be cancelled - the obligation could be
drawn at once, bypassing the schedule. That is why every activity is created and
funded before TGE (funding the storages and revoking the mint authority).

A month is fixed at **30 days** (`MONTH = 2,592,000 s`). The count runs from
`config.tge_ts`: period `n` arrives at `tge_ts + n × MONTH` (n = 0 is the TGE
moment / day 1, n = 1 is day 31, …).

```
MONTH = 30 * 86400

s = max(cliff_months, 1)                       // the period of the first monthly tranche
M = ceil((10000 − tge_bps) / monthly_bps)      // the number of monthly tranches (monthly_bps > 0)

vested(now):
  if now < config.tge_ts:  return 0
  if monthly_bps == 0:     return reference_amount                // a "100% at TGE" activity
  p = (now − config.tge_ts) / MONTH                               // floor, the period number
  months = 0 if p < s else min(p − s + 1, M)
  if months == M:          return reference_amount                // the last tranche tops up to the reference
  tge_amt     = reference_amount * tge_bps / 10000                // floor
  monthly_amt = reference_amount * monthly_bps / 10000            // floor
  return tge_amt + months * monthly_amt

claimable(now) = vested(now) − claimed
```

- The cliff is counted in the same 30-day months: a 9-month cliff ⟹ `s = 9` ⟹ the
  first tranche on day 271 (`tge_ts + 270 days`). With no cliff, `s = 1` ⟹ the first
  tranche on day 31. The values `cliff_months == 0` and `cliff_months == 1` give the
  same schedule (`s = max(cliff_months, 1) = 1`), so when `monthly_bps > 0` the input
  `cliff_months == 1` is rejected with `InvalidSchedule` as ambiguous (§7.4): a
  one-month cliff is meaningless and would be silently ignored. `cliff_months == 0` and
  `cliff_months ≥ 2` are allowed.
- The integer arithmetic is checked, intermediates are u128, downcast to u64 after the
  divisions, multiply before divide. Tranches round **down**; the accumulated dust is
  given out whole in the last tranche (the `months == M` branch returns exactly
  `reference_amount`) - the unlocked amount reaches the reference exactly.
- **The `claimed` ceiling.** A claim only ever changes `claimed` by the step
  `claimed += (vested(now) − claimed)`, i.e. after a claim `claimed = vested(now)`.
  `vested` is monotonically non-decreasing and is bounded above by
  `reference_amount` by construction: `M = ceil((10000 − tge_bps)/monthly_bps)` gives,
  at step `months = M−1`, the exact share `tge_bps + (M−1)·monthly_bps < 10000`, and
  the floored sums do not exceed it (strictly `< reference_amount`); the terminal
  branches (`monthly_bps == 0` and `months == M`) give exactly `reference_amount`.
  Therefore `claimed ≤ reference_amount` always holds, and
  `claimed == reference_amount` is reachable by exactly one claim in the terminal
  period (`p ≥ s + M − 1`). What is not drawn carries over and is paid out
  cumulatively (VE-12); after the full unlock the remainder is available to the
  recipient with no time limit (VE-14).

```
The timeline (the axis runs from TGE; day = 1 + n·30):

Ecosystem (tge 1000, monthly 500, cliff 0):
  day 1      31    61    91   …   541
   10% ─▶  +5% ─▶ +5% ─▶ +5% … +5%(top-up → 100%)
  cumul:   10   15    20   25   …  100 %

Treasury (tge 0, monthly 400, cliff 9 = 270 days):
  day 1  ·········(cliff 270d)········· 271   301   331  …  991
    0%                                  +4% ─▶ +4% ─▶ +4% … +4%(top-up → 100%)
  cumul:   0 ······························   4     8    12  …  100 %
```

The bucket parameters (day = `1 + n·30`):

| Bucket | `tge_bps` | `monthly_bps` | `cliff_months` | `M` | 1st monthly tranche | Fully paid out |
|---|---|---|---|---|---|---|
| Marketing | 10000 | 0 | 0 | 0 | - | the start (day 1) |
| Ecosystem | 1000 | 500 | 0 | 18 | day 31 | day 541 |
| Community Rewards | 0 | 834 | 0 | 12 | day 31 | day 361 |
| Treasury | 0 | 400 | 9 | 25 | day 271 | day 991 |
| Liquidity | 1000 | 750 | 0 | 12 | day 31 | day 361 |

### 7.6 The claim

The claim is a single instruction, `claim(activity_id)`. It can be triggered by **the
activity's recipient or by `operator`** (the automation account from
`VestingConfig`); whoever calls it, the unlocked $1WIN always goes **only to the
activity's `recipient`** - redirection is structurally excluded.

```
claim(activity_id):
  require signer == vesting.recipient || signer == config.operator   // otherwise NotAuthorized
  require config.tge_ts > 0                                           // otherwise ScheduleNotStarted (TGE not set)
  require now ≥ config.tge_ts                                         // otherwise ScheduleNotStarted
  amount = vested(now) − claimed
  require amount > 0                                                  // otherwise NothingToClaim
  require vault.amount ≥ amount                                       // otherwise InsufficientVaultBalance (a full revert)
  transfer amount $1WIN from the activity's storage to the recipient's ATA
  claimed += amount
  event Claimed{ activity_id, recipient, amount, claimed }
```

- **The recipient binding is structural.** The recipient is supplied as an account of
  type `SystemAccount` pinned with `address = vesting.recipient` (the address is taken
  from the stored field, not from the signer); the destination is its ATA, given by the
  constraints `associated_token::authority = recipient` and
  `associated_token::mint = token_mint`. The operator physically cannot substitute an
  account of their own: any other token account (even one with `mint = token_mint` but
  a different authority) fails the canonical ATA address derivation and the instruction
  reverts before the transfer. `recipient` is not a signer and is changed only by Admin
  (`set_recipient`, at any point in the activity's life): the claim reads the field's
  current value, so after a change the payout goes to the new wallet while what was
  already paid out stays with the previous one.
- **Authorization** is the only runtime check:
  `signer ∈ {vesting.recipient, config.operator}` (otherwise `NotAuthorized`). Both
  keys are always set (`recipient` is mandatory on an activity, `operator` is mandatory
  in `VestingConfig`).
- **The ATA rent.** `init_if_needed` creates the recipient's ATA if needed; the signer
  pays (`payer = signer`) - either the recipient for themselves or the operator for the
  recipient. `payer ≠ authority` is the normal case; that rent is never refunded to the
  operator (it is the operator's operational cost).
- The claim is repeatable and has no time limit - there is no deadline; a repeat with
  `claimable == 0` reverts (`NothingToClaim`). If the balance is insufficient the whole
  thing reverts (`InsufficientVaultBalance`), there is no partial payout; on a revert
  the ATA is not created. There is no sweep of what was never claimed - the recipient's
  funds are never taken back (VE-14).

### 7.7 Funding, cancelling, closing

- **Funding happens outside the program.** Owner (the mint authority) issues a
  bucket's emission into its storage; the volumes (exactly the activity's emission
  amount) and the balance bookkeeping are handled off-chain by the backend, and the
  program stores no "this activity is funded" flag. Every storage is funded before
  TGE - by the very emission issue that ends with revoking the mint authority in a
  single transaction (§4): `mint_to` into each storage, then `assert_funded` on each
  vesting activity, then the closing `SetAuthority(None)`. An underfunded storage
  brings that whole transaction down on `assert_funded`, so the mint authority is not
  revoked on an under-mint. If a storage nevertheless ends up underfunded after the
  revoke, topping it up is impossible and claims beyond the balance revert
  (`InsufficientVaultBalance`); getting the volumes exactly right is the backend's
  responsibility.
- **The funding check (`assert_funded`).** The check
  `vault.amount + claimed ≥ reference_amount`: the storage must cover the **undrawn**
  part of the reference. The absolute form `vault.amount ≥ reference_amount` is correct
  only while `claimed == 0`: after the first claim the balance is reduced by what was
  paid out, and a fully funded activity would report itself as underfunded. The
  instruction changes no state and grants no rights: no role is checked, there are no
  `mut` accounts and no event; on a shortfall - `InsufficientVaultBalance`. Outside the
  mint transaction it serves as an on-chain funding check at any moment, including
  before TGE and after some of the claims.

`cancel_vesting` and `close_vesting` share one mechanism for withdrawing the remainder
and take **two accounts, both bound to the single `config.withdrawal_authority`** (the
project's withdrawal account - the field's current value): `beneficiary_token`, a $1WIN
token account for the storage remainder (the checks are in §7.4: the right mint,
otherwise `InvalidMint`; `owner == config.withdrawal_authority` and not equal to the
activity's storage, otherwise `InvalidBeneficiary`), and `rent_collector`, a system
account for the SOL rent with `address == config.withdrawal_authority`, otherwise
`InvalidRentCollector`. The remainder is transferred to `beneficiary_token` **only when
`vault.amount > 0`** (otherwise the step is skipped and the instruction does not fail)
and precedes the close - SPL Token closes a non-native token account only at a zero
balance; then the storage and the `Vesting` account are closed and the rent of both
goes back to `rent_collector`.

- **Cancelling (`cancel_vesting`, Owner, before TGE).** For an activity created in
  error; the storage remainder (if the emission has already been minted) goes to
  `beneficiary_token`. Other pre-TGE corrections (the amount, the schedule) are made by
  cancelling and creating again. `activity_count` is not rolled back (§7.2). The actual
  boundary for fixing mistakes is **the mint-authority revoke, not TGE**: a recreated
  activity can only be filled by minting, and minting into every storage ends with the
  atomic transaction `mint_to` + `assert_funded` + `SetAuthority(None)` before TGE
  (§4). After the right to issue is revoked there is nothing left to fund a new
  activity with, even though the `now < config.tge_ts` gate formally keeps the
  cancel/recreate window open right up to TGE.
- **Closing (`close_vesting`, Owner, `claimed ≥ reference_amount`).** The storage
  remainder at that point is an overflow beyond the emission amount (an accidental
  third-party transfer). The `≥` form (rather than `==`) is fail-closed and robust to a
  refactor of the formula: `claimed` never exceeds `reference_amount` (§7.5), so in
  practice the gate fires on equality. While `claimed < reference_amount` a close is
  impossible - the close depends on the recipient's final claim (there is no sweep,
  VE-14/§7.6); this is a deliberate property, not a defect. `close_vesting` has no time
  gate and needs none: before TGE `vested = 0`, hence `claimed = 0` while
  `reference_amount > 0`, and the "fully drawn before TGE" condition is unsatisfiable
  for any activity. A pre-TGE condition would not add a single rejection but would
  close the normal path - a drawn activity is closed precisely after TGE.
  Symmetrically, `cancel_vesting` with `claimed > 0` is unreachable: it is only open
  before TGE, where there are no claims. The `claimed == 0` gate (otherwise
  `ScheduleStarted`) is nevertheless stated explicitly in the instruction - by the
  fail-closed rule, the unreachability is derived from a gate in a **different**
  instruction (`set_tge`), and the insurance holds the invariant independently of it.

### 7.8 Events and errors

The events (an audit trail for the indexer):
`Initialized{owner, admin, operator, token_mint, withdrawal_authority, tge_ts}`,
`TgeUpdated{tge_ts}`, `AdminUpdated{admin}`, `OperatorUpdated{operator}`,
`OwnerProposed{pending_owner}`, `OwnerProposalCancelled{}`, `OwnerUpdated{new_owner}`,
`WithdrawalAuthorityUpdated{withdrawal_authority}`,
`VestingCreated{activity_id, recipient, reference_amount, tge_bps, monthly_bps, cliff_months, label}`,
`RecipientUpdated{activity_id, new_recipient}`,
`VestingCancelled{activity_id, amount, beneficiary_token, rent_collector}`,
`Claimed{activity_id, recipient, amount, claimed}`,
`VestingClosed{activity_id, amount, beneficiary_token, rent_collector}` (in both
closing events `amount` is the storage remainder that was withdrawn, and
`beneficiary_token` and `rent_collector` are the addresses of the withdrawal
accounts). There are thirteen events against fourteen instructions:
`assert_funded` changes no state, so the indexer has nothing to record. The ownership
handover is observable through three events: `OwnerProposed` (a candidate is named),
`OwnerProposalCancelled` (the candidate is withdrawn), `OwnerUpdated` (the candidate
has taken over). The bucket `label` lives only in the event and is not stored on-chain.

The errors form a single `#[error_code] enum`; the order of the variants is fixed and
defines the codes 6000-6020: `MathOverflow`, `NotOwner`, `NotUpgradeAuthority`
(`initialize` not from the upgrade-authority holder), `NotAuthorized` (a claim from
neither the recipient nor the operator), `InvalidAuthority` (a zero role or recipient
address, a recipient equal to `vault_authority`, or a `withdrawal_authority` that is
zero or equal to `vault_authority` - in `initialize` and in
`set_withdrawal_authority`), `InvalidMint` (a foreign mint on `beneficiary_token`),
`InvalidMintDecimals` (the mint's decimals ≠ 8; checked only in `initialize`),
`InvalidTge` (`tge_ts` not in the future or beyond `MAX_TGE_DELAY`), `InvalidParams`
(the length of `label` beyond `MAX_LABEL_LEN`), `InvalidReferenceAmount`
(`reference_amount == 0`), `InvalidBps` (a share beyond 10000), `InvalidSchedule` (a
combination of shares that never unlocks the amount in full, a cliff with
`monthly_bps == 0`, a schedule beyond `MAX_SCHEDULE_MONTHS`, or a degenerate number of
monthly tranches `M == 0` - the last is reachable only on a forged activity record and
therefore serves as insurance for the calculation inside `claim`),
`InvalidBeneficiary` (a `beneficiary_token` whose owner ≠ `config.withdrawal_authority`
or which coincides with the activity's storage), `ScheduleStarted` (creating an
activity, cancelling it or moving the TGE date after TGE has arrived, or cancelling an
activity with `claimed > 0`; a recipient edit is not covered by this code -
`set_recipient` has no time condition), `ScheduleNotStarted` (a claim before TGE or
with `tge_ts ≤ 0`), `NothingToClaim`, `InsufficientVaultBalance` (a claim beyond the
storage balance, or an uncovered remainder of the obligation in `assert_funded`),
`NotFullyClaimed` (a close with `claimed < reference_amount`), `InvalidRentCollector`
(`rent_collector` ≠ `config.withdrawal_authority`), `NoPendingOwner` (code 6019:
`accept_owner` or `cancel_owner_proposal` with an empty `pending_owner`), `NotAdmin`
(code 6020: `set_tge`, `set_recipient` or `set_withdrawal_authority` not from
`config.admin`).

Three variants serve more than one invariant - the merging is deliberate. The order of
the codes is frozen: splitting one in place would shift the codes of errors that have
already been reviewed, so the list is extended append-only - a new variant gets the
next free code and the codes of the existing ones never change. The merged variants:

- `InsufficientVaultBalance` - the payout gate in `claim` (`vault.amount ≥ amount`) and
  the solvency gate in `assert_funded` (`vault.amount + claimed ≥ reference_amount`).
  The meaning is told apart by the instruction's context: "there is nothing to pay out
  right now" versus "the undrawn remainder of the obligation is not covered";
  operationally - by the index of the failing instruction in the transaction (in the
  mint transaction of §4 the code comes from `assert_funded` on a specific activity,
  outside it - from `claim`).
- `InvalidAuthority` - four invariants: a zero `owner`, `admin` or `operator`;
  `withdrawal_authority == vault_authority` (on both write paths of the withdrawal
  target); a zero recipient; a recipient `== vault_authority`. The violation is told
  apart by the instruction and by the set of accounts it supplied; each check has a
  negative test of its own.
- `InvalidBeneficiary` - two invariants on the single `beneficiary_token` slot in
  `cancel_vesting` / `close_vesting`: the owner is not equal to
  `config.withdrawal_authority`, and coinciding with the activity's storage. The second
  condition is insurance against a change of the slot's binding: supplying the storage
  in both slots is already cut off by the duplicate-mutable-account check before the
  instruction body. The rent withdrawal target is a separate slot and a separate edit
  to the transaction, so it has an `InvalidRentCollector` of its own.

### 7.9 Reading the available amount publicly (`claimable`)

`claimable(now) = vested(now) − claimed` is a derived quantity; **there is no separate
on-chain read instruction**. The indexer computes it outside the program,
deterministically, from the public fields of two accounts: `config.tge_ts` (the shared
start) and the activity's (`reference_amount`, `claimed`, `tge_bps`, `monthly_bps`,
`cliff_months`) - a local calculation by the formula of §7.5.

---

## 8. What is stored on-chain

On-chain there are only wallet public keys and numeric quantities. There are no
personal data, no reasons for a block, no officer's notes and no risk score.

| What | Where | Contents |
|---|---|---|
| The sale's global state | the Sale account | the price, the emission, the pool size (the working counter `deposits_usdc_contributed` before it is fixed and the frozen `pool_fixed_amount` after), the `pool_fixed` and `paused` flags, `claim_start` (the Claim deadline is not stored - it is computed as `claim_start + duration`), the min/max per-transaction limits, the start and the end date of the Sale phase (`start_date`, `end_date`), the Claim duration, the counter of participating wallets, the references to the `deposit_vault` / `token_vault` vaults (`proceeds_vault` and - on oversubscription - `refund_vault` are created on activation and bound by seeds), the authorities (owner, admin, operator) and the Owner candidate (`pending_owner`, empty until a handover has begun - §9.1), the bump of the `vault_authority` PDA (needed as a seed for signing CPIs). The phase and the soft cap are not stored - they are computed (§5.1, §5.2) |
| A wallet's total contribution (the aggregate) | an account per wallet | the net contribution (`net`), the number of deposits |
| A deposit | an account per contribution transaction | the deposit amount (it lives until an AML refund; after the pool is fixed it is an immutable archive, closed in a batch after the deadline, §5.9) |
| The Blacklist | an account per wallet | presence = blocked |
| The Graylist | an account per wallet | the individual limits: `min_per_tx`, `max_per_tx`, `max_total` |
| The sale's vaults | before activation - two token accounts; after - three (undersubscribed) or four (oversubscribed) | USDC: `deposit_vault` (taking contributions), on activation `proceeds_vault` (the proceeds) is added and - only on oversubscription - `refund_vault` (the refunds plus the reserve); $1WIN: `token_vault` (the minted emission). All of them under the shared `vault_authority` |
| The Burn state | the state account + a counter-accumulator account | the daily and the weekly limit, the authorities (`owner`, `admin`, `operator`) and the Owner candidate (`pending_owner`, empty until a handover has begun - §9.1) - in the state account; the burnt-amount counters - in a separate accumulator singleton: the windows of the current UTC day and UTC week, where the window of an elapsed period is lazily reset on the first burn of a new one. The amount burnt by each burn lives in the events, not on-chain |
| The Vesting configuration | the state account (a singleton) | `owner`, `pending_owner` (the Owner candidate, empty until a handover has begun), `admin`, `operator`, `token_mint`, `withdrawal_authority`, `tge_ts`, `activity_count`, `vault_authority_bump` (§7.2) |
| A Vesting activity | an account per activity + a $1WIN vault per activity | `recipient`, `reference_amount`, `claimed`, `tge_bps`, `monthly_bps`, `cliff_months`; the number of tranches and the deadlines are not stored (§7.1). The activity's vault is under the shared `vault_authority`, and the bucket label lives only in the event |

The bumps of the PDA addresses are not stored in the Sale accounts, with the exception
of the `vault_authority` bump: a PDA address is derived every time by recomputing the
canonical bump rather than read out of an account. The stored `vault_authority` bump is
needed only as a seed byte when signing CPIs and takes no part in address checks.

The audit trail (the history of contributions, refunds and claims) lives in the
contract's events, which the backend indexes for the landing page. The correctness of
the operations themselves is fully guaranteed by the accounts; the events are an
auxiliary layer for display.

---

## 9. Governance and configuration

### 9.1 The principles

- Updating a parameter never requires a program upgrade. The conditions under which
  each parameter is mutable are in §5.1 and §5.2 (the sale) and §6.3 (Burn); the
  token's authorities are in §4.
- Changing Owner in the Token Sale, Burn and Vesting programs is two-step:
  `propose_owner(new)` names the candidate, and `accept_owner()` is executed by the new
  key itself, which is how it proves it owns it. Until it is confirmed the rights stay
  with the sitting Owner, and the nomination is replaced by another `propose_owner` or
  withdrawn with `cancel_owner_proposal()`. A wrong address never takes control: there
  is nothing to confirm it with. Changing Admin and Operator is one-step (`set_admin` /
  `set_operator`): the sitting Owner fixes a wrong assignment themselves.

### 9.2 The roles' keys

The operations are split into two classes by the level of authorization:

- **Operator (the automation key):** the automation - Blacklist add/remove, `refund`
  (before the pool is fixed, refunded strictly to the originating wallet), batch-closing
  accounts, the daily burn (`burn`, §6 - within the daily/weekly limits that Admin
  sets). Owner can replace the operator address at any time without an upgrade - up to
  and including moving the role onto a multisig.

- **The multisigs:** Owner MS and Ops MS are multisig accounts. The M-of-N quorum is
  assembled inside the multisig program; the on-chain execution reaches the 1WIN
  programs as a signature of its vault PDA (to the program it is an ordinary signer
  pubkey stored in `SaleState`).

- **The upgrade authority** of every program, Vesting included, is held by **Owner MS**.
  For Vesting this is a load-bearing requirement rather than merely an upgrade policy:
  the right to call `initialize` is bound to the upgrade-authority holder (§7.4). The
  singleton's `owner` role is set by an instruction argument (§7.3) and is not checked
  against the signer; in the normal flow the same Owner MS that calls `initialize` is
  written into it.

- Administrative decisions are prepared off-chain and reach the chain either as a
  multisig execution (the parameters, the schedule, activating Claim) or as a single
  signature of the operator key (the automation, within the limits the program checks).
- Compliance decisions are taken off-chain and have no on-chain role of their own:
  they reach the chain through the operator.
- Purchases and claims are signed by the users themselves (through their own wallet);
  these are not administrative operations.
- The Burn operation is a daily one, executed by the operator key; its ceiling is the
  limits of §6.3.

### 9.3 The deploy

At init each program references only mint addresses (USDC is external; $1WIN is created
at deploy time). No program references another or grants it any role - hence there are
no circular dependencies.

```
USDC mint (external)        $1WIN mint  (mint authority - Owner MS, freeze - Ops MS, supply 0)
                                 │  read-only
        ┌────────────────────────┼────────────────────────┐
   TokenSale                    Burn                   Vesting
 (deposit + token vault)                          (a vault per activity)
```

1. **The $1WIN SPL mint** (necessarily first) - the mint authority with Owner MS, the
   freeze authority with Ops MS, supply 0.
2. **TokenSale**, **Burn** and **Vesting** do not depend on one another and are deployed
   separately, in any order. TokenSale references both mints and creates
   `deposit_vault` and `token_vault` (the proceeds and refund vaults appear when Claim
   is activated). Vesting references only the $1WIN mint; the activity vaults are
   created together with the activities themselves (§7.2).

The upgrade authority of every program is Owner MS (§9.2). The order for Vesting:
deploy → `initialize` signed by Owner MS (the right to initialize is bound to the
upgrade-authority holder, §7.4) → creating the activities → minting the emission.

### 9.4 Security

- Every privileged operation is logged as an event.
- User funds sit in isolated vaults under PDAs; direct user access to a vault is
  impossible.

---
