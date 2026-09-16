# Business requirements for the 1WIN programs

> This document holds business requirements only - the list of what the system must
> do. Their technical elaboration (PDAs, account structures, arithmetic,
> instructions, deployment) lives in
> [1WIN_Programs_Technical_Requirements.md](1WIN_Programs_Technical_Requirements.md)
> and in the architecture documents.

## Contents

1. [System-wide requirements](#1-system-wide-requirements)
2. [The $1WIN token](#2-the-1win-token)
3. [TokenSale](#3-tokensale)
4. [Burn](#4-burn)
5. [Vesting](#5-vesting)

---

## 1. System-wide requirements

### 1.1 Roles

| Role | Who | Powers |
|---|---|---|
| **Owner** | The project multisig | Deployment and upgrades (upgrade authority), token issuance (mint authority and minting the emission, including into the vesting activity vaults, and revoking mint/metadata to None), withdrawing the proceeds (`withdraw_proceeds`) and the unclaimed funds (`sweep_unclaimed`), role management (ownership handover - `propose_owner` / `accept_owner` / `cancel_owner_proposal` - and assigning the other roles - `set_admin` / `set_operator`), managing the Vesting program (creating activities with their schedules and cancelling them before the unlock starts, closing exhausted ones; the unlock start date, an activity's recipient change and the address for the remainder and the rent belong to Admin) |
| **Admin** | The administration multisig | Every sale parameter, the phase schedule, activating Claim, the Burn parameters, the pause, per-wallet limits (Graylist), the token's Freeze Authority; in Vesting - the unlock start date (before it arrives), an activity's recipient address (at any point in its life) and the project address that receives the remainder and the rent when an activity is cancelled or closed |
| **Operator** | The automation key | Automation: blocking (Blacklist), refunding risky deposits, closing exhausted records; executing the daily burn (§4); triggering a vesting claim (the tokens go to the recipient, §5) |
| **User** | A sale participant | Deposits USDC, claims |
| **Vesting recipient** | The project wallet of an emission bucket - an ordinary wallet with a private key of its own (VE-19) | Claims the unlocked $1WIN of its vesting activity |

### 1.2 Governance

- **G-1.** The roles' permission sets are disjoint: Owner does not inherit Admin's
  operations, and Admin does not inherit Operator's automation.
- **G-2.** Administrative operations are initiated through the backend and executed
  by the signature of the corresponding multisig (Owner, Admin, Operator).
  Purchases and Claim are signed by the user.
- **G-3.** Changing the Owner in the TokenSale and Vesting programs is two-step: the
  sitting Owner nominates a candidate and the role moves only once the candidate
  confirms it with its own signature. Until then the rights stay with the sitting
  Owner, and the nomination can be replaced or withdrawn. That makes a typo in the
  address harmless: an address nobody can sign with never produces a confirmation
  and control goes nowhere. If between the nomination and the confirmation the
  candidate turns out to be the same program's Operator, the confirmation fails -
  the Owner and Operator roles must differ; after the Operator is changed the
  confirmation goes through. In the Burn program the Owner change is one-step; there
  the risk of a typo is removed by the backend offering only the multisig addresses
  the system already knows.
- **G-4.** Owner changes the Admin and Operator addresses at any time; the Operator
  role may be moved onto a multisig.
- **G-5.** Every business parameter is set and updated by the authorized roles
  without redeploying the programs.
- **G-6.** Compliance decisions are taken off-chain; they have no on-chain role of
  their own and are executed by the Operator.

### 1.3 Compliance and security

- **G-7.** Only wallet public keys and amounts are stored on-chain. Neither the
  reasons for blocking nor risk scores live on-chain - the risk grading is kept
  off-chain.
- **G-8.** User funds sit in isolated vaults under the programs' control; users have
  no direct access to them.
- **G-9.** Burn operates on funds isolated from the sale's pool.
- **G-11.** Every privileged operation is logged; the full history (deposits,
  refunds, claims) is available to the backend for display on the landing page.
- **G-12.** The token ($1WIN) is deployed first - the other programs reference it
  read-only; the programs (TokenSale, Burn, Vesting) are independent and deployed
  separately, in any order.

---

## 2. The $1WIN token

- **T-1.** The standard is classic SPL Token (not Token-2022): maximum compatibility
  with exchanges and DEXes; the token trades freely.
- **T-2.** Precision is 8 decimal places.
- **T-3.** The token's total supply is **1,000,000,000 $1WIN**: **130,000,000** are
  distributed through TokenSale (the sale's emission) and the remaining
  **870,000,000** through the vesting buckets (§5). The buckets' amounts are set when the
  activities are created.
- **T-4.** During the sale the right to mint belongs to Owner; it is not handed to
  the TokenSale program - the program hands tokens to participants out of its own
  vault.
- **T-5.** The sale's distributable amount is issued **once, before Claim is
  activated**: under oversubscription the whole sale emission (130M), under
  undersubscription the amount actually bought.
- **T-6.** Owner revokes the right to mint (to None) only after the whole
  distributable amount has been issued - both the sale's emission and every vesting
  bucket's; after the revocation the total supply is fixed for good. The right to
  change the metadata is likewise revoked once the metadata is final.
- **T-7.** The token's freeze authority belongs to the multisig.
- **T-8.** Owner mints the vesting buckets' emission into the activity vaults of the
  Vesting program. The whole amount (the sale plus every vesting bucket) is minted
  and the right to mint is revoked (T-6) **in a single transaction before the
  vesting starts**: a sequence of credits into every vault, then the coverage check
  of the sale's vault (TS-30) and of every vesting activity (VE-6), then moving the
  mint authority to None - atomically. After that minting becomes unavailable.

---

## 3. TokenSale

### 3.1 The sale mechanics (oversubscription)

- **TS-1.** The sale runs on oversubscription mechanics: participants deposit USDC
  into a shared pool from the landing page and sign the transactions themselves.
- **TS-2.** The price is fixed - **$0.03 USDC per 1 $1WIN** - and does not depend on
  the pool's size.
- **TS-3.** The collection lasts about 2 weeks (the Sale phase). The program bounds
  the schedule from above: the sale's start is set no further than 90 days ahead and
  the intake window itself may not exceed 30 days. That is not a business constraint
  but a safeguard against input error: the limits are deliberately wider than the
  planned schedule.
- **TS-4.** The soft cap is a derived quantity, `emission × price`; it is not set
  separately.
- **TS-5.** Once the collection ends the pool is fixed:
  - **pool > soft cap (oversubscribed)** - tokens are distributed pro rata to the
    contributions and the excess USDC is returned to participants;
  - **pool ≤ soft cap (undersubscribed)** - everyone buys at the fixed price and
    there is no refund.

### 3.2 Parameters

| Parameter | Reference value | Who changes it | When |
|---|---|---|---|
| Token price | $0.03 USDC | Admin | Before the Sale phase starts |
| Emission amount | 130,000,000 $1WIN | Admin | Before the Sale phase starts |
| Minimum purchase | 10 USDC | Admin | Before the Sale phase starts |
| Maximum purchase | 250,000 USDC | Admin | Before the Sale phase starts |
| Sale start date | - | Admin | Before the start; no further out than 90 days from the edit |
| Sale duration | ~2 weeks | Admin | Only before the start; afterwards the schedule is immutable; at most 30 days |
| Claim start date | - | Admin | At initialization; editable until the pool is fixed; later than the sale's end and than the moment of the edit, and no more than +30 days from the later of the two |
| Claim duration | 12 months | Admin | At initialization; editable until the pool is fixed; at least 1 hour and at most 12 months |

  The values above are the reference configuration this document works with; each of
  them is an initialization parameter rather than a constant of the program.

- **TS-6.** The maximum limit applies per purchase; the number of purchases from one
  wallet is unlimited. A wallet's total contribution is not globally capped but may
  be capped individually through the Graylist (`max_total`).
- **TS-7.** Every parameter except the Claim schedule is editable **only before the
  Sale phase starts**; from its start onwards the parameters are immutable.

### 3.3 Phases and schedule

- **TS-8.** The sale's lifecycle: **PreSale → Sale → SaleEnded → Claim →
  ClaimClosed**:
  - *PreSale* - purchasing is closed and the parameters are editable;
  - *Sale* - USDC intake is open (unless paused);
  - *SaleEnded* - intake is closed and the compliance window runs (about a day
    before Claim is activated);
  - *Claim* - tokens are handed out and USDC is refunded;
  - *ClaimClosed* - the unclaimed funds are withdrawn to the project.
- **TS-9.** The phase transitions are one-way; a phase cannot be re-entered.
- **TS-10.** The pause blocks purchases only and does not move the schedule; there
  is no separate "end the sale manually" operation - the pause is what stops the
  intake early. Pausing and resuming belong to Admin; a pause can only be set during
  the Sale phase and lifted at any time.
- **TS-11.** Claim can be activated only after the scheduled end of the Sale and
  only once the emission has been issued.

### 3.4 Purchasing

- **TS-12.** A purchase goes through when, simultaneously: the phase is Sale, the
  pause is off, the amount is within the per-transaction min/max, the buyer's wallet
  has a private key of its own (TS-12a) and the wallet is not on the Blacklist.
- **TS-12a.** Only a wallet with a private key of its own - one the user signs with
  - may take part in the sale. Program wallets (a multisig, a custody service, a
  smart wallet) are not supported as buyers: a purchase from such an address is
  rejected by the program. The participant's remaining steps - a deposit refund and
  Claim - are unreachable for such an address, because it will never have a
  contribution.
- **TS-13.** One wallet may contribute many times; the contributions add up and each
  is recorded separately (for per-transaction compliance).
- **TS-14.** The contract is the source of truth about a wallet's contribution; the
  total is read from it directly. Aggregate figures ("the pool's size", "the number
  of holders", "the top participants") are collected by the backend.

### 3.5 Compliance, blocking and per-wallet limits

- **TS-15.** Risk scoring is performed off-chain. Only its outcome is applied
  on-chain - the blocking records (Blacklist) and the per-wallet limits (Graylist).
  The risk levels themselves, the reasons and the risk scores are not on-chain
  (G-7).
- **TS-15a.** The **Graylist** holds a specific wallet's individual contribution
  limits: the minimum and maximum amount per transaction and the maximum total for
  the whole sale. When a Graylist record exists for a wallet, its per-transaction
  `min`/`max` **fully replace** the global ones and the wallet's total for the sale
  is additionally checked: `current contribution + purchase amount ≤ max_total`. A
  Graylist record is created by Admin and its rent is paid by the project. The
  Graylist record and the Blacklist record are independent (a wallet may be on both
  lists at once); while a block is active a purchase is rejected regardless of the
  Graylist.
- **TS-16.** Every purchase is checked against the Blacklist and the Graylist by the
  program itself, not by the frontend.
- **TS-17.** The compliance check runs per purchase: a wallet whose off-chain
  assessment requires it is placed on the Blacklist and new purchases from it are
  rejected. The decision to refund deposits and to remove a wallet from the
  Blacklist is likewise taken off-chain.
- **TS-18.** The Blacklist closes **purchasing only**: it affects neither existing
  contributions nor Claim. Adding and removing is done by the Operator, at any time.
- **TS-19.** Blocking and refunding are independent decisions: the Blacklist means
  "you buy no more", a refund means "return the money".
- **TS-20.** A risky deposit is refunded individually, per purchase: the deposit
  being refunded is identified by the contributing wallet (passed as an account) and
  the purchase's index; the USDC goes strictly into an account controlled by the
  original wallet - a fresh account the refund creates for that deposit, so that
  nothing the participant holds can block the return - and the wallet's total
  contribution and the pool's size decrease. The same deposit cannot be refunded
  twice.
- **TS-21.** Refunds are available **only until the pool is fixed**; afterwards the
  pool and the allocations are final.
- **TS-22.** Refunding all of a wallet's deposits zeroes its contribution - the
  wallet stops having any claim.
- **TS-23.** On a retroactive risk increase: new purchases are rejected; past
  deposits are refundable only until the pool is fixed; after fixation the right to
  Claim remains, and the last resort is freezing the token account after the payout.
- **TS-24.** The compliance window between the end of the Sale and the activation of
  Claim (~1 day) exists so that every alert can be worked through and the risky
  deposits subtracted from the pool **before it is fixed**; a separate vault for
  high-risk funds is not required.

### 3.6 Claim

- **TS-25.** The contract is the only source of truth about participation and
  allocation (an internal database is not a source of truth).
- **TS-26.** Activating Claim is a single Admin operation: it fixes the pool. It is
  available only after the Sale ends and is performed once. The Claim phase begins
  only after the pool is fixed - before that claiming is unavailable even if the
  start date has already arrived.
- **TS-27.** The Claim start date is set at initialization together with the Sale
  schedule and is editable until the pool is fixed. It always lies ahead: after the
  Sale ends and after the edit itself, and no later than 30 days after the later of
  the two - so a valid start can be scheduled at any time before the pool is fixed,
  and a late activation is recovered by moving the start forward.
- **TS-28.** The Claim window is always full: the deadline = the start date plus the
  duration; moving the start date moves the beginning but does not shorten the
  window.
- **TS-29.** Before Claim is activated, handing out tokens, refunding USDC and
  withdrawing the unclaimed funds are all impossible - funds cannot be taken out
  around the participants.
- **TS-30.** The emission is issued into the vault before Claim is activated;
  without the issuance the activation is impossible. The program provides an
  on-chain coverage check of the vault: the vault's balance is at least the sale's
  emission. The check changes no state, stores no "the vault is funded" flag and is
  available to any caller; it does not verify the precision of the issued amount -
  controlling the amounts and tracking the vault balances happens off-chain. The
  check must be in the same transaction as the revocation of the right to mint
  (T-8): only then does an underfunded vault cancel the transaction before the
  irreversible revocation. It applies before the payouts begin: once Claim starts
  the vault is spent by claims, and the coverage of the unreleased obligation cannot
  be derived from the program's state.
- **TS-31.** The right to Claim requires a positive wallet contribution and a time
  inside the Claim window. A second payout is impossible.
- **TS-32.** The allocation is computed at Claim time from the wallet's contribution
  (after compliance refunds) and the fixed pool: when undersubscribed, at the fixed
  price with a USDC refund of 0; when oversubscribed, a share of the emission pro
  rata to the contribution, with the excess USDC returned. For example: a pool of
  $10M and a contribution of $10,000 → 130,000 $1WIN and a refund of $6,100 USDC.
- **TS-33.** Rounding favours the protocol: a refund never exceeds what is due. The
  indivisible remainder ("dust") stays in the vault and is withdrawn after the
  deadline.
- **TS-34.** Claim is a single operation: the $1WIN payout followed by the return of
  the excess USDC to the same wallet the contribution came from. Inside the
  operation the $1WIN payout comes first and the USDC refund second. With a zero
  refund only the $1WIN payout happens, but it remains the same single operation.
  Claim is initiated by the user.
- **TS-36.** A USDC refund left unclaimed by the deadline stays in the vault and is
  withdrawn to the project: those are the funds of wallets that never claimed at
  all, like any other unclaimed remainder.

### 3.7 Proceeds and unclaimed funds

- **TS-38.** The protocol's proceeds = `min(pool, soft cap)`: the whole pool when
  undersubscribed, the soft cap when oversubscribed. The rest of the pool is the
  obligation to refund participants.
- **TS-39.** Owner may withdraw the proceeds only from the moment Claim begins:
  while participants cannot actually claim, the protocol receives no proceeds.
- **TS-40.** The total withdrawal of proceeds is reduced by a small reserve (a
  rounding allowance) so that the refunds of the last claimers are guaranteed to be
  covered; the reserve is collected by withdrawing the remainder after the deadline.
- **TS-41.** Participants' unpaid refunds stay in the vault until the deadline; they
  cannot be withdrawn to the project early.
- **TS-42.** After the deadline Owner withdraws the remainders of both vaults ($1WIN
  and USDC) to the project, including the undistributed amounts and the rounding
  remainder.
- **TS-43.** Exhausted service records are closed and the funds reserved for their
  storage are returned to their user owners: a wallet's contribution record by the
  claim; everything the claim did not close (the deposit records and the
  contribution records of wallets that never claimed) after the deadline. The
  closing is performed by the Operator. A deposit record refunded before the pool is
  fixed is closed by the refund itself, and its storage reserve reaches the user
  through the account that received the refund.

---

## 4. Burn

### 4.1 Purpose and model

- **BR-1.** A daily burn of part of the $1WIN spent on the platform - it permanently
  reduces the total supply.
- **BR-2.** The amount of tokens spent over the day is computed by the platform, and
  the burn is executed by the **Operator** with its own signature - a daily
  operation is automated and does not require assembling a multisig. The platform
  transfers the tokens to burn into the program's vault, and the burn goes out of
  it; the Operator triggers the burn but holds no authority over the vault.
- **BR-3.** The burn amount is trusted off-chain input; what is controlled on-chain
  are the roles and the limits. The result of every burn is published as an event.

### 4.2 Rules and limits

| Parameter | Initial value | Who changes it | When |
|---|---|---|---|
| Daily burn limit | Set at initialization | Admin | At any time |
| Weekly burn limit | Set at initialization | Admin | At any time |

- **BR-4.** Every burn is published as an event: the period, the amount burned and
  the totals accumulated for the day and the week. Those data are not stored
  on-chain. The planned amount does not go into the event: it does not affect
  execution, and as a signer's self-report it confirms nothing - the planning stays
  off-chain.
- **BR-5.** The burned totals are bounded by the daily and weekly limits; an amount
  above a limit is rejected.
- **BR-6.** If the burn amount is zero, no burn is performed.
- **BR-7.** The rights to burn and to set the limits are separated (G-1): **only the
  Operator** may burn and **only Admin** may change the limits. Neither Admin nor
  Owner executes a burn; the limits are the only on-chain control over the
  Operator's hot key. The Operator's address is assigned by Owner at initialization
  and changed at any time (`set_operator`).

---

## 5. Vesting

### 5.1 Purpose and model

- **VE-1.** Vesting is a separate program for the gradual unlock of the $1WIN
  allocated to the emission buckets outside TokenSale.
- **VE-2.** The program runs several independent **activities**. An activity is one
  recipient, one $1WIN emission amount and one unlock schedule; each has its own
  isolated $1WIN vault. The unlock start is shared by every activity (VE-7).
- **VE-3.** The emission buckets and their unlock rules (the schedule is set when an
  activity is created):

| Bucket (activity) | TGE (on the start date) | Cliff | Monthly |
|---|---|---|---|
| Growth | 100% | - | - |
| Marketing | 100% | - | - |
| Ecosystem | 10% | none | 5% |
| Community Rewards | 0% | none | 8.34% |
| Treasury | 0% | 9 months | 4% |
| Liquidity | 10% | none | 7.5% |

  The percentages are shares of the activity's **emission amount**. "100% at TGE" is
  a special case: the whole amount unlocks at once on the start date.

### 5.2 The emission, the unlock amount and the vaults

- **VE-4.** An activity's unlock is computed against its **emission amount set at
  creation** (the reference amount) rather than against the vault's current balance.
  Stray transfers into the vault do not affect the computation.
- **VE-5.** Every activity has its own $1WIN vault under the program's control. The
  vaults are isolated: one activity's unlock and payout never touch another's funds.
- **VE-6.** A bucket's emission is issued into an activity's vault by Owner (the
  token's mint authority, T-8); the program has no issuance operation of its own.
  The program provides an on-chain coverage check of an activity: its vault's
  balance plus what has already been released to the recipient is at least the
  emission amount - that is, the unreleased part of the obligation is covered. The
  check changes no state and stores no "the activity is funded" flag; it does not
  verify the precision of the amount (issuing exactly what the tokenomics
  prescribes) - controlling the minted amounts and tracking the vault balances
  happens off-chain. The checks for every activity must be in the same transaction
  as the revocation of the right to mint (T-8): only then does an underfunded vault
  cancel the transaction before the irreversible revocation. Beyond that, the chain
  guarantees that a claim never hands out more than what is unlocked or more than
  the vault's balance holds (VE-13).

### 5.3 The start date, creation and edits

- **VE-7.** The unlock start is **shared by every activity**. The TGE instant is set
  by Owner as a specific date (at initialization, editable by a separate operation
  until the unlock starts); there is no separate activation button. Once TGE
  arrives, the instant becomes immutable and every activity's unlock is counted from
  it. Every bucket's emission is minted into the vaults and the right to mint is
  revoked **before TGE arrives** (T-8).
- **VE-8.** An activity is created by Owner **before TGE arrives**. Its creation
  sets: the recipient's address, the emission amount and the unlock rules - **the
  TGE unlock share, the monthly share and the cliff length**. What is validated: the
  shares are within 0-100%; with a zero monthly share the TGE share must be 100%
  (otherwise the amount never unlocks in full); the cliff length and the derived
  number of months are within their permitted ranges; and the emission amount is
  positive. An activity cannot be created after TGE arrives - it would be unlocked
  immediately.
- **VE-9.** The recipient's address is changed by Admin, **at any point in the
  activity's life** - both before TGE and after it. The change alters neither the
  obligation's amount nor the accounting of what was already released: what was
  released earlier stays with the previous wallet and only the remainder not yet
  drawn down goes to the new address. The change does not require the recipient's
  consent; every change is recorded as an event, and that event is the operation's
  only trace, which is why the right belongs to the project multisig and the event
  must be monitored.
- **VE-10.** Owner may cancel a mistakenly created activity **only before TGE
  arrives**: the record and the vault are closed. If tokens have already been minted
  into the vault they are returned to the project; if the vault is empty (the
  emission has not been minted yet) no transfer is performed. Other corrections
  before TGE (the amount, the schedule) are made by cancelling and creating again. A
  recreated activity can only be filled by minting, so the actual correction
  boundary is **the revocation of the mint authority rather than TGE**: the right to
  mint is revoked by the atomic minting transaction before TGE (T-8), and after the
  revocation there is nothing left to fund a recreated activity with. Once TGE
  arrives the set of activities, their amounts and their schedules are final,
  cancellation is impossible and an activity's funds are no longer withdrawn to the
  project (VE-14); the recipient's address is not part of that freeze (VE-9).

### 5.4 The unlock timeline

- **VE-11.** The unlock follows the calendar from the schedule's start date. A month
  is 30 days. The TGE share (if any) unlocks on the start date (day 1); the monthly
  shares unlock every 30 days (day 31, 61, …). Without a cliff the first monthly
  tranche is on day 31; with a cliff of N months it is on day N×30+1. The final
  tranche tops the total up to the full emission amount.

```
The unlock schedule (time from the start date; a month = 30 days)

Ecosystem - 10% at TGE, then 5% per month, no cliff:
  TGE (day 1)      +30d     +60d     +90d    …    day 541
      10%    ─▶    +5%  ─▶  +5%  ─▶  +5%  … ─▶  +5% (topped up to 100%)
  total:  10 %      15 %     20 %     25 %   …      100 %

Treasury - 0% at TGE, a 9-month cliff (270 days), then 4% per month:
  TGE (day 1) ····· 270-day cliff ····· day 271     +30d    +60d   …   day 991
      0 %                                    +4%  ─▶  +4%  ─▶  +4%  … +4% (topped up)
  total:   0 % ······························     4 %      8 %    12 %  …    100 %
```

### 5.5 Claiming

- **VE-12.** A claim is triggered by **the activity's recipient or by the Operator**
  (the automation account, assigned by Owner - VE-18); whoever calls it, the
  unlocked tokens always go **to the activity's recipient only**. One call releases
  the whole unlocked but not yet released amount (cumulatively): what was not drawn
  down in past periods is released together with the current one in a single call.
- **VE-13.** If the vault's balance is insufficient to cover the unlocked amount,
  the claim reverts entirely (there is no partial payout) until the vault is topped
  up. For example: an emission amount of 1000 with a monthly unlock of 10% (100 at a
  time) and a balance of 500 - the claim passes while the accumulated unlocked
  amount is ≤ 500 (the first five unlocks) and reverts thereafter until the vault is
  topped up.
- **VE-14.** **After TGE arrives** what remains unclaimed neither expires nor is
  revoked: the unlocked tokens stay available to the recipient without a time limit,
  there is no revoke of the unlocked remainder, and an activity's emission amount
  cannot be withdrawn to the project - unlike the sale's unclaimed funds (TS-42).
  Before TGE arrives VE-10 applies: Owner cancels a mistaken activity and its
  vault's whole remainder is returned to the project. The only withdrawal to the
  project after TGE is the overflow beyond the emission amount when a fully
  exhausted activity is closed (VE-17).
- **VE-15.** A claim is repeatable and available until the recipient has taken the
  whole emission amount; the claim has no deadline.
- **VE-16.** The amount available to claim (including what was not drawn down
  earlier) can be read at any time by any observer (the landing page, an indexer) -
  it is a public quantity computed from the activity's state and the shared TGE; no
  separate transaction or signature is required to read it.

### 5.6 Closing

- **VE-17.** Owner closes an activity that has fully run its course (the recipient
  has taken the whole emission amount): the vault and the record are closed and the
  rent is returned. Whatever the vault holds at that moment is an overflow beyond
  the emission amount (an accidental third-party transfer); it is returned to the
  project, and if the vault is empty the transfer is skipped. The recipient cannot
  claim the overflow - the unlock is bounded by the emission amount. While the
  recipient has not drawn the emission amount down in full, the activity cannot be
  closed and its funds cannot be taken.

### 5.7 Roles and ownership

- **VE-18.** Vesting uses the roles **Owner** (the project multisig), **Admin** (the
  project multisig), **Operator** (the automation account) and an activity's
  **recipient**. Owner holds the roles and everything that moves tokens and rent:
  creating, cancelling and closing an activity. Admin holds the configuration: the
  TGE date (before it arrives), an activity's recipient (at any point in its life,
  VE-9) and the project address that receives the remainder and the rent on a
  cancellation or a close - Admin changes that address in one step, at any time.
  Admin moves no funds and neither creates nor closes accounts, but it does set both
  destination addresses - the one the remainder not yet drawn down will go to and
  the one the project's funds will come back to - which is why the role belongs to a
  multisig. Withdrawing the remainder and the rent requires both roles: Admin sets
  the destination address and Owner signs the cancellation or the close. The
  Operator can only trigger a claim - it does not dispose of the funds and cannot
  redirect them (the tokens go to the recipient). The three keys must be pairwise
  distinct. There is no pause - blocking a recipient's claim is unacceptable. The
  Owner change is two-step (G-3): ownership moves only after the candidate confirms
  it with a signature, and until then the nomination can be replaced or withdrawn.
  Admin and Operator are assigned by Owner in one step - a mistaken assignment is
  fixed by Owner itself, and those roles dispose of no funds.
- **VE-19.** The two payout addresses - an activity's recipient and the project
  address that receives the remainder and the rent - are ordinary wallets with a
  private key of their own. Program wallets (a multisig vault, a custody service, a
  smart wallet) are not supported as payout addresses: assigning such an address is
  rejected by the program, on creation and on every later change alike. The roles
  are not restricted this way - Owner and Admin are multisigs and sign through their
  program. An ordinary address that is mistyped cannot be told apart by the program:
  checking the recipients against the list of known project addresses and proving
  control over each of them before the unlock starts stays an operational duty.
