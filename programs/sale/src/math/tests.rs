use proptest::collection::vec;
use proptest::prelude::*;

use super::*;

const DEFAULT_PRICE: u64 = expected::DEFAULT_PRICE_USDC;

// ----- Worked numeric examples from the spec (E1-E3) -----

/// The headline example (2.56x oversubscription): a $10,000 contribution yields
/// exactly 130,000 tokens and $6,100 comes back. The numbers were checked by hand.
#[test]
fn p1_oversubscription_canonical() {
    let got = allocate(
        expected::P1_NET,
        expected::P1_POOL,
        expected::DEFAULT_EMISSION,
        DEFAULT_PRICE,
        TOKEN_UNITS,
    )
    .unwrap();
    assert_eq!(
        got,
        Allocation {
            tokens: expected::P1_TOKENS,
            refund: expected::P1_REFUND,
        }
    );
    // "Spent" = contribution minus refund. Compared against a pre-computed
    // reference value.
    assert_eq!(expected::P1_NET - got.refund, expected::P1_SPENT);
}

/// Three wallets share an indivisible pool. Rounding down hands out slightly
/// less than the emission - the remainder ("dust") stays in the vault. Every
/// wallet is checked.
#[test]
fn p2_rounding_dust_three_wallets() {
    let mut tokens_sum: u64 = 0;
    for (i, &net) in expected::P2_NETS.iter().enumerate() {
        let got = allocate(
            net,
            expected::P2_POOL,
            expected::P2_EMISSION,
            DEFAULT_PRICE,
            TOKEN_UNITS,
        )
        .unwrap();
        assert_eq!(got.tokens, expected::P2_TOKENS[i], "wallet #{i}");
        assert_eq!(got.refund, expected::P2_REFUNDS[i], "wallet #{i}");
        tokens_sum += got.tokens;
    }
    assert_eq!(tokens_sum, expected::P2_TOKENS_SUM);
    assert_eq!(expected::P2_EMISSION - tokens_sum, expected::P2_REMAINDER);
    // Since it is oversubscribed, the entire emission is handed out.
    assert_eq!(
        distributable(
            true,
            expected::P2_POOL,
            expected::P2_EMISSION,
            DEFAULT_PRICE,
            TOKEN_UNITS
        )
        .unwrap(),
        expected::P2_EMISSION
    );
}

/// The boundary: oversubscription so small that spent = contribution and the
/// refund is 0.
#[test]
fn p3_spent_equals_net() {
    let got = allocate(
        expected::P3_NET,
        expected::P3_POOL,
        expected::DEFAULT_EMISSION,
        DEFAULT_PRICE,
        TOKEN_UNITS,
    )
    .unwrap();
    assert_eq!(
        got,
        Allocation {
            tokens: expected::P3_TOKENS,
            refund: expected::P3_REFUND,
        }
    );
}

// ----- Edge cases (spec §10.6) -----

/// A wallet contributed nothing - no tokens and no refund. Both when
/// oversubscribed and when undersubscribed.
#[test]
fn edge_net_zero_allocates_nothing() {
    let zero = Allocation {
        tokens: 0,
        refund: 0,
    };
    // The oversubscribed case
    let over = allocate(
        0,
        expected::P1_POOL,
        expected::DEFAULT_EMISSION,
        DEFAULT_PRICE,
        TOKEN_UNITS,
    )
    .unwrap();
    assert_eq!(over, zero);
    // The undersubscribed case (collected exactly the soft cap)
    let under = allocate(
        0,
        expected::SOFT_CAP_USDC_DEFAULT,
        expected::DEFAULT_EMISSION,
        DEFAULT_PRICE,
        TOKEN_UNITS,
    )
    .unwrap();
    assert_eq!(under, zero);
}

