use anchor_lang::prelude::*;
use anchor_spl::token::TokenAccount;

use crate::errors::SaleError;
use crate::seeds::SALE_STATE_SEED;
use crate::state::SaleState;

#[derive(Accounts)]
pub struct AssertFunded<'info> {
    #[account(seeds = [SALE_STATE_SEED], bump)]
    pub sale_state: Account<'info, SaleState>,

    #[account(address = sale_state.token_vault)]
    pub token_vault: Account<'info, TokenAccount>,
}

pub fn assert_funded(ctx: Context<AssertFunded>) -> Result<()> {
    require!(
        ctx.accounts.token_vault.amount >= ctx.accounts.sale_state.emission,
        SaleError::VaultNotFunded
    );
    Ok(())
}
