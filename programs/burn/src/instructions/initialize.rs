//! `initialize`: creates the `BurnState` and `BurnCounters` singletons and the
//! vault. The right to init is bound to the program's upgrade authority.

use anchor_lang::prelude::*;
use anchor_spl::token::{Mint, Token, TokenAccount};

use crate::errors::BurnError;
use crate::events::Initialized;
use crate::seeds::{BURN_COUNTERS_SEED, BURN_STATE_SEED, VAULT_AUTHORITY_SEED, VAULT_SEED};
use crate::state::{BurnCounters, BurnState, PeriodWindow, TOKEN_DECIMALS};

#[derive(Accounts)]
#[event_cpi]
pub struct Initialize<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(constraint = burn_program.programdata_address()? == Some(program_data.key()) @ BurnError::Unauthorized)]
    pub burn_program: Program<'info, crate::program::Burn>,

    #[account(constraint = program_data.upgrade_authority_address == Some(authority.key()) @ BurnError::Unauthorized)]
    pub program_data: Account<'info, ProgramData>,

    #[account(
        init,
        seeds = [BURN_STATE_SEED],
        bump,
        payer = authority,
        space = 8 + BurnState::INIT_SPACE,
    )]
    pub burn_state: Account<'info, BurnState>,

    #[account(
        init,
        seeds = [BURN_COUNTERS_SEED],
        bump,
        payer = authority,
        space = 8 + BurnCounters::INIT_SPACE,
    )]
    pub burn_counters: Account<'info, BurnCounters>,

    pub token_mint: Account<'info, Mint>,

    /// The vault the burns are made from (§6.2): tokens are transferred into it
    /// and leave it only through `burn`.
    #[account(
        init,
        seeds = [VAULT_SEED],
        bump,
        payer = authority,
        token::mint = token_mint,
        token::authority = vault_authority,
    )]
    pub vault: Account<'info, TokenAccount>,

    /// CHECK: the data-less PDA that owns the vault; the address comes from the
    /// seeds, the bump is recomputed.
    #[account(seeds = [VAULT_AUTHORITY_SEED], bump)]
    pub vault_authority: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn handler(
    ctx: Context<Initialize>,
    owner: Pubkey,
    admin: Pubkey,
    operator: Pubkey,
    daily_limit: u64,
    weekly_limit: u64,
) -> Result<()> {
    require!(
        ctx.accounts.token_mint.decimals == TOKEN_DECIMALS,
        BurnError::InvalidMintDecimals
    );

    require!(daily_limit > 0, BurnError::InvalidParams);
    require!(weekly_limit > 0, BurnError::InvalidParams);

    require_keys_neq!(owner, Pubkey::default(), BurnError::InvalidAuthority);
    require_keys_neq!(admin, Pubkey::default(), BurnError::InvalidAuthority);
    require_keys_neq!(operator, Pubkey::default(), BurnError::InvalidAuthority);

    let state = &mut ctx.accounts.burn_state;
    state.owner = owner;
    state.pending_owner = None;
    state.admin = admin;
    state.operator = operator;
    state.token_mint = ctx.accounts.token_mint.key();
    state.daily_limit = daily_limit;
    state.weekly_limit = weekly_limit;

    // The period windows are zeroed explicitly; "period 0 holding zero" is
    // safe: the first burn of any real period (index > 0) resets the window
    // (accumulate's lazy reset).
    let counters = &mut ctx.accounts.burn_counters;
    counters.day = PeriodWindow {
        index: 0,
        burned: 0,
    };
    counters.week = PeriodWindow {
        index: 0,
        burned: 0,
    };

    emit_cpi!(Initialized {
        owner,
        admin,
        operator,
        token_mint: state.token_mint,
        daily_limit,
        weekly_limit,
    });

    Ok(())
}
