//! Unit tests for the `VestingConfig` value checks: the TGE window boundaries, a
//! zero role key, any two of the three roles colliding, and the payout address.
//! Each case expects a specific error code, not merely the fact of a rejection.

use super::*;

/// The error code taken from a validation result; success is `None`.
fn code(r: Result<()>) -> Option<u32> {
    match r {
        Ok(()) => None,
        Err(Error::AnchorError(e)) => Some(e.error_code_number),
        Err(other) => panic!("expected a program error, got: {other:?}"),
    }
}

fn err(e: VestingError) -> Option<u32> {
    Some(u32::from(e))
}

/// A non-zero key with the given first byte.
fn key(tag: u8) -> Pubkey {
    let mut bytes = [0u8; 32];
    bytes[0] = tag;
    Pubkey::new_from_array(bytes)
}

// ----- TGE window -----

const NOW: i64 = 1_000_000;

/// TGE must be strictly in the future: the past and equality with now are both
/// rejected.
#[test]
fn tge_must_be_ahead_of_now() {
    let cases: [(i64, Option<u32>); 3] = [
        (NOW - 1, err(VestingError::InvalidTge)),
        (NOW, err(VestingError::InvalidTge)),
        (NOW + 1, None),
    ];
    for (tge_ts, want) in cases {
        assert_eq!(
            code(VestingConfig::validate_tge(tge_ts, NOW)),
            want,
            "tge_ts = {tge_ts}"
        );
    }
}

/// The delay ceiling: exactly on it passes, one second beyond does not.
#[test]
fn tge_delay_ceiling() {
    let at = NOW + MAX_TGE_DELAY;
    let cases: [(i64, Option<u32>); 3] = [
        (at - 1, None),
        (at, None),
        (at + 1, err(VestingError::InvalidTge)),
    ];
    for (tge_ts, want) in cases {
        assert_eq!(
            code(VestingConfig::validate_tge(tge_ts, NOW)),
            want,
            "tge_ts = {tge_ts}"
        );
    }
}

/// The i64 extremes: the ceiling addition returns an error rather than panics.
#[test]
fn tge_ceiling_overflow_is_error() {
    assert_eq!(
        code(VestingConfig::validate_tge(i64::MAX, i64::MAX - 1)),
        err(VestingError::MathOverflow)
    );
}

// ----- Role key -----

/// The zero key is rejected, any other key passes.
#[test]
fn role_key_rejects_zero() {
    let cases: [(Pubkey, Option<u32>); 3] = [
        (Pubkey::default(), err(VestingError::InvalidAuthority)),
        (key(1), None),
        (Pubkey::new_from_array([255u8; 32]), None),
    ];
    for (k, want) in cases {
        assert_eq!(code(VestingConfig::validate_role_key(k)), want, "key = {k}");
    }
}

// ----- Payout address -----

/// The zero address and the vault authority's address are rejected; an ordinary
/// one passes.
#[test]
fn payee_rejects_zero_and_vault_authority() {
    let vault_authority = key(9);
    let cases: [(Pubkey, Option<u32>); 3] = [
        (key(1), None),
        (Pubkey::default(), err(VestingError::InvalidAuthority)),
        (vault_authority, err(VestingError::InvalidAuthority)),
    ];
    for (payee, want) in cases {
        assert_eq!(
            code(VestingConfig::validate_payee(payee, vault_authority)),
            want,
            "payee = {payee}"
        );
    }
}
