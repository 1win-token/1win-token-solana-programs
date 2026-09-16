# Use cases of the $1WIN Token Sale program

The Sale program's user scenarios, by role and by lifecycle stage.

---

## Roles

| Role | Who it is | What it does |
|---|---|---|
| **Owner** | The project multisig | Initialization and upgrades (upgrade authority), the mint authority (minting the emission, revoking mint/metadata), withdrawing the proceeds (`withdraw_proceeds`) and the remainders (`sweep_unclaimed`), rotating the roles (`propose_owner` / `cancel_owner_proposal` / `set_admin` / `set_operator`). |
| **Admin** | The operations multisig | Every sale parameter (price, emission, per-transaction limits - only before the sale starts), the schedule, the pause, activating Claim, the Burn parameters, per-wallet limits (Graylist), the token's Freeze Authority |
| **Operator** | The automation key | The Blacklist, compliance refunds of deposits, batch closing of accounts |
| **User** | A sale participant | Deposits USDC, claims tokens and refunds |

---

## The lifecycle

The phases are not stored in state - the program derives the phase from the current
time and two flags (`pool_fixed`, `paused`):

```
PreSale → Sale → SaleEnded → (Claim activation) → Claim → ClaimClosed
```

The time-based transitions happen automatically; the only manual action is
activating Claim.

---

## 1. Preparing the sale (PreSale)

**Who:** Owner (initialization), Admin (parameters).

1. Owner initializes the program: the price, the emission, the per-transaction
   min/max limits, the start/end dates, and the claim window's start date and
   duration.
2. Two vaults are created under PDAs: the **USDC vault** (where the contributions
   go) and the **token vault** (where the $1WIN are handed out from).
3. Before the start, Admin edits the price, the emission, the min/max limits and the
   schedule.

**What changes:** a SaleState and the vaults are created; the parameters are
editable.

**The key point:** from the moment Sale starts, **the price, the emission and the
transaction limits are frozen for good** - the contribution terms cannot be changed
retroactively, structurally (a guard on `start_date`).

---

## 2. Purchasing (Sale)

**Who:** User.

1. The user makes a purchase.
2. Every contribution is its own `purchase` instruction: the USDC leaves the wallet
   for the USDC vault and a separate **Deposit** account is created (one
   contribution = one account = one unit of compliance review).
3. The program creates an **aggregate** once - the wallet's personal counter.
4. The aggregate's total contribution (`net`) grows, and so does the program's
   overall pool (`total`).

**The entry checks:** the phase is Sale, it is not paused, the wallet has a private
key of its own, the amount is within the limits (by default the global 10 - 250K
USDC; if the wallet has a Graylist record, its individual `min`/`max` and the
`max_total` ceiling apply, §3.4), and the wallet is not on the Blacklist.

**Who may take part:** only a wallet with a private key of its own. Program wallets
(a multisig, a custody service, a smart wallet) are not supported as buyers - both
creating an aggregate and purchasing from such an address revert
(`WalletNotOnCurve`). The check sits at the entry, so a program wallet never gets a
contribution, and without a contribution there can be neither a refund nor a claim
for it.

**What changes:** +a Deposit, aggregate.net += amount, pool.total += amount, and
USDC moves from the wallet into the USDC vault. The accounts' rent is paid by the
User (and comes back when they are closed).

---

## 3. Compliance control (before the pool is fixed)

**Who:** Operator (on an off-chain compliance decision).

### 3.1. Blocking a wallet

The backend reviews every contribution (per transaction). A risky wallet
(MEDIUM/HIGH) is added by the backend to the **Blacklist** through the program's
instruction (`blacklist_add`). The wallet's address is passed as an instruction
argument - any address can be blocked, whatever account occupies it.

**The effect:** new purchases from the wallet revert. The contributions already made
and the future claim stay unchanged. It is reversible through `blacklist_remove`.

### 3.2. Refunding a risky deposit

