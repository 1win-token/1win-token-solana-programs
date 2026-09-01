//! e2e harness for Vesting against surfpool - a REAL validator simulator behind
//! RPC rather than an in-process SVM.
//!
//! Contents:
//! - [`surfnet`] - the surfpool process plus cheat codes (deploy at declare_id,
//!   pauseClock + setAccount(Clock) used as a warp);
//! - [`env`] - [`env::E2e`]: actors, the $1WIN mint, sending and confirming
//!   transactions, error-code assertions, state readers;
//! - [`events`] - a parser for emit_cpi events "the way an indexer sees them",
//!   plus a discriminator cross-check of all thirteen events against the IDL.
//!
//! Instruction builders are NOT duplicated - they are imported from
//! vesting-test-common (pure Instruction constructors, the same ones the LiteSVM
//! tests use), including `funding_tx_ixs` - the shape of the atomic funding
//! transaction (Arch §2).
//!
//! Every test in the crate is #[ignore]: it runs only via `make e2e-vesting`.

pub mod env;
pub mod events;
pub mod surfnet;

pub use env::{custom_code, E2e, ACTOR_LAMPORTS, PACKET_LIMIT, WIN_DECIMALS};
pub use events::{assert_idl_event_discriminators, parse_vesting_events, VestingEvent};
pub use surfnet::Surfnet;

/// The on-chain VestingError code = the Anchor offset plus the variant index.
pub fn vesting_err(e: vesting::errors::VestingError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + e as u32
}

/// The on-chain error code of the Anchor dispatcher (AccountNotInitialized etc.).
pub fn anchor_err(e: anchor_lang::error::ErrorCode) -> u32 {
    e as u32
}
