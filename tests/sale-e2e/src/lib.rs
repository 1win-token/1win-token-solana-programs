//! e2e harness for Sale against surfpool - a REAL validator simulator behind RPC
//! rather than an in-process SVM (iteration 6, M2 item 8).
//!
//! Contents:
//! - [`surfnet`] - the surfpool process plus cheat codes (writeProgram at
//!   declare_id, pauseClock + setAccount(Clock) used as a warp);
//! - [`env`] - [`env::E2e`]: actors, mints, sending and confirming transactions,
//!   error-code assertions, state readers;
//! - [`events`] - a parser for emit_cpi events "the way an indexer sees them",
//!   plus a discriminator cross-check against the IDL.
//!
//! Instruction builders are NOT duplicated - they are imported from
//! sale-test-common (pure Instruction constructors, the same ones the LiteSVM
//! tests use).
//!
//! Every test in the crate is #[ignore]: it runs only via `make e2e`.

pub mod env;
pub mod events;
pub mod surfnet;

pub use env::{custom_code, E2e, ACTOR_LAMPORTS};
pub use events::{assert_idl_event_discriminators, parse_sale_events, SaleEvent};
pub use surfnet::Surfnet;

/// The on-chain SaleError code = the Anchor offset plus the variant index.
pub fn sale_err(e: sale::errors::SaleError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + e as u32
}

/// The on-chain error code of the Anchor dispatcher (AccountNotInitialized etc.).
pub fn anchor_err(e: anchor_lang::error::ErrorCode) -> u32 {
    e as u32
}
