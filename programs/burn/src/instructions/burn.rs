//! `burn` - the Operator burns tokens out of the source token account, checking
//! the daily and weekly limits of the current UTC period (§6.2, §6.3).

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount};

use crate::errors::BurnError;
use crate::events::BurnExecuted;
use crate::period::{accumulate, period_indices};
use crate::state::{BurnCounters, BurnState, PeriodWindow};

#[derive(Accounts)]
#[event_cpi]
pub struct Burn<'info> {
    #[account(
        seeds = [b"burn_state"],
        bump,
        has_one = operator @ BurnError::Unauthorized,
    )]
    pub burn_state: Account<'info, BurnState>,

    /// Operator (automation hot key) - executes the burn and is the SPL
    /// authority of the source account: the role is checked by has_one, while
    /// the right to debit is enforced by SPL Token itself during the CPI.
    /// Admin (ops multisig) cannot burn - the role permission sets are disjoint
    /// (BRD G-1, BR-7).
    pub operator: Signer<'info>,

    #[account(
        mut,
        seeds = [b"burn_counters"],
        bump,
    )]
    pub burn_counters: Account<'info, BurnCounters>,

    /// The source of the tokens being burned (§6.2); ownership is verified by
    /// SPL Token itself during the burn CPI.
    #[account(mut, token::mint = burn_state.token_mint)]
    pub source: Account<'info, TokenAccount>,

    #[account(mut, address = burn_state.token_mint @ BurnError::InvalidMint)]
    pub token_mint: Account<'info, Mint>,

    pub token_program: Program<'info, Token>,
}

pub fn burn(ctx: Context<Burn>, amount: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(now >= 0, BurnError::InvalidTimestamp);

    // 1. The current UTC day and week indices come from Clock only (§6.3):
    //    burning into a foreign period is impossible by construction (there are
    //    no index arguments any more).
    let (cur_day, cur_week) = period_indices(now)?;

    // 2. A zero burn is not performed (BR-6). The amount is the only argument:
    //    an input that does not affect state is of no use to the instruction.
    require!(amount > 0, BurnError::ZeroBurnAmount);

    // 3. Period limits: an excess is rejected, not clamped (BR-5). accumulate
    //    is pure (it does not mutate the windows): a window holding a foreign -
    //    that is, past - index is counted from zero (lazy reset).
    let day_total = accumulate(
        &ctx.accounts.burn_counters.day,
        cur_day,
        amount,
        ctx.accounts.burn_state.daily_limit,
        BurnError::DailyLimitExceeded,
    )?;
    let week_total = accumulate(
        &ctx.accounts.burn_counters.week,
        cur_week,
        amount,
        ctx.accounts.burn_state.weekly_limit,
        BurnError::WeeklyLimitExceeded,
    )?;

    // 4. Both windows are mutated after both limit checks (a partial mutation
    //    is impossible) and before the CPI. Rewriting index within the same
    //    period is idempotent.
    let counters = &mut ctx.accounts.burn_counters;
    counters.day = PeriodWindow {
        index: cur_day,
        burned: day_total,
    };
    counters.week = PeriodWindow {
        index: cur_week,
        burned: week_total,
    };

    // 5. CPI: SPL Token itself requires the source owner's signature - a source
    //    the operator neither owns nor is delegated on is rejected. An
    //    insufficient source balance is likewise an SPL error; we do not
    //    duplicate that pre-check - bookkeeping runs on our own counters, not
    //    on the balance.
    token::burn(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            token::Burn {
                mint: ctx.accounts.token_mint.to_account_info(),
                from: ctx.accounts.source.to_account_info(),
                authority: ctx.accounts.operator.to_account_info(),
            },
        ),
        amount,
    )?;

    emit_cpi!(BurnExecuted {
        day_index: cur_day,
        week_index: cur_week,
        amount,
        day_total,
        week_total,
    });
    Ok(())
}
