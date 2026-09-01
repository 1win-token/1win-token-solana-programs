//! `close_deposits` / `close_aggregates` - the Operator returns rent to users in
//! batches.

use anchor_lang::prelude::*;
use anchor_lang::system_program::ID as SYSTEM_PROGRAM_ID;

use crate::errors::SaleError;
use crate::events::AccountsClosed;
use crate::seeds::{AGGREGATE_SEED, DEPOSIT_SEED, SALE_STATE_SEED};
use crate::state::{Deposit, SaleState, WalletAggregate, MAX_CLOSE_PER_BATCH};

#[derive(Accounts)]
#[event_cpi]
pub struct CloseDeposits<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = operator @ SaleError::Unauthorized,
    )]
    pub sale_state: Box<Account<'info, SaleState>>,

    pub operator: Signer<'info>,
}

#[derive(Accounts)]
#[event_cpi]
pub struct CloseAggregates<'info> {
    #[account(
        seeds = [SALE_STATE_SEED],
        bump,
        has_one = operator @ SaleError::Unauthorized,
    )]
    pub sale_state: Box<Account<'info, SaleState>>,

    pub operator: Signer<'info>,
}

trait ClosablePda {
    fn owner_wallet(&self) -> Pubkey;
    /// The record's canonical address, derived from its own body.
    fn derive_pda(&self, program_id: &Pubkey) -> Pubkey;
}

impl ClosablePda for Deposit {
    fn owner_wallet(&self) -> Pubkey {
        self.wallet
    }
    fn derive_pda(&self, program_id: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[
                DEPOSIT_SEED,
                self.wallet.as_ref(),
                self.index.to_le_bytes().as_ref(),
            ],
            program_id,
        )
        .0
    }
}

impl ClosablePda for WalletAggregate {
    fn owner_wallet(&self) -> Pubkey {
        self.wallet
    }
    fn derive_pda(&self, program_id: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[AGGREGATE_SEED, self.wallet.as_ref()], program_id).0
    }
}

/// Validates and manually closes each `[account, wallet]` pair.
fn close_batch<'info, T>(
    program_id: &Pubkey,
    accounts: &'info [AccountInfo<'info>],
) -> Result<(u32, u64)>
where
    T: AccountSerialize
        + AccountDeserialize
        + anchor_lang::Discriminator
        + Owner
        + Clone
        + ClosablePda,
{
    require!(
        accounts.len().is_multiple_of(2),
        SaleError::InvalidAccountInBatch
    );
    let pairs = accounts.len() / 2;
    require!(pairs <= MAX_CLOSE_PER_BATCH, SaleError::BatchTooLarge);

    let mut count: u32 = 0;
    let mut rent_returned: u64 = 0;

    for chunk in accounts.chunks(2) {
        let closable = &chunk[0];
        let wallet = &chunk[1];

        require_keys_eq!(
            *closable.owner,
            *program_id,
            SaleError::InvalidAccountInBatch
        );
        require!(!closable.data_is_empty(), SaleError::InvalidAccountInBatch);
        {
            let parsed = Account::<T>::try_from(closable)
                .map_err(|_| error!(SaleError::InvalidAccountInBatch))?;
            // (3) the canonical address is derived from the body (wallet[/index]) -
            //     this rejects a forgery carrying a valid discriminator but a body
            //     that does not belong to its address.
            let expected = parsed.derive_pda(program_id);
            require_keys_eq!(*closable.key, expected, SaleError::InvalidAccountInBatch);
            // (5) the rent goes strictly to the record's owner, never to a wallet
            //     taken from untrusted input.
            require_keys_eq!(
                *wallet.key,
                parsed.owner_wallet(),
                SaleError::InvalidAccountInBatch
            );
        }

        let lamports = closable.lamports();
        require!(lamports > 0, SaleError::InvalidAccountInBatch);
        **wallet.try_borrow_mut_lamports()? = wallet
            .lamports()
            .checked_add(lamports)
            .ok_or(SaleError::MathOverflow)?;
        **closable.try_borrow_mut_lamports()? = 0;
        closable.assign(&SYSTEM_PROGRAM_ID);
        closable.resize(0)?;

        rent_returned = rent_returned
            .checked_add(lamports)
            .ok_or(SaleError::MathOverflow)?;
        count = count.checked_add(1).ok_or(SaleError::MathOverflow)?;
    }

    Ok((count, rent_returned))
}

pub fn close_deposits(ctx: Context<CloseDeposits>) -> Result<()> {
    ctx.accounts.sale_state.require_claim_closed()?;
    let (count, rent_returned) = close_batch::<Deposit>(ctx.program_id, ctx.remaining_accounts)?;
    emit_cpi!(AccountsClosed {
        count,
        rent_returned,
    });
    Ok(())
}

pub fn close_aggregates(ctx: Context<CloseAggregates>) -> Result<()> {
    ctx.accounts.sale_state.require_claim_closed()?;
    let (count, rent_returned) =
        close_batch::<WalletAggregate>(ctx.program_id, ctx.remaining_accounts)?;
    emit_cpi!(AccountsClosed {
        count,
        rent_returned,
    });
    Ok(())
}