The Operator refunds a specific deposit: `refund(index)`, where the contributing
wallet is passed as an account (a deposit is bound to a wallet and an index through
its seeds) and `index` is the number of the purchase being refunded.

**What changes:** the USDC moves from the USDC vault **into a fresh USDC account
that the refund creates for this deposit and that belongs to the contribution's
original wallet** (the refund account), the Deposit account is closed into that same
account (its rent goes back to the user with the USDC), and aggregate.net and
pool.total decrease.

**The key points:**
- The amount and the recipient are taken **from the Deposit itself**, not from the
  arguments; the destination is the deposit's own program-derived account, so the
  Operator decides only whether to refund, never where.
- A refund is possible **only until the pool is fixed** (`!pool_fixed`).
- The refund depends on nothing the wallet holds: it does not use the wallet's ATA
  (whose SPL authority the wallet can hand to another key, which can be closed, or
  frozen by the issuer) and does not touch the wallet account itself. The new
  account is created inside the refund, paid for by the project - a refund cannot
  get stuck. The wallet then moves the USDC or closes the account at will; the
  account's address is derived from the wallet and the purchase index.
- The aggregate is not closed even at net = 0 - protection against address reuse
  (revival).

### 3.3. Per-wallet limits (the Graylist)

**Who:** Admin (the ops multisig).

A wallet can be given **individual limits** instead of the global ones: the minimum
and maximum amount per transaction and the maximum total for the whole sale
(`graylist_set(wallet, min_per_tx, max_per_tx, max_total)`).

**The effect in `purchase`:**
- when a Graylist record exists, its per-transaction `min`/`max` **fully replace**
  the global ones (10 - 250K), and `the wallet's contribution + the purchase amount
  ≤ max_total` is checked as well;
- with no record, the global limits apply.

**The key points:**
- The record is kept by Admin (a multisig) and its rent is paid by the project.
- The Graylist and the Blacklist are independent: while a block is active a purchase
  is rejected regardless of the limits.
- The Graylist slot is mandatory in `purchase` and bound by seeds - a wallet cannot
  "omit" the record to bypass `max_total` or get the global limits back
  (fail-closed).
- Individual limits are lifted by `graylist_remove` (the wallet returns to the
  global ones).

---

## 4. Fixing the pool and activating Claim (SaleEnded)

**Who:** Admin. The `activate_claim` instruction.

**Preconditions:** the phase is SaleEnded (`now ≥ end_date`), the pool is not fixed,
and the start of Claim still lies ahead (`now < claim_start`). The Claim schedule
(`claim_start`, the duration) is set at initialization and editable until the pool
is fixed; the activation does not take it as an argument.

**The claim-window gate:** activating at `claim_start` or later reverts
(`ClaimStartInPast`) - otherwise after fixation the phase would immediately read as
Claim (participants would get a shortened window) or ClaimClosed (there would be no
window at all, while withdrawal and vault closing would already be open to Owner).
If the fixation is late, Admin moves `claim_start` forward through
`set_claim_schedule` (available while the pool is not fixed, to any moment within
30 days of the edit) and activates again; the way out does not expire.

**The coverage gate:** the token vault must already hold enough $1WIN (Owner mints
in advance, or the mint and the activation go in one transaction). Otherwise the
instruction reverts with `VaultNotFunded`.

