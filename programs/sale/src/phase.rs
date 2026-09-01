//! Lazy phase machine: the phase is NOT stored - it is derived from time and
//! flags on every instruction. Instructions assert the phase via [current_phase].

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// purchases are closed, parameters can still be edited
    PreSale,
    /// purchases are open - USDC is accepted (unless paused)
    Sale,
    /// the pool can be fixed
    SaleEnded,
    /// tokens are handed out and USDC is refunded
    Claim,
    /// sweep, close_deposits and close_aggregates are available
    ClaimClosed,
}

#[derive(Debug, Clone, Copy)]
pub struct PhaseSchedule {
    pub sale_start_date: i64,
    pub sale_end_date: i64,
    pub pool_fixed: bool,
    pub claim_start_date: i64,
    pub claim_duration: i64,
}

pub fn current_phase(now: i64, schedule: &PhaseSchedule) -> Phase {
    let claim_deadline = schedule
        .claim_start_date
        .saturating_add(schedule.claim_duration);
    match schedule.pool_fixed {
        true if now >= claim_deadline => Phase::ClaimClosed,
        true if now >= schedule.claim_start_date => Phase::Claim,
        true => Phase::SaleEnded,
        false if now < schedule.sale_start_date => Phase::PreSale,
        false if now < schedule.sale_end_date => Phase::Sale,
        false => Phase::SaleEnded,
    }
}

#[cfg(test)]
mod tests;