/// A share smaller than one token unit: rounding down yields 0 tokens, so the
/// whole contribution is refunded.
#[test]
fn edge_extreme_oversubscription_zero_tokens() {
    let got = allocate(
        expected::EDGE_ZERO_TOKENS_NET,
        expected::EDGE_ZERO_TOKENS_POOL,
        expected::EDGE_ZERO_TOKENS_EMISSION,
        DEFAULT_PRICE,
        TOKEN_UNITS,
    )
    .unwrap();
    assert_eq!(
        got,
        Allocation {
            tokens: 0,
            refund: expected::EDGE_ZERO_TOKENS_NET,
        }
    );
}

/// A single wallet takes the whole pool (oversubscribed): it receives the entire
/// emission, spends exactly the cap, and the rest is refunded.
#[test]
fn edge_single_participant_takes_full_pool() {
    let got = allocate(
        expected::EDGE_FULL_POOL_NET,
        expected::EDGE_FULL_POOL_NET, // pool == net
        expected::DEFAULT_EMISSION,
        DEFAULT_PRICE,
        TOKEN_UNITS,
    )
    .unwrap();
    assert_eq!(
        got,
        Allocation {
            tokens: expected::DEFAULT_EMISSION,
            refund: expected::EDGE_FULL_POOL_REFUND,
        }
    );
    // "Spent" (contribution minus refund) must equal the soft cap
    assert_eq!(
        expected::EDGE_FULL_POOL_NET - got.refund,
        expected::SOFT_CAP_USDC_DEFAULT
    );
}

/// Exactly on the boundary (the cap was collected precisely) counts as
/// undersubscribed - the comparison is strict. We check that the total handed
/// out never exceeds the emission, even when the pool is split across wallets.
#[test]
fn edge_exact_soft_cap_boundary_is_undersubscribed() {
    let pool = expected::SOFT_CAP_USDC_DEFAULT;
    let emission = expected::DEFAULT_EMISSION;
    assert!(!is_oversubscribed(pool, emission, DEFAULT_PRICE, TOKEN_UNITS).unwrap());

    // One wallet holding the whole pool: the entire emission, no refund.
    let single = allocate(pool, pool, emission, DEFAULT_PRICE, TOKEN_UNITS).unwrap();
    assert_eq!(
        single,
        Allocation {
            tokens: emission,
            refund: 0
        }
    );

    // The same pool split across two wallets: rounding down eats 1 unit, and the
    // amount handed out still does not exceed the emission.
    let mut tokens_sum: u64 = 0;
    for (i, &net) in expected::EDGE_BOUNDARY_SPLIT_NETS.iter().enumerate() {
        let got = allocate(net, pool, emission, DEFAULT_PRICE, TOKEN_UNITS).unwrap();
        assert_eq!(
            got.tokens,
            expected::EDGE_BOUNDARY_SPLIT_TOKENS[i],
            "wallet #{i}"
        );
        assert_eq!(got.refund, 0, "wallet #{i}");
        tokens_sum += got.tokens;
    }
    assert_eq!(tokens_sum, expected::EDGE_BOUNDARY_SPLIT_TOKENS_SUM);
    assert!(tokens_sum <= emission);

    assert_eq!(
        distributable(false, pool, emission, DEFAULT_PRICE, TOKEN_UNITS).unwrap(),
        emission
    );
}

/// An empty sale (0 collected): there is nothing to hand out, distributable = 0.
#[test]
fn edge_pool_zero_distributable_zero() {
    assert_eq!(
        distributable(
            false,
            0,
            expected::DEFAULT_EMISSION,
            DEFAULT_PRICE,
            TOKEN_UNITS
        )
        .unwrap(),
        0
    );
}

/// With the standard parameters the soft cap is exactly $3.9M.
#[test]
fn soft_cap_usdc_at_defaults() {
    assert_eq!(
        soft_cap_usdc(expected::DEFAULT_EMISSION, DEFAULT_PRICE, TOKEN_UNITS).unwrap(),
        expected::SOFT_CAP_USDC_DEFAULT
    );
}

// ----- Error paths -----

/// When an intermediate result does not fit into 64 bits the result is an error,
/// not a panic.
#[test]
fn allocate_overflow_on_downcast_is_error() {
    // Undersubscribed (pool=1): tokens = u64::MAX × token_units ≈ 1.8e27 does not
    // fit into 64 bits → an error when narrowing after the division.
    let got = allocate(u64::MAX, 1, u64::MAX, 1, TOKEN_UNITS);
    assert_eq!(got, Err(SaleError::MathOverflow));
}