**The coverage check as its own instruction (`assert_funded`).** The token vault's
coverage is verifiable on-chain before the activation too: the instruction requires
`token_vault's balance ≥ the sale's emission` and, on a shortfall, reverts the
transaction it is part of (`VaultNotFunded`). Anyone can call it - the instruction
has no signer; it changes no state, moves no funds and emits no events. Its regular
usage is as a link in the issuance transaction: the credits into the vaults → the
checks (of the sale's vault and of every vesting activity) → the revocation of the
right to mint, all in one transaction. An underfunded vault fails the whole thing,
so the right to mint is not revoked until the sale's emission is assembled in the
vault, and a shortfall can still be fixed by minting more. Once the payouts begin
the check stops passing: the vault is spent by claims and the program does not
account for what was released.

**What changes (atomically):** `pool_fixed = true` (write-once, irreversible). The
instruction does not write the Claim schedule; the deadline
(`claim_start + duration`) is computed on demand and not stored.

**After fixation:** the pool and the allocations are final; purchases and refunds
are impossible; the claim schedule is immutable; a repeated activation reverts. The
Claim phase begins only with the pool fixed: while it is not, claiming is
unavailable even if `claim_start` has already arrived.

---

## 5. Claiming (Claim)

**Who:** the User, for itself, with a single `claim(wallet)` instruction (the signer
is the wallet that owns the contribution).

### Computing the allocation

The formula runs off the fixed values: `emission × price` (130M × $0.03 = $3.9M).

| Scenario | Condition | Tokens | USDC refund |
|---|---|---|---|
| **Undersubscribed** | pool ≤ soft cap | at the fixed price: net / price | 0 |
| **Oversubscribed** | pool > soft cap | a share of the emission: (net / pool) × emission | net − the cost of the tokens released |

### One instruction, atomically

`claim(wallet)` does it all in a single instruction: it releases the tokens, refunds
the USDC where needed, and closes the aggregate. The order:

1. Tokens from the token vault to the wallet (a zero payout is allowed).
2. Only when the refund is > 0 - the refund from the USDC vault to the wallet.
3. The aggregate is closed unconditionally (its rent goes to the user). The claim is
   complete.

**Replay protection:** the aggregate is closed → a revert (`AccountNotInitialized`).
The closed aggregate is the only guard against a repeated claim.

### Worked allocation examples

The parameters are the same in every example: a price of $0.03 USDC per 1 $1WIN, an
emission of 130,000,000 $1WIN, and a soft cap of 130M × $0.03 = **3,900,000 USDC**.
The "pool" is the total contribution of **every** participant; the six wallets in
each table are a representative sample and the rest of the volume comes from other
participants. `net` is a wallet's net contribution after the compliance refunds.

- **Undersubscribed / the boundary** (`pool ≤ soft cap`): `$1WIN = net / 0.03` and
  the USDC refund is 0.
- **Oversubscribed** (`pool > soft cap`): `$1WIN = (net / pool) × emission` and the
  USDC refund is `net − $1WIN × 0.03`.

The token count is rounded down (8 decimals of precision) and the cost of the
released tokens in the refund formula is rounded up; the indivisible remainder
("dust") stays in the vaults and is withdrawn after the deadline (TS-33). Under
oversubscription the effective price per token stays $0.03: the mechanism scales the
**number** of tokens in proportion to the share of the pool and returns the unspent
USDC.

**Integer arithmetic (how the program computes it).** Every amount is stored in
minimal units: USDC has 6 decimals (1 USDC = 10⁶ raw), $1WIN has 8 (1 $1WIN = 10⁸
raw), and the price is 30,000 raw USDC per token (0.03 × 10⁶). The multiplication
always happens **before** the division and the intermediates live in u128 (the
numerator `net × emission` does not fit into u64). The program computes the
allocation **per wallet** with this formula - `floor(net × emission / pool)`; it
neither computes nor stores an intermediate "$1WIN per 1 USDC" rate or a "refund
percentage" (computing `emission / pool` up front would be a division before a
multiplication, losing precision). The percentages below are given only as a
sanity-check reference. In each scenario below the sample's first wallet - a
contribution of 10 USDC - is worked through.

#### Scenario A - a pool of 5,000,000 USDC (oversubscribed ×1.28)

| Contribution (net), USDC | $1WIN to release | USDC refund | Paid for the tokens, USDC |
|---:|---:|---:|---:|
| 10 | 260 | 2.20 | 7.80 |
| 33 | 858 | 7.26 | 25.74 |
| 39 | 1,014 | 8.58 | 30.42 |
| 50 | 1,300 | 11.00 | 39.00 |
| 77 | 2,002 | 16.94 | 60.06 |
| 100 | 2,600 | 22.00 | 78.00 |

