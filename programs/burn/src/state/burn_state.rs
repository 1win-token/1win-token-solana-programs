//! The `BurnState` singleton account and the token decimals constant.

use anchor_lang::prelude::*;

pub const TOKEN_DECIMALS: u8 = 8;

/// Global burn state. A singleton with seeds `[b"burn_state"]`. Never closed.
/// Holds only the limits and the authorities (§8); burned totals are not kept
/// on-chain (BR-4) - the period windows live in the `BurnCounters` accumulator.
/// The bump is not cached: every context re-derives the address
/// (`seeds = [b"burn_state"], bump`), so there is nothing to store.
#[account]
#[derive(InitSpace)]
pub struct BurnState {
    pub owner: Pubkey,
    /// The owner candidate awaiting acceptance: `None` - no handover in flight,
    /// `Some(key)` - nominated, but the role has not moved yet (§9.1).
    pub pending_owner: Option<Pubkey>,
    /// Admin (ops multisig) - program parameters (`set_limits`).
    pub admin: Pubkey,
    /// Operator (automation hot key) - executes the burn (`burn`). A role only:
    /// the key holds no token-level right over the vault.
    pub operator: Pubkey,
    pub token_mint: Pubkey,
    pub daily_limit: u64,
    pub weekly_limit: u64,
}
