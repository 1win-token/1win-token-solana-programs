//! `burn` - the Operator burns tokens out of the vault, checking the daily and
//! weekly limits of the current UTC period (§6.2, §6.3).

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount};

use crate::errors::BurnError;
use crate::events::BurnExecuted;
use crate::period::{accumulate, period_indices};
use crate::seeds::{BURN_COUNTERS_SEED, BURN_STATE_SEED, VAULT_AUTHORITY_SEED, VAULT_SEED};
use crate::state::{BurnCounters, BurnState, PeriodWindow};

#[derive(Accounts)]
#[event_cpi]
pub struct Burn<'info> {
    #[account(
        seeds = [BURN_STATE_SEED],
        bump,
        has_one = operator @ BurnError::Unauthorized,
    )]
    pub burn_state: Account<'info, BurnState>,

    /// Operator (automation hot key) - executes the burn; the role is checked by
    /// has_one. Admin (ops multisig) cannot burn - the role permission sets are
    /// disjoint (BRD G-1, BR-7).
    pub operator: Signer<'info>,

    #[account(
        mut,
        seeds = [BURN_COUNTERS_SEED],
        bump,
    )]
    pub burn_counters: Account<'info, BurnCounters>,

    /// The vault the tokens are burned from (§6.2).
    #[account(mut, seeds = [VAULT_SEED], bump)]
    pub vault: Account<'info, TokenAccount>,

    /// CHECK: the data-less PDA that owns the vault and signs the burn CPI; the
    /// address comes from the seeds, the bump is recomputed.
    #[account(seeds = [VAULT_AUTHORITY_SEED], bump)]
    pub vault_authority: UncheckedAccount<'info>,

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

    // 5. CPI: the program signs as the vault's authority. An insufficient vault
    //    balance is an SPL error; we do not duplicate that pre-check -
    //    bookkeeping runs on our own counters, not on the balance.
    let signer_seeds: &[&[&[u8]]] = &[&[VAULT_AUTHORITY_SEED, &[ctx.bumps.vault_authority]]];
    token::burn(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            token::Burn {
                mint: ctx.accounts.token_mint.to_account_info(),
                from: ctx.accounts.vault.to_account_info(),
                authority: ctx.accounts.vault_authority.to_account_info(),
            },
            signer_seeds,
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