Worked through for a contribution of **10 USDC**:

```
raw:  net      = 10 × 10⁶ = 10 000 000
      pool     = 5 000 000 × 10⁶ = 5 000 000 000 000
      emission = 130 000 000 × 10⁸ = 13 000 000 000 000 000

1) Tokens (floor, multiplication before division, the numerator in u128):
   $1WIN_raw = ⌊ net × emission / pool ⌋
             = ⌊ 10 000 000 × 13 000 000 000 000 000 / 5 000 000 000 000 ⌋
             = ⌊ 1.3 × 10²³ / 5 × 10¹² ⌋
             = 26 000 000 000 raw = 260.00000000 $1WIN

2) The cost of what was released (ceil) and the refund:
   cost_raw   = ⌈ 26 000 000 000 × 30 000 / 10⁸ ⌉ = 7 800 000 raw = 7.80 USDC
   refund_raw = net − cost = 10 000 000 − 7 800 000 = 2 200 000 raw = 2.20 USDC

The division is exact: precisely 260 $1WIN and 2.20 USDC.
```

The sale's totals: the whole 130M $1WIN emission is distributed; the proceeds are
the soft cap of 3,900,000 USDC; the total refunded to participants is
5,000,000 − 3,900,000 = 1,100,000 USDC (the same 22% of the pool).

#### Scenario B - a pool of 5,472,916 USDC (oversubscribed ×1.40)

| Contribution (net), USDC | $1WIN to release | USDC refund | Paid for the tokens, USDC |
|---:|---:|---:|---:|
| 10 | 237.53333689 | 2.873999 | 7.126001 |
| 33 | 783.86001173 | 9.484199 | 23.515801 |
| 39 | 926.38001387 | 11.208599 | 27.791401 |
| 50 | 1,187.66668445 | 14.369999 | 35.630001 |
| 77 | 1,829.00669405 | 22.129799 | 54.870201 |
| 100 | 2,375.33336890 | 28.739998 | 71.260002 |

Worked through for a contribution of **10 USDC**.

```
raw:  net = 10 000 000;  pool = 5 472 916 × 10⁶ = 5 472 916 000 000
      emission = 130 000 000 × 10⁸ = 13 000 000 000 000 000

1) The tokens to receive (multiplication before division in u128, then floor):
   dividend = net × emission = 10 000 000 × 13 000 000 000 000 000
            = 130 000 000 000 000 000 000 000
   divisor  = pool           = 5 472 916 000 000

   130 000 000 000 000 000 000 000 / 5 472 916 000 000
       = 23 753 333 689   remainder 132 876 000 000   (the division is inexact)

   $1WIN_raw = 23 753 333 689 raw = 237.53333689 $1WIN
   → a non-zero remainder: the floor dropped ~0.024 raw $1WIN - they stay in the token vault

2) The cost (multiplication before division, then ceil) and the refund:
   23 753 333 689 × 30 000 = 712 600 010 670 000
   712 600 010 670 000 / 10⁸ = 7 126 000.1067  →  ceil = 7 126 001 raw = 7.126001 USDC
   → the ceil took ~0.893 raw USDC above the exact cost - that share stays in the USDC vault (dust)

   refund_raw = net − cost = 10 000 000 − 7 126 001 = 2 873 999 raw = 2.873999 USDC

The dust from this wallet: ~0.024 raw $1WIN (< 10⁻⁸ of a token) + ~0.893 raw USDC (< 10⁻⁶ USDC).
```

The totals: 130M $1WIN; proceeds of 3,900,000 USDC; refunds of
5,472,916 − 3,900,000 = 1,572,916 USDC (~28.7%). Practically every wallet leaves
some dust: the floor on the tokens and the ceil on the cost leave fractions of a
minimal unit in the vaults. Per wallet that is < 1 raw (less than $0.000001), but
across every participant it adds up - a reserve of 1 raw USDC per participant is
withheld from the proceeds withdrawal against it (§6), and the remainder is
collected through `sweep_unclaimed` after the deadline.

