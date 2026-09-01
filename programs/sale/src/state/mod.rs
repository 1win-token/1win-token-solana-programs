//! On-chain state (SaleState, WalletAggregate, Deposit, Blacklist, Graylist).

pub mod aggregate;
pub mod blacklist;
pub mod deposit;
pub mod graylist;
pub mod sale_state;

pub use aggregate::*;
pub use blacklist::*;
pub use deposit::*;
pub use graylist::*;
pub use sale_state::*;