/// A division by zero is possible only at price 0. token_units is always >= 1,
/// so the cap and the "spent" amount never divide by zero. We expect an error,
/// not a panic.
#[test]
fn zero_divisor_is_error_not_panic() {
    // Price 0 with pool=0 (undersubscribed) → the token computation divides by
    // the price → an error.
    assert_eq!(
        allocate(5, 0, 10, 0, TOKEN_UNITS),
        Err(SaleError::MathOverflow)
    );
    // The same for distributable: at price 0 total must be 0, otherwise any
    // pool>0 makes it oversubscribed and returns the emission without dividing.
    assert_eq!(
        distributable(false, 0, 10, 0, TOKEN_UNITS),
        Err(SaleError::MathOverflow)
    );
}

// ----- Property checks over random inputs -----
// proptest runs each check over thousands of random numbers, looking for an
// input set that breaks it. A green test means the rule always holds.
//
// token_units is drawn from every valid decimals value (0..=19, always >= 1).
// For the money-related checks the price is drawn from the working range
// 1..token_units (the dust reserve requires price < token_units). The
// "nothing panics" check runs at any price, including 0.

/// A valid token_units = 10^dec, with dec from 0 to 19 (always >= 1).
fn token_units_strategy() -> impl Strategy<Value = u64> {
    (0u32..=19).prop_map(|d| 10u64.pow(d))
}

/// A (token_units, price) pair constrained by price < token_units (the dust
/// reserve). The price is derived from token_units rather than filtered out, so
/// no cases are rejected. dec >= 1, otherwise the range 1..token_units is empty.
fn units_and_valid_price() -> impl Strategy<Value = (u64, u64)> {
    (1u32..=19u32).prop_flat_map(|d| {
        let units = 10u64.pow(d);
        (Just(units), 1u64..units)
    })
}