#### Scenario C - a pool of exactly 3,900,000 USDC (= the soft cap, the boundary)

| Contribution (net), USDC | $1WIN to release | USDC refund | Paid for the tokens, USDC |
|---:|---:|---:|---:|
| 10 | 333.33333333 | 0 | 10 |
| 33 | 1,100 | 0 | 33 |
| 39 | 1,300 | 0 | 39 |
| 50 | 1,666.66666666 | 0 | 50 |
| 77 | 2,566.66666666 | 0 | 77 |
| 100 | 3,333.33333333 | 0 | 100 |

Worked through for a contribution of **10 USDC** (pool = the soft cap → the
undersubscribed branch, at the fixed price):

```
raw:  net = 10 000 000;  price = 30 000 raw USDC per token

1) Tokens at the fixed price (floor):
   $1WIN_raw = ⌊ net × 10⁸ / price ⌋ = ⌊ 10 000 000 × 10⁸ / 30 000 ⌋
             = ⌊ 10¹⁵ / 30 000 ⌋ = ⌊ 33 333 333 333.33… ⌋
             = 33 333 333 333 raw = 333.33333333 $1WIN
2) refund = 0 (there is no refund when undersubscribed)
```

The totals: exactly the whole 130M $1WIN emission is sold; the proceeds are the
whole pool of 3,900,000 USDC; there are no refunds. Any pool below 3.9M behaves the
same way (a refund of 0), the emission simply is not sold out in full.

---

## 6. Withdrawing the proceeds (from the start of Claim)

**Who:** Owner. The `withdraw_proceeds` instruction (it takes no amount argument),
available from `claim_start` onwards (the pool is fixed by then): while participants
cannot actually claim, the protocol does not withdraw the proceeds.

**The effect:** a single call moves the **whole** balance of `proceeds_vault` to
Owner's USDC ATA and closes `proceeds_vault` (its rent goes to the collector address
that was passed in). A repeated call fails - the vault no longer exists.

**The limit:** a physical one only - the `proceeds_vault` balance. At activation
exactly the proceeds are placed into `proceeds_vault`: under oversubscription
`soft cap − participants × 1 minimal USDC unit`, and when undersubscribed the whole
pool. The margin withheld under oversubscription (≤ 1 USDC even with a million
wallets) sits in a separate `refund_vault` that `withdraw_proceeds` cannot reach, and
it covers the small change lost to rounding for the last claimers (TS-40, Arch
I9/§10.7). The `refund_vault` remainder is collected for the project through
`sweep_unclaimed` after the deadline.

---

## 7. Closing (ClaimClosed, after the deadline)

**Who:** Owner, Operator (the batch closing).

1. The claim instructions revert.
2. **sweep_unclaimed** (Owner): the unclaimed $1WIN and USDC go to the project.
3. **The batch closing** (Operator): the remaining Deposits and aggregates are
   closed in batches after the deadline; their rent goes back to the user owners.

---

## 8. Governance and operations (cross-cutting scenarios)

### 8.1. The pause

Admin sets the pause during the Sale phase; it can be lifted in any phase. It blocks
**purchasing only** (creating an aggregate and `purchase`); refund, claim, withdraw
and sweep keep working. The schedule does not move - SaleEnded arrives as planned.
It is the tool for stopping the intake early.

### 8.2. Changing the schedule and the transaction limits

Admin edits the start and end dates **only before the Sale phase starts**. From the
start onwards the sale's schedule is immutable: a running or finished sale cannot be
extended. Stopping the intake early is done with the pause (§8.1); an attempt at
`set_sale_schedule` after the start reverts with `SaleAlreadyStarted`.

Both dates are bounded from above, both at initialization and on every edit: the
start is set no further than 90 days ahead (`StartDateTooFar`) and the intake window
is no longer than 30 days (`SaleDurationTooLong`). The limits are deliberately wider
than the planned schedule (~2 weeks) and exist only so that a typo of the "start in
100 years" or "a 50-year sale" kind never reaches the state. The transaction limits
(min/max) obey the same guard - editable only in PreSale, and after the start
`set_tx_limits` reverts with `SaleAlreadyStarted`.

