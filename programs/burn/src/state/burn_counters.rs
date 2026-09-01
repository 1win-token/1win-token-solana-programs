//! The `BurnCounters` singleton accumulator - the burned-total windows for the
//! current UTC day and UTC week, seeds `[b"burn_counters"]`.

use anchor_lang::prelude::*;

/// A single period window: how much was burned (`burned`) during the period
/// with UTC index `index`. A window holding a foreign (past) index is reset
/// lazily on the next `burn` (see `period::accumulate`).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace)]
pub struct PeriodWindow {
    pub index: u64,
    pub burned: u64,
}

/// The period-limit accumulator (§6.3). A singleton with seeds
/// `[b"burn_counters"]`. Never closed. Created in `initialize` next to
/// `BurnState`; `burn` passes it separately so that the config stays non-mut.
/// The bump is not cached: `burn` re-derives the address
/// (`seeds = [b"burn_counters"], bump`).
#[account]
#[derive(InitSpace)]
pub struct BurnCounters {
    pub day: PeriodWindow,
    pub week: PeriodWindow,
}
