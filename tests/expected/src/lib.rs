//! Reference constants for the Sale math tests: the "worked numeric examples"
//! E1-E3 and the edges of §10.6 from `docs/TokenSale_Program_Architecture.md`.
//!
//! Every reference value is a hand-written literal derived BY HAND from the
//! example's premises (the derivation lives in the constant's doc comment). The
//! crate has zero dependencies and physically cannot import the program's formula:
//! "a reference is an independent literal" holds by construction
//! (tests-bind-to-real-code).
//!
//! Units: USDC units (6 decimals, 1 USDC = 10⁶ units),
//! token units (8 decimals, 1 $1WIN = 10⁸ units).

// ---------- Round defaults (Arch §9.2, §10.1) ----------

/// The $0.03 price: USDC base units (6 decimals) per ONE WHOLE $1WIN.
/// $0.03 × 10⁶ USDC units = 30_000 units per whole token. A valid price satisfies
/// 0 < price_usdc < token_units (10⁸) - one USDC unit buys more than 1 token unit.
/// Rate: token_units / price_usdc = 10⁸ / 30_000 = 10_000 / 3 token units per USDC unit.
pub const DEFAULT_PRICE_USDC: u64 = 30_000;

/// Emission of 130M $1WIN × 10⁸ = 1.3×10¹⁶ token units.
pub const DEFAULT_EMISSION: u64 = 13_000_000_000_000_000;

/// The soft cap at the defaults: floor(EMI × price_usdc / token_units)
/// = floor(13_000_000_000_000_000 × 30_000 / 100_000_000)
/// = floor(390_000_000_000_000_000_000 / 100_000_000)
/// = 3_900_000_000_000 exactly (no remainder) = $3.9M.
pub const SOFT_CAP_USDC_DEFAULT: u64 = 3_900_000_000_000;

// ---------- E1: the canonical 2.56x oversubscription (TS-32) ----------

/// The wallet's contribution: $10,000 = 10⁴ × 10⁶ units.
pub const P1_NET: u64 = 10_000_000_000;

/// The final pool: $10M = 10⁷ × 10⁶ units. Oversubscribed (pool × token_units > EMI × price_usdc):
/// pool × 10⁸ = 10¹³ × 10⁸ = 10²¹ > EMI × 30_000 = 1.3×10¹⁶ × 3×10⁴ = 3.9×10²⁰.
pub const P1_POOL: u64 = 10_000_000_000_000;

/// tokens = floor(net × EMI / pool) = floor(10¹⁰ × 1.3×10¹⁶ / 10¹³)
/// = floor(1.3×10²⁶ / 10¹³) = 1.3×10¹³ exactly = 130,000.00000000 $1WIN.
pub const P1_TOKENS: u64 = 13_000_000_000_000;

/// spent = ceil(tokens × price_usdc / token_units) = ceil(1.3×10¹³ × 30_000 / 10⁸)
/// = ceil(3.9×10¹⁷ / 10⁸) = 3.9×10⁹ exactly (the ceil has no effect) = $3,900.
pub const P1_SPENT: u64 = 3_900_000_000;

/// refund = net − spent = 10_000_000_000 − 3_900_000_000 = $6 100.
pub const P1_REFUND: u64 = 6_100_000_000;

// ---------- E2: rounding dust (oversubscribed, an indivisible share) ----------
//
// The example's price: the default price_usdc = 30_000. The choice is documented:
// the architecture fixes only pool/EMI/nets in E2, and the price may be any valid
// one (0 < price_usdc < token_units) that produces oversubscription; the default
// qualifies:
// pool × token_units = 7 × 10⁸ = 7×10⁸ > EMI × price_usdc = 10 × 30_000 = 3×10⁵,
// - and it avoids introducing a second price into the tests.

/// The aggregate pool of example E2: 7 USDC units.
pub const P2_POOL: u64 = 7;

/// The emission of example E2: 10 token units.
pub const P2_EMISSION: u64 = 10;

/// The three wallets' contributions: 3 + 3 + 1 = 7 = pool.
pub const P2_NETS: [u64; 3] = [3, 3, 1];

/// tokens_i = floor(net_i × 10 / 7):
/// floor(30/7) = floor(4.28…) = 4; floor(30/7) = 4; floor(10/7) = floor(1.42…) = 1.
pub const P2_TOKENS: [u64; 3] = [4, 4, 1];

/// spent_i = ceil(tokens_i × 30_000 / 100_000_000) (at price_usdc = 30_000):
/// ceil(120_000/10⁸ = 0.0012) = 1; ceil(120_000/10⁸) = 1; ceil(30_000/10⁸ = 0.0003) = 1.
/// refund_i = net_i − spent_i: 3−1 = 2; 3−1 = 2; 1−1 = 0.
pub const P2_REFUNDS: [u64; 3] = [2, 2, 0];

