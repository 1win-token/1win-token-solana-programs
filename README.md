# $1WIN Solana Programs

Anchor smart contracts for the **$1WIN** token on Solana. The token itself is a
plain SPL mint; these programs custody user and treasury funds in program-derived
vaults.

## Programs

### Sale (`programs/sale`)

A fixed-price USDC raise with oversubscription handling and self-serve
claim/refund.

During the sale window buyers deposit USDC into a program-owned vault at a fixed
price; each deposit gets its own on-chain record alongside a per-wallet aggregate.
Compliance decisions are made off-chain and enforced on-chain through a Blacklist
(blocks future purchases) and an optional per-wallet Graylist (custom limits).
After the sale ends, a settlement window allows risky deposits to be refunded
before an admin-only `activate_claim` permanently fixes the pool. If the raise
exceeds the soft cap, allocation and refunds are pro-rated; otherwise every buyer
receives tokens at the fixed price. A single `claim` instruction pays out tokens
and any USDC refund and closes the buyer's aggregate; after the claim deadline
unclaimed funds sweep to the owner and leftover records are batch-closed.

Roles: **Owner** (multisig - initialization, withdrawals, role rotation), **Admin**
(ops multisig - sale parameters, schedule, pause, `activate_claim`, Graylist),
**Operator** (automation - Blacklist, refunds, batch closes).

Phases, derived from time and on-chain flags and one-way:
`PreSale → Sale → SaleEnded → Claim → ClaimClosed`.

### Vesting (`programs/vesting`)

Gradual unlock of the $1WIN allocated to the non-sale emission buckets, gated by a
single shared Token Generation Event (TGE).

Each bucket is an independent **activity** with its own recipient, reference
amount, schedule and isolated token vault; all activities share one `tge_ts`.
Before TGE the owner can create activities, assign recipients and cancel unfunded
ones. Once `now >= tge_ts` the TGE timestamp and every activity's schedule and
existence are permanently frozen. Unlock is a pure function of time: an optional
upfront TGE percentage, then a monthly percentage released every 30-day period,
with an optional cliff delaying the first monthly tranche; the final tranche rounds
up to the exact reference amount. `claim` can be triggered by the recipient or by
an operator, but funds always move to the recipient's own token account. There is
no pause and no revoke - once vested, funds are permanently claimable.

Roles: **Owner** (multisig - config, activity lifecycle, role rotation), **Admin**
(TGE date, activity recipient, withdrawal target), **Operator** (triggers `claim`
on behalf of recipients), **Recipient** (claims its own activity).

### Burn (`programs/burn`)

Rate-limited burning of $1WIN out of the program's vault.

The Operator burns tokens under a daily and a weekly limit tracked per UTC period;
the period windows reset lazily on the first burn of a new period, an excess is
rejected rather than clamped, and the burned totals are kept in a dedicated
accumulator rather than derived from balances. The vault is owned by a
program-derived authority: the platform transfers the tokens to burn into it, the
Operator triggers the burn, and tokens leave the vault only through `burn`, within
the limits - no key can burn or transfer them directly.

Roles: **Owner** (role rotation, two-step handover), **Admin** (limits),
**Operator** (executes the burn).

## Repository layout

```
programs/
  sale/         Sale program (Anchor)
  vesting/      Vesting program (Anchor)
  burn/         Burn program (Anchor)
tests/
  sale/             Sale + Burn unit/integration tests (LiteSVM)
  burn/             Burn instruction builders and readers (LiteSVM)
  sale-e2e/         Sale end-to-end tests (Surfpool, #[ignore]-gated)
  vesting/          Vesting unit/integration tests (LiteSVM)
  vesting-e2e/      Vesting end-to-end tests (Surfpool, #[ignore]-gated)
  expected/         Hand-written expected values for Sale
  vesting-expected/ Hand-written expected values for Vesting
docs/           Specifications: requirements, architecture, use cases
```

## Prerequisites

- Rust `1.96.0` (pinned in `rust-toolchain.toml`, installed automatically by
  `rustup` on first build)
- [Anchor](https://www.anchor-lang.com/) `1.0.2`
- Solana CLI `3.1.10`
- [Surfpool](https://github.com/txtx/surfpool) for the end-to-end tests

Check your local versions against the pinned ones with `make tools`.

## Building and testing

```bash
make build          # build every program
make build-sale     # build Sale only
make build-vesting  # build Vesting only

make math           # program-crate unit tests, no .so build (fastest feedback loop)
make test           # build + the full workspace test suite
make test-sale      # build Sale and Burn + run their tests in isolation
make test-vesting   # build Vesting + run its tests

make e2e            # Sale end-to-end tests against Surfpool (ports 8905-8910)
make e2e-vesting    # Vesting end-to-end tests against Surfpool (ports 8911-8915)

make fmt / fmt-check   # cargo fmt / --check
make lint              # cargo clippy --workspace --all-targets -- -D warnings
make id-check          # declare_id!() in each program must match Anchor.toml

make check             # fmt-check + lint + id-check + build + test - run before pushing
```

`make help` lists every target with a short description.
