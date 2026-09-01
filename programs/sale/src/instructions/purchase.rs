//! Purchasing: `init_aggregate` (creates the wallet aggregate) and `purchase`
//! (accepts USDC into a contribution).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::pubkey::bytes_are_curve_point;
use anchor_spl::token::{self, Token, TokenAccount, Transfer};

use crate::errors::SaleError;
use crate::events::{AggregateInitialized, Purchased};
use crate::phase::Phase;
use crate::seeds::{AGGREGATE_SEED, BLACKLIST_SEED, DEPOSIT_SEED, GRAYLIST_SEED, SALE_STATE_SEED};
use crate::state::{Deposit, Graylist, SaleState, WalletAggregate};

#[derive(Accounts)]
#[event_cpi]
pub struct InitAggregate<'info> {
    #[account(
        mut,
        seeds = [SALE_STATE_SEED],
        bump,
    )]
    pub sale_state: Account<'info, SaleState>,

    /// The buyer. The address must lie on the Ed25519 curve, meaning it has a
    /// private key of its own. A PDA (multisig, custody) lies off the curve and
    /// cannot be a buyer.
    #[account(
        mut,
        constraint = bytes_are_curve_point(user.key()) @ SaleError::WalletNotOnCurve,
    )]
    pub user: Signer<'info>,

    #[account(
        init,
        seeds = [AGGREGATE_SEED, user.key().as_ref()],
        bump,
        payer = user,
        space = 8 + WalletAggregate::INIT_SPACE,
    )]
    pub aggregate: Account<'info, WalletAggregate>,

    /// CHECK: the blacklist PDA slot; the owner is checked in the body, the
    /// address comes from the seeds.
    #[account(seeds = [BLACKLIST_SEED, user.key().as_ref()], bump)]
    pub blacklist: UncheckedAccount<'info>,

    pub system_program: Program<'info, System>,
}

pub fn init_aggregate(ctx: Context<InitAggregate>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let state = &mut ctx.accounts.sale_state;

    require!(state.phase(now) == Phase::Sale, SaleError::WrongPhase);
    require!(!state.paused, SaleError::SalePaused);

    let wallet = ctx.accounts.user.key();

    let blacklist_acc = &ctx.accounts.blacklist;
    require_keys_neq!(
        *blacklist_acc.owner,
        crate::ID,
        SaleError::WalletBlacklisted
    );

    // +1 per aggregate, an over-estimate of the reserve - overstating it only
    // grows refund_vault, so solvency still holds.
    state.participants = state
        .participants
        .checked_add(1)
        .ok_or(SaleError::MathOverflow)?;

    let aggregate = &mut ctx.accounts.aggregate;
    aggregate.wallet = wallet;
    aggregate.net_usdc_contributed = 0;
    aggregate.deposit_count = 0;

    emit_cpi!(AggregateInitialized { wallet });
    Ok(())
}

#[derive(Accounts)]
#[event_cpi]
pub struct Purchase<'info> {
    #[account(
        mut,
        seeds = [SALE_STATE_SEED],
        bump,
    )]
    pub sale_state: Account<'info, SaleState>,

    /// The buyer. Same requirement as when the aggregate is created: an address
    /// on the Ed25519 curve, that is, one with a private key of its own.
    #[account(
        mut,
        constraint = bytes_are_curve_point(user.key()) @ SaleError::WalletNotOnCurve,
    )]
    pub user: Signer<'info>,

    #[account(
        mut,
        seeds = [AGGREGATE_SEED, user.key().as_ref()],
        bump,
    )]
    pub aggregate: Account<'info, WalletAggregate>,

    #[account(
        init,
        seeds = [DEPOSIT_SEED, user.key().as_ref(), aggregate.deposit_count.to_le_bytes().as_ref()],
        bump,
        payer = user,
        space = 8 + Deposit::INIT_SPACE,
    )]
    pub deposit: Account<'info, Deposit>,

    /// CHECK: the blacklist PDA slot; the owner is checked in the body, the
    /// address comes from the seeds.
    #[account(seeds = [BLACKLIST_SEED, user.key().as_ref()], bump)]
    pub blacklist: UncheckedAccount<'info>,

    /// CHECK: the graylist PDA slot; the owner is checked in the body, the
    /// address comes from the seeds.
    #[account(seeds = [GRAYLIST_SEED, user.key().as_ref()], bump)]
    pub graylist: UncheckedAccount<'info>,

    #[account(
        mut,
        token::mint = sale_state.usdc_mint,
        token::authority = user,
    )]
    pub user_usdc_ata: Account<'info, TokenAccount>,

    #[account(mut, address = sale_state.deposit_vault)]
    pub deposit_vault: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

pub fn purchase(ctx: Context<Purchase>, amount: u64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let state = &ctx.accounts.sale_state;

    require!(state.phase(now) == Phase::Sale, SaleError::WrongPhase);
    require!(!state.paused, SaleError::SalePaused);
    require!(!state.pool_fixed, SaleError::PoolAlreadyFixed);

    let wallet = ctx.accounts.user.key();

    let blacklist_acc = &ctx.accounts.blacklist;
    require_keys_neq!(
        *blacklist_acc.owner,
        crate::ID,
        SaleError::WalletBlacklisted
    );

    let net = ctx.accounts.aggregate.net_usdc_contributed;
    let graylist_acc = &ctx.accounts.graylist;
    let (min_per_tx, max_per_tx) = if *graylist_acc.owner == crate::ID {
        let data = graylist_acc.try_borrow_data()?;
        let entry = Graylist::try_deserialize(&mut &data[..])?;
        let new_net = net.checked_add(amount).ok_or(SaleError::MathOverflow)?;
        require!(new_net <= entry.max_total, SaleError::WalletTotalExceeded);
        (entry.min_per_tx, entry.max_per_tx)
    } else {
        (state.min_per_tx, state.max_per_tx)
    };
    require!(amount >= min_per_tx, SaleError::BelowMin);
    require!(amount <= max_per_tx, SaleError::AboveMax);

    token::transfer(
        CpiContext::new(
            ctx.accounts.token_program.key(),
            Transfer {
                from: ctx.accounts.user_usdc_ata.to_account_info(),
                to: ctx.accounts.deposit_vault.to_account_info(),
                authority: ctx.accounts.user.to_account_info(),
            },
        ),
        amount,
    )?;

    let index = ctx.accounts.aggregate.deposit_count;

    let deposit = &mut ctx.accounts.deposit;
    deposit.wallet = wallet;
    deposit.amount = amount;
    deposit.index = index;

    let aggregate = &mut ctx.accounts.aggregate;
    aggregate.net_usdc_contributed = net.checked_add(amount).ok_or(SaleError::MathOverflow)?;
    aggregate.deposit_count = index.checked_add(1).ok_or(SaleError::MathOverflow)?;
    let new_net = aggregate.net_usdc_contributed;

    let state = &mut ctx.accounts.sale_state;
    state.deposits_usdc_contributed = state
        .deposits_usdc_contributed
        .checked_add(amount)
        .ok_or(SaleError::MathOverflow)?;
    let new_total = state.deposits_usdc_contributed;

    emit_cpi!(Purchased {
        wallet,
        index,
        amount,
        net: new_net,
        total: new_total,
    });
    Ok(())
}