/// Σ tokens = 4 + 4 + 1 = 9 ≤ EMI = 10.
pub const P2_TOKENS_SUM: u64 = 9;

/// The emission remainder (floor-rounding dust): 10 − 9 = 1 unit → sweep.
pub const P2_REMAINDER: u64 = 1;

// ---------- E3: the spent == net edge (minimum contribution, slight oversubscription) ----------

/// The contribution: $10 = 10⁷ USDC units (the min_per_tx minimum).
pub const P3_NET: u64 = 10_000_000;

/// The pool: SOFT_CAP + 1 unit. Oversubscription is minimal (pool × token_units > EMI × price_usdc):
/// pool × 10⁸ = 3_900_000_000_001 × 10⁸ = 3.9000000000010×10²⁰ > 3.9×10²⁰ = EMI × 30_000.
pub const P3_POOL: u64 = 3_900_000_000_001;

/// tokens = floor(10⁷ × 1.3×10¹⁶ / 3_900_000_000_001) = floor(1.3×10²³ / 3.900000000001×10¹²)
/// = floor(33,333,333,333.32…) = 33_333_333_333.
/// The bounds checked by hand:
///   33_333_333_333 × 3_900_000_000_001 = 1.2999999999873×10²³ ≤ 1.3×10²³,
///   33_333_333_334 × 3_900_000_000_001 ≈ 1.3000000000263×10²³ > 1.3×10²³.
pub const P3_TOKENS: u64 = 33_333_333_333;

/// spent = ceil(33_333_333_333 × 30_000 / 100_000_000) = ceil(999_999_999_990_000 / 10⁸)
/// = ceil(9_999_999.9999) = 10_000_000 == net ⇒ refund = 0.
pub const P3_REFUND: u64 = 0;

// ---------- The edges of §10.6 ----------

/// Extreme oversubscription (tokens == 0): a pool of 1_000_000 units at EMI = 10.
/// Oversubscribed: 1_000_000 × token_units = 10⁶ × 10⁸ = 10¹⁴ > 10 × 30_000 = 3×10⁵.
pub const EDGE_ZERO_TOKENS_POOL: u64 = 1_000_000;

/// The emission of the "tokens == 0" edge.
pub const EDGE_ZERO_TOKENS_EMISSION: u64 = 10;

/// A contribution of 1 unit: tokens = floor(1 × 10 / 1_000_000) = 0;
/// spent = ceil(0 × 30_000 / 10⁸) = 0; refund = net = 1 (a full refund).
pub const EDGE_ZERO_TOKENS_NET: u64 = 1;

/// A single participant holding the whole pool (oversubscribed): net == pool == $10M
/// (the premises of E1). tokens = floor(10¹³ × 1.3×10¹⁶ / 10¹³) = EMI;
/// spent = ceil(1.3×10¹⁶ × 30_000 / 10⁸) = 3.9×10¹² exactly = SOFT_CAP.
pub const EDGE_FULL_POOL_NET: u64 = 10_000_000_000_000;

/// refund = net − soft_cap = 10_000_000_000_000 − 3_900_000_000_000 = $6.1M.
pub const EDGE_FULL_POOL_REFUND: u64 = 6_100_000_000_000;

/// The boundary pool × token_units == EMI × price_usdc (pool == SOFT_CAP: 3.9×10¹² × 10⁸ ==
/// 1.3×10¹⁶ × 30_000 == 3.9×10²⁰ - the UNDERSUBSCRIBED branch, the comparison is strict `>`):
/// the pool split across two wallets.
/// 1_900_000_000_000 + 2_000_000_000_000 = 3_900_000_000_000 = pool.
pub const EDGE_BOUNDARY_SPLIT_NETS: [u64; 2] = [1_900_000_000_000, 2_000_000_000_000];

/// tokens_i = floor(net_i × token_units / price_usdc) = floor(net_i × 10⁸ / 30_000)
/// = floor(net_i × 10_000 / 3):
/// floor(1.9×10¹⁶ / 3) = floor(6_333_333_333_333_333.33…) = 6_333_333_333_333_333;
/// floor(2.0×10¹⁶ / 3) = floor(6_666_666_666_666_666.66…) = 6_666_666_666_666_666.
pub const EDGE_BOUNDARY_SPLIT_TOKENS: [u64; 2] = [6_333_333_333_333_333, 6_666_666_666_666_666];

/// Σ = 6_333_333_333_333_333 + 6_666_666_666_666_666 = 12_999_999_999_999_999
/// = EMI − 1 ≤ EMI (the floor ate 1 unit - the dust stays in the vault).
pub const EDGE_BOUNDARY_SPLIT_TOKENS_SUM: u64 = 12_999_999_999_999_999;
