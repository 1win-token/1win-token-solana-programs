# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project context

Solana/Anchor smart contracts for the $1WIN token: token sale, vesting, and burn. The token is a plain SPL. User funds sit in vaults under PDAs — the security bar is high, and it is held not by the final review but by code rules applied to every change.

## Repository state

The repo holds three Anchor programs (`programs/sale`, `programs/burn`, `programs/vesting`), their LiteSVM and Surfpool test crates (`tests/`), the spec (`docs/`) and the curated Claude Code stack (`.claude/`). Build, lint and test commands live in the `Makefile` — `make check` runs the full gate (fmt-check, clippy, program-id check, build, tests).

## Canonical spec — `docs/`

Read before writing or reviewing program code. These documents are the source of truth for mechanics, account layout, and phase machine; the on-chain code must match them, not the other way around.

- `1WIN_Programs_Business_Requirements.md` — BRD: product mechanics, sale phases, oversubscription rules, claim/refund flows.
- `1WIN_Programs_Technical_Requirements.md` — TRD: instructions, accounts, authorities, math, error model.
- `TokenSale_Program_Architecture.md` — PDA layout, state machine, authority/multisig model, program isolation, upgrade & rent strategy.
- `TokenSale_Use_Cases.md` — happy-path and adversarial scenarios that the implementation and tests must cover.
- `Vesting_Program_Architecture.md` — Vesting program: TGE gate, per-activity PDAs and vaults, unlock math, authority model.

## Why `.claude/rules/`

The rules in `.claude/rules/` distill lessons from a previous implementation into general Solana/Anchor coding principles. They are written as principles applicable to arbitrary program code, not to the current mechanics — so new features hold the same security bar by the same rules.

**Before writing or reviewing program code — read every file in `.claude/rules/`.** They auto-load via `paths`-frontmatter on matching files, but conscious reading up front is still required.

## Code principles (`.claude/rules/`)

The cross-cutting principle behind every rule: **structural enforcement beats runtime checks.** An invariant expressible through PDA seeds or an Anchor type must not live in a `require!` — by-construction protection survives refactoring; a runtime check is easy to lose.

Rules (mono-thematic, one file per concern):

**Security / correctness**
- `accounts-untrusted.md` — every account is untrusted until authenticity is proven structurally (types, seeds, owner, vault binding).
- `input-validation.md` — instruction arguments are validated at the entry; `String`/`Vec` length in bytes, not characters; numeric ranges.
- `init-and-reinit.md` — `init` vs `init_if_needed`; singleton initialized once; init authority bound to an on-chain fact.
- `privileged-roles.md` — authorization and role lifecycle; PDA-presence = grant, revoke = close; two-step owner handover.
- `state-machine-and-time.md` — every instruction asserts the phase; transitions are one-way; time from `Clock`.
- `fail-closed.md` — security code reverts, it does not stay silent; "unset" ≠ zero; check every participant.
- `checked-math.md` — checked arithmetic; `u128` with downcast after divisions; divisor protection; round in favor of the protocol.
- `reload-after-cpi.md` — account data is stale after a CPI; decide on pre-CPI values or after `reload()`.
- `accounting-integrity.md` — bookkeeping by the program's own state, not by the token-account balance; symmetric counters.
- `account-close-lifecycle.md` — on close, subtract the account's amount from counters; do not close with an open obligation; close as a replay guard.
- `no-test-code-in-production.md` — no feature flags or test code in the prod build; mock doubles are separate programs; the contract is tested from the outside.

**Code quality**
- `no-overengineering.md` — simplest construction; fix at init instead of settable-then-locked; no extensibility without a second consumer.
- `minimal-onchain-footprint.md` — on-chain is expensive: store only what the logic reads (including necessary records — one PDA per element); pure display/analytics data goes into events; no derived fields, no growing loops.
- `errors-explicit.md` — one variant per invariant; no `unwrap` / `expect` on fallible paths.

**Testing**
- `tests-bind-to-real-code.md` — a test calls the real instruction and asserts the on-chain effect, it does not duplicate the contract's formula; every guard gets a positive and a negative path.

## Tooling stack — `.claude/TOOLING.md`

The committed Claude Code stack (subagents, MCP servers, skill) is documented in `.claude/TOOLING.md`, including what is deliberately NOT installed and why. Consult it before adding new tools to the repo.

Currently active subagents (`.claude/agents/`):
- `solana-architect` — account/PDA design, phase machine, authority model, oversubscription math.
- `solana-developer` — implements Anchor programs to spec.
- `solana-tester` — LiteSVM/Mollusk + Surfpool/devnet, adversarial coverage.
- `solana-security-auditor` — Solana/Anchor vuln-class review before anything ships.

## `program_autofixer` is mandatory on program code

When writing or modifying on-chain Rust (Anchor or Pinocchio), call the `program_autofixer` tool from `solana-mcp` **before** returning the code. Apply the fixes, call again, loop until `require_another_tool_call_after_fixing` is `false`. The flag stays true only while syntax errors or critical/high issues remain; medium/low and advisory suggestions do not require another pass.

## Review stance

The code is written in-house (including by AI agents), so apply the same strictness to your own code as to third-party code:

- Verify real behavior and the diff, not the claim that it "works" or "is fixed".
- A change closes the entire class of the problem, not only the concrete example.