/// Cases per invariant: the default is fast, PROPTEST_CASES raises it (M2: 10^5).
/// We read the env var ourselves: an explicit `cases` field would override it.
fn proptest_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024)
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: proptest_cases(),
        // prop_assume filters: the reject budget scales with the case count.
        max_global_rejects: proptest_cases().saturating_mul(64),
        ..ProptestConfig::default()
    })]

    /// Refund = contribution minus spent, and spent never exceeds the contribution.
    #[test]
    fn spent_refund_identity_oversubscribed(
        (pool, net) in (1u64..=u64::MAX).prop_flat_map(|pool| (Just(pool), 0..=pool)),
        emission in any::<u64>(),
        (token_units, price_usdc) in units_and_valid_price(),
    ) {
        prop_assume!(is_oversubscribed(pool, emission, price_usdc, token_units).unwrap());
        let got = allocate(net, pool, emission, price_usdc, token_units).unwrap();
        // The spent amount is recomputed by a separate formula (rounded up).
        let spent = u128::from(got.tokens)
            .checked_mul(u128::from(price_usdc))
            .unwrap()
            .div_ceil(u128::from(token_units));
        prop_assert!(spent <= u128::from(net));
        prop_assert_eq!(u128::from(got.refund), u128::from(net) - spent);
    }

    /// When undersubscribed there is never a refund (always 0).
    #[test]
    fn undersubscribed_refund_is_zero(
        net in any::<u64>(),
        pool in any::<u64>(),
        emission in any::<u64>(),
        token_units in token_units_strategy(),
        price_usdc in any::<u64>(),
    ) {
        prop_assume!(!is_oversubscribed(pool, emission, price_usdc, token_units).unwrap());
        if let Ok(got) = allocate(net, pool, emission, price_usdc, token_units) {
            prop_assert_eq!(got.refund, 0);
        }
    }

    /// Contributing more never yields fewer tokens (at the same pool and price).
    #[test]
    fn tokens_monotonic_in_net(
        net_a in 0u64..=10_000_000_000_000,
        net_b in 0u64..=10_000_000_000_000,
        pool in 1u64..=u64::MAX,
        emission in any::<u64>(),
        (token_units, price_usdc) in units_and_valid_price(),
    ) {
        let (lo, hi) = if net_a <= net_b { (net_a, net_b) } else { (net_b, net_a) };
        prop_assume!(hi <= pool);
        let a = allocate(lo, pool, emission, price_usdc, token_units).unwrap();
        let b = allocate(hi, pool, emission, price_usdc, token_units).unwrap();
        prop_assert!(a.tokens <= b.tokens);
    }

    /// Oversubscribed: however the pool is split across wallets, the total handed
    /// out never exceeds what was promised - the vault holds enough tokens for
    /// everyone.
    #[test]
    fn vault_solvency_oversubscribed(
        nets in vec(0u64..=1_000_000_000_000u64, 1..=16),
        emission in 0u64..=10_000_000_000_000,
        (token_units, price_usdc) in units_and_valid_price(),
    ) {
        let pool: u64 = nets.iter().sum();
        prop_assume!(is_oversubscribed(pool, emission, price_usdc, token_units).unwrap());
        let dist = distributable(true, pool, emission, price_usdc, token_units).unwrap();
        let total: u128 = nets
            .iter()
            .map(|&net| u128::from(allocate(net, pool, emission, price_usdc, token_units).unwrap().tokens))
            .sum();
        prop_assert!(total <= u128::from(dist));
    }

    /// Undersubscribed: the total handed out never exceeds distributable, which in
    /// turn never exceeds the emission.
    #[test]
    fn vault_solvency_undersubscribed(
        nets in vec(0u64..=1_000_000_000_000u64, 1..=16),
        emission in any::<u64>(),
        (token_units, price_usdc) in units_and_valid_price(),
    ) {
        let pool: u64 = nets.iter().sum();
        prop_assume!(!is_oversubscribed(pool, emission, price_usdc, token_units).unwrap());
        let dist = distributable(false, pool, emission, price_usdc, token_units).unwrap();
        prop_assert!(dist <= emission);
        let total: u128 = nets
            .iter()
            .map(|&net| u128::from(allocate(net, pool, emission, price_usdc, token_units).unwrap().tokens))
            .sum();
        prop_assert!(total <= u128::from(dist));
    }

    /// The dust reserve: because of rounding, the money shortfall stays below the
    /// number of wallets (1 unit per wallet is enough). This needs
    /// price < token_units, otherwise the per-wallet round-up could exceed 1 unit.
    #[test]
    fn dust_reserve_sufficient(
        nets in vec(1u64..=1_000_000_000_000u64, 1..=16),
        emission in 1u64..=1_000_000_000_000_000,
        (token_units, price_usdc) in units_and_valid_price(),
    ) {
        let pool: u64 = nets.iter().sum();
        prop_assume!(is_oversubscribed(pool, emission, price_usdc, token_units).unwrap());
        let soft_cap = soft_cap_usdc(emission, price_usdc, token_units).unwrap();
        let spent_total: i128 = nets
            .iter()
            .map(|&net| {
                let got = allocate(net, pool, emission, price_usdc, token_units).unwrap();
                i128::from(net) - i128::from(got.refund)
            })
            .sum();
        let wallets = nets.len() as i128;
        prop_assert!(i128::from(soft_cap) - spent_total < wallets);
    }

    /// No function panics on any input - not even at price zero. Every function
    /// is exercised at any price (including 0) and valid token_units.
    #[test]
    fn math_is_total_on_full_u64_domain(
        net in any::<u64>(),
        pool in any::<u64>(),
        emission in any::<u64>(),
        token_units in token_units_strategy(),
        price_usdc in any::<u64>(),
    ) {
        let over = is_oversubscribed(pool, emission, price_usdc, token_units).unwrap_or(false);
        let _ = allocate(net, pool, emission, price_usdc, token_units);
        let _ = distributable(over, pool, emission, price_usdc, token_units);
        let _ = soft_cap_usdc(emission, price_usdc, token_units);
    }
}