The Claim schedule (`claim_start`, the duration) is the one parameter that stays
editable after the sale starts - until the pool is fixed (`set_claim_schedule`,
`!pool_fixed`). The start must lie ahead of both the sale's end and the edit itself,
and no more than 30 days after the later of the two (`ClaimStartInPast` /
`ClaimStartTooFar`); the duration is no shorter than an hour and no longer than
12 months (`ClaimDurationTooShort` / `ClaimDurationTooLong`): a window that would
close before participants can use it never reaches the state, because after the
fix nobody could widen it. Because the ceiling follows the edit rather than the
sale's end, a late activation is always recoverable: Admin
moves the start forward and activates (§4).

### 8.3. Changing the roles

- Owner → a new Owner: **two steps**. The sitting Owner calls `propose_owner(new)` -
  the candidate is recorded but the rights are still its own. Then the candidate
  itself calls `accept_owner()` from its own key - the role moves and the candidate
  is cleared.
  - Until the second step is taken everything works as before: the owner
    instructions are executed by the sitting key and the candidate holds no owner
    rights.
  - The nomination is corrected by a repeated `propose_owner` - the previous
    candidate is displaced and can no longer accept the role.
  - A handover in flight is withdrawn entirely: `cancel_owner_proposal()` removes the
    candidate and the state returns to "no handover". Without it a nominated key
    could step into the role at any later moment.
  - A mistake in the address does not cost control: only the key's holder can confirm
    the nomination, so a handover to a foreign or non-existent address simply never
    completes.
- Owner rotates Admin and Operator (`set_admin` / `set_operator`) in one step, at any
  time - including on a compromise of an operational key. A second step is
  unnecessary here: Owner fixes a mistaken assignment itself.

---

## 9. Negative and edge scenarios

