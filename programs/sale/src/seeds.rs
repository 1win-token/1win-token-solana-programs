//! The program's PDA seed bytes. A single place for the whole program - used
//! both by `seeds = [...]` and by the runtime PDA signature: a typo would have
//! made the address and the signature disagree.

pub(crate) const SALE_STATE_SEED: &[u8] = b"sale_state";
pub(crate) const VAULT_AUTHORITY_SEED: &[u8] = b"vault_authority";
pub(crate) const DEPOSIT_VAULT_SEED: &[u8] = b"deposit_vault";
pub(crate) const TOKEN_VAULT_SEED: &[u8] = b"token_vault";
pub(crate) const PROCEEDS_VAULT_SEED: &[u8] = b"proceeds_vault";
pub(crate) const REFUND_VAULT_SEED: &[u8] = b"refund_vault";
pub(crate) const AGGREGATE_SEED: &[u8] = b"aggregate";
pub(crate) const DEPOSIT_SEED: &[u8] = b"deposit";
pub(crate) const REFUND_ACCOUNT_SEED: &[u8] = b"refund_account";
pub(crate) const BLACKLIST_SEED: &[u8] = b"blacklist";
pub(crate) const GRAYLIST_SEED: &[u8] = b"graylist";
