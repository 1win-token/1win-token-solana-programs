# Claude Code tooling stack — 1WIN Solana programs

This project ships a curated Claude Code stack (committed, so the whole team gets it).
It is scoped to **on-chain contract development** for a system that custodies real USDC.
The automated security tools below catch common issues early — treat them as a first-pass
filter, not a guarantee of correctness.

## What's installed (active now)

| Component | Type | Scope | Role served | Why |
|---|---|---|---|---|
| `solana-mcp` (https://mcp.solana.com/mcp) | MCP (remote) | `.mcp.json` | Architect, Dev, Security | Official, no API key, zero supply-chain risk. Docs/semantic search, `Solana_Expert__Ask_For_Help`, and `program_autofixer` (Anchor security analysis + fixes) |
| `solana-dev` skill (Solana Foundation) | Skill | installed per developer, not vendored | Architect, Dev, Tester | Official Anchor/SPL/Token-2022 playbook, LiteSVM/Mollusk/Surfpool testing, security checklists |
| `solana-architect` | Subagent | `.claude/agents/` | Architect | Account/PDA design, phase machine, authority model, oversubscription math |
| `solana-developer` | Subagent | `.claude/agents/` | Developer | Implements Anchor programs to spec; runs `program_autofixer` |
| `solana-tester` | Subagent | `.claude/agents/` | Tester | LiteSVM/Mollusk + Surfpool/devnet, adversarial coverage |
| `solana-security-auditor` | Subagent | `.claude/agents/` | Security | Solana/Anchor vuln-class review before anything ships |

### Invoking the agents
Ask Claude e.g. *"use the solana-architect to review the PDA layout"*, or let Claude
route automatically based on the task. The `solana-dev` skill is not committed here -
install it per developer (`claude plugin install solana-dev@claude-plugins-official`);
it then triggers on Solana tasks.

> First run: the project MCP server (`solana-mcp`) requires a one-time trust approval.
> Run `claude mcp list` or open `/mcp` and approve it.

## Phase 2 — activate when the work reaches that stage

### solana-fender (static security analyzer) — activate when the first Anchor code lands
Community tool (engine by Honey Guard, MCP wrapper by `Cian`). Anchor-specific static
analysis: re-entrancy, integer overflow, missing account checks, seeds anti-patterns.
Runs locally/offline (good — no code leaves the machine). Toolchain (`cargo`/`anchor`)
is already present.

```bash
# Verify exact crate/binary names against the upstream repos before trusting the binary:
#   engine:  https://github.com/HoneyGuard/solana-fender   (or honey-guard org)
#   wrapper: anchor-mcp by Cian
cargo install solana_fender
cargo install anchor-mcp
# then register as a project MCP (confirm the run command from the wrapper's README):
claude mcp add --scope project solana-fender -- anchor-mcp
```
The `solana-security-auditor` agent already knows to use it once present.

### Helius MCP (devnet RPC / tx parsing) — activate when integration/devnet testing starts
Needs a paid Helius API key (from ~$1). Official Claude Code plugin available, but it
also bundles frontend/trading skills (Phantom/Jupiter/DFlow/OKX) that are **not relevant
to this contracts repo** — prefer the standalone MCP to stay lean.

```bash
# standalone MCP (leaner — recommended for this repo):
claude mcp add --transport http --scope local helius "https://mcp.helius.dev/mcp" \
  --header "Authorization: Bearer $HELIUS_API_KEY"
# OR the full official plugin (brings extra skills you mostly won't need here):
#   claude plugin install helius@claude-plugins-official
```
Scope is `local` (not committed) because it carries a secret. **Never give Helius — or any
outbound tool — access to keys controlling real funds.** Use throwaway devnet keys.

## Deliberately NOT installed (and why)
- **`solana-foundation/solana-dev-mcp`** (GitHub demo): self-described "not for production",
  3 basic RPC methods. Superseded by `solana-mcp` + Helius.
- **`claudskills/solana-development`** skill: community duplicate of the official `solana-dev`
  skill. Two overlapping skills cause context bloat and conflicting guidance.
- **`solanabr/solana-claude`** framework: good reference for role-based agents, but community
  (~65★) and installs a whole `.claude/` via `curl | bash`. For a funds project we adopted the
  *idea* (the 4 agents above) rather than running its installer wholesale.