| Scenario | The program's behaviour |
|---|---|
| A purchase outside Sale / while paused / outside the limits / from the Blacklist / after fixation | Revert |
| A purchase (or aggregate creation) from a program wallet - a multisig, custody, a smart wallet | `WalletNotOnCurve` revert - only a wallet with a private key of its own may take part |
| A participant hands its account over to a third-party program to escape the ban, the limits and the refund | It does not help: the lists and `refund` treat the wallet as an address (an instruction argument, or an account bound by seeds) and never read the account's owner. The ban, the individual limits and the refund still apply to it |
| A participant tries to block a forced refund through its destination: closes its USDC ATA, hands the ATA's SPL authority to another key, gets the ATA frozen, pre-funds the refund account's address, or turns the wallet into an executable program account | It does not help: `refund` creates a fresh account at a program-derived address that only the program can create, sends the USDC and the deposit's rent there, and neither reads nor writes the wallet's existing accounts or the wallet itself. A pre-funded address is completed to the token account; a substituted address is rejected (`ConstraintSeeds`) |
| A purchase from a Graylisted wallet with an amount outside its `min`/`max` | A `BelowMin` / `AboveMax` revert (the individual limits replace the global ones) |
| A purchase from a Graylisted wallet whose total contribution exceeds `max_total` | A `WalletTotalExceeded` revert (`contribution + amount > max_total`) |
| An attempt to bypass the Graylist / `max_total` by not passing the Graylist slot | Impossible: the slot is mandatory and bound by seeds (fail-closed) |
| A Graylist record with `min > max` or `max > max_total` | An `InvalidParams` revert on `graylist_set` - an "unreadable" record is never created |
| Refunding a deposit after the pool is fixed | A revert - the computation is final (fail-closed) |
| A repeated Claim activation, or editing the Claim schedule after the pool is fixed | A revert - write-once / `!pool_fixed` |
| Activating Claim with an underfunded token vault | A `VaultNotFunded` revert |
| Revoking the right to mint with an underfunded token vault | It does not go through: `assert_funded` in the same transaction requires `balance ≥ the sale's emission` and fails it with `VaultNotFunded` before the revocation. The guarantee rests on the transaction's composition: the assembler must include the check (the pre-flight procedure), and the program cannot demand it |
| `assert_funded` during Claim on a fully covered sale | A `VaultNotFunded` revert - expected behaviour rather than a defect: the vault is already being spent by claims and the program does not account for what was released. The check is meant for the window before the first payouts |
| The Claim activation is late: `claim_start` has already arrived (or the deadline has passed), even long after the sale's end | A `ClaimStartInPast` revert - the claim window cannot open retroactively. The way out: Admin moves `claim_start` forward (`set_claim_schedule`: later than now, within 30 days of the edit) and activates again; the window is handed out at full length |
| `set_claim_schedule` with a start that has already passed, or more than 30 days after the later of the sale's end and the edit | A `ClaimStartInPast` / `ClaimStartTooFar` revert - the setter and the activation agree on one predicate, so a start the activation would reject never reaches the state |
| A repeated claim | A revert - the aggregate is closed (`AccountNotInitialized`) |
| Changing the schedule or the transaction limits after the sale starts | A `SaleAlreadyStarted` revert - frozen from the start |
| A typo in the sale schedule: a start beyond 90 days or an intake window longer than 30 days | A `StartDateTooFar` / `SaleDurationTooLong` revert - both on `initialize` and on `set_sale_schedule` |
| A Claim window of seconds or minutes, or longer than 12 months | A `ClaimDurationTooShort` / `ClaimDurationTooLong` revert - both on `initialize` and on `set_claim_schedule`; a window nobody can use must never reach the state, because after the pool is fixed nobody can widen it |
| Never claimed before the deadline | The tokens and the refund are swept to the project treasury; the aggregate's rent comes back on the batch closing |
| A risky wallet identified after the claim | The program is powerless; the break-glass measure is freezing the token account through the Freeze Authority (outside the program, held by the ops multisig / Admin) |
| A full refund of a wallet's contributions, then a new purchase | The new deposit gets a new index (the counter never decreases) - address reuse is impossible |
| Withdrawing more than the proceeds | Impossible: `withdraw_proceeds` moves the whole `proceeds_vault` balance and closes it; at activation exactly the proceeds were placed into that vault and the rounding margin was withheld in a separate `refund_vault` |
| A compromised Operator: mass blacklisting | Griefing rather than theft - only purchasing is blocked and it is reversible with `blacklist_remove`; Owner rotates the Operator |
| A compromised Admin or a mistake: overly tight Graylist limits | Griefing rather than theft - the wallet's purchases are blocked; Admin is a multisig (a lower risk); it is reversible with `graylist_remove`; Owner rotates the Admin |
| A compromised Operator: mass refunds | The funds go only into accounts owned by the contributors' original wallets (the deposits' refund accounts) - sabotage of the collection, not theft; each refund also costs the project the new account's rent |
| An attempt to close SaleState / the vaults / a Blacklist / a Graylist through a batch | A revert - the discriminator and PDA checks (a batch closes only Deposit/WalletAggregate) |
| Owner nominated to a mistaken address (a typo, a foreign or a non-existent key) | Control is not lost: `propose_owner` only records the candidate and `accept_owner` requires its signature. The rights stay with the sitting Owner, and the nomination is replayed with a repeated `propose_owner` or withdrawn with `cancel_owner_proposal` |
| `accept_owner` with no nomination, or from a foreign key | A `NoPendingOwner` / `Unauthorized` revert - only the nominated candidate may step into the role |
| `cancel_owner_proposal` when no handover is in flight | A `NoPendingOwner` revert - there is nothing to withdraw and the instruction does not pass vacuously |
| The candidate tries to withdraw its own nomination | An `Unauthorized` revert - the withdrawal belongs to whoever nominated (the sitting Owner) |

---
