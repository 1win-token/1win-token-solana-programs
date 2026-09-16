//! The program's PDA seed bytes. A single place for the whole program - used
//! both by `seeds = [...]` and by the runtime PDA signature: a typo would have
//! made the address and the signature disagree.

pub(crate) const BURN_STATE_SEED: &[u8] = b"burn_state";
pub(crate) const BURN_COUNTERS_SEED: &[u8] = b"burn_counters";
pub(crate) const VAULT_SEED: &[u8] = b"vault";
pub(crate) const VAULT_AUTHORITY_SEED: &[u8] = b"vault_authority";
