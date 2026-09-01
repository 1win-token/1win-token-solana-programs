//! Reference constants for the Vesting math tests: the schedules of §10.5 and the
//! edges of §10.6 from `docs/Vesting_Program_Architecture.md`.
//!
//! Every reference value is a hand-written literal derived BY HAND from the
//! example's premises (the derivation lives in the constant's doc comment). The
//! crate has zero dependencies and physically cannot import the program's formula:
//! "a reference is an independent literal" holds by construction
//! (tests-bind-to-real-code).
//!
//! Units: token units (8 decimals, 1 $1WIN = 10⁸ units).

pub mod vesting;
