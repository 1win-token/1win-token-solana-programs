//! Reference constants for the Vesting math tests: the five schedules of §10.5 and
//! the "worked numeric examples" V1-V5 from `docs/Vesting_Program_Architecture.md`.
//!
//! Every number was derived BY HAND from the example's premises following the text of
//! §10.1-§10.3 (the derivation, with its intermediate results, lives in the
//! constant's doc comment). The program's `vested()` formula is neither reproduced
//! nor imported here: the crate has zero dependencies, so "a reference is an
//! independent literal" holds by construction (tests-bind-to-real-code). A
//! divergence between the code and the spec is caught precisely by this - the test
//! does not check the code against itself.
//!
//! Units: token units (8 decimals, 1 $1WIN = 10⁸ token units), shares in basis points
//! (10000 = 100%), time in unix seconds.

// ---------- Scale, shares and time (§10.1) ----------

/// One whole $1WIN in token units: 8 decimals ⇒ 10⁸ = 100_000_000.
pub const TOKEN_UNIT: u64 = 100_000_000;

/// The basis-point denominator: 10000 bps = 100%.
pub const BPS_DENOM: u64 = 10_000;

/// A month is fixed at 30 days: 30 × 86_400 = 2_592_000 s. Period `n` arrives at
/// `tge_ts + n × MONTH`.
pub const MONTH: i64 = 2_592_000;

/// Seconds in a day: `MONTH / 30 = 2_592_000 / 30 = 86_400`.
///
/// Converting "a day → an offset from TGE": day `d` (numbered per §10.1,
/// `day = 1 + n·30`) begins at `tge_ts + (d − 1) × DAY_SECS`, which is the same as
/// `tge_ts + ((d − 1) / 30) × MONTH`. For example, day 271 → `270 × 86_400 =
/// 23_328_000` s `= 9 × 2_592_000 = 9 × MONTH` ⇒ period `p = 9`. Conversely, period
/// `p` begins on day `1 + 30 × p`.
pub const DAY_SECS: i64 = 86_400;

// ---------- The five reference schedules of §10.5 ----------
//
// `s = max(cliff_months, 1)` is the period of the first monthly tranche;
// `M = ceil((10000 − tge_bps) / monthly_bps)` when `monthly_bps > 0`; the full
// release lands in period `s + M − 1`, on day `1 + 30 × (s + M − 1)`.
// §10.5 does not fix `reference_amount` (the tokenomics does) - the buckets' amounts
// are below, in the V1-V5 blocks and in "Liquidity".

// --- Marketing: «100% TGE» ---

/// `tge_bps = 10000` - the whole amount at the TGE instant.
pub const MARKETING_TGE_BPS: u16 = 10_000;

/// `monthly_bps = 0` - there are no monthly tranches (the "100% at TGE" branch of
/// §10.2).
pub const MARKETING_MONTHLY_BPS: u16 = 0;

/// There is no cliff. With `monthly_bps == 0` the value does not affect the payout at
/// all: the branch returns `reference_amount` before `s` is computed.
pub const MARKETING_CLIFF_MONTHS: u16 = 0;

/// `M` is undefined when `monthly_bps == 0` (one cannot divide by zero, §10.4); the
/// table of §10.5 carries a degenerate `0`.
pub const MARKETING_TRANCHES_M: u16 = 0;

/// The full release is at the start, day 1 (`p = 0`, the TGE instant). This schedule
/// has no separate "day of the first monthly tranche".
pub const MARKETING_FULL_RELEASE_DAY: i64 = 1;

// --- Ecosystem ---

/// `tge_bps = 1000` = 10% at the TGE instant.
pub const ECOSYSTEM_TGE_BPS: u16 = 1_000;

/// `monthly_bps = 500` = 5% per month.
pub const ECOSYSTEM_MONTHLY_BPS: u16 = 500;

/// No cliff ⇒ `s = max(0, 1) = 1`.
pub const ECOSYSTEM_CLIFF_MONTHS: u16 = 0;

/// `M = ceil((10000 − 1000) / 500) = ceil(9000 / 500) = ceil(18.0) = 18`
/// (9000 = 18 × 500, with no remainder). Checking the ceiling of §10.3:
/// `1000 + (18 − 1) × 500 = 1000 + 8500 = 9500 < 10000`, while
/// `1000 + 18 × 500 = 10000` is exactly 100%, so the terminal branch tops up nothing
/// beyond it.
pub const ECOSYSTEM_TRANCHES_M: u16 = 18;

/// The first monthly tranche: period `s = 1`, day `1 + 30 × 1 = 31`.
pub const ECOSYSTEM_FIRST_MONTHLY_DAY: i64 = 31;

/// The full release: period `s + M − 1 = 1 + 18 − 1 = 18`, day
/// `1 + 30 × 18 = 1 + 540 = 541`.
pub const ECOSYSTEM_FULL_RELEASE_DAY: i64 = 541;

// --- Community Rewards ---

/// `tge_bps = 0` - nothing is released at the TGE instant.
pub const COMMUNITY_TGE_BPS: u16 = 0;

/// `monthly_bps = 834` = 8.34% per month.
pub const COMMUNITY_MONTHLY_BPS: u16 = 834;

/// No cliff ⇒ `s = 1`.
pub const COMMUNITY_CLIFF_MONTHS: u16 = 0;

/// `M = ceil((10000 − 0) / 834) = ceil(11.9904…) = 12`, because `834 × 11 = 9174 <
/// 10000` while `834 × 12 = 10008 ≥ 10000`. The shortfall in shares (11 monthly
/// tranches give only `11 × 834 = 9174` bps) is closed by the terminal branch in the
/// 12th period (example V4).
pub const COMMUNITY_TRANCHES_M: u16 = 12;

/// The first monthly tranche: period `s = 1`, day `1 + 30 = 31`.
pub const COMMUNITY_FIRST_MONTHLY_DAY: i64 = 31;

/// The full release: period `s + M − 1 = 1 + 12 − 1 = 12`, day
/// `1 + 30 × 12 = 1 + 360 = 361`.
pub const COMMUNITY_FULL_RELEASE_DAY: i64 = 361;

// --- Treasury ---

/// `tge_bps = 0` - nothing is released at the TGE instant.
pub const TREASURY_TGE_BPS: u16 = 0;

/// `monthly_bps = 400` = 4% per month.
pub const TREASURY_MONTHLY_BPS: u16 = 400;

/// A 9-month cliff ⇒ `s = max(9, 1) = 9`.
pub const TREASURY_CLIFF_MONTHS: u16 = 9;

/// `M = ceil((10000 − 0) / 400) = ceil(25.0) = 25` (10000 = 25 × 400, with no
/// remainder). The sum `cliff + M = 9 + 25 = 34 ≤ MAX_SCHEDULE_MONTHS = 120` - the
/// longest real schedule clears the ceiling with room to spare.
pub const TREASURY_TRANCHES_M: u16 = 25;

/// The first monthly tranche: period `s = 9`, day `1 + 30 × 9 = 1 + 270 = 271`.
pub const TREASURY_FIRST_MONTHLY_DAY: i64 = 271;

/// The full release: period `s + M − 1 = 9 + 25 − 1 = 33`, day
/// `1 + 30 × 33 = 1 + 990 = 991`.
pub const TREASURY_FULL_RELEASE_DAY: i64 = 991;

// --- Liquidity ---

/// `tge_bps = 1000` = 10% at the TGE instant.
pub const LIQUIDITY_TGE_BPS: u16 = 1_000;

/// `monthly_bps = 750` = 7.5% per month.
pub const LIQUIDITY_MONTHLY_BPS: u16 = 750;

/// No cliff ⇒ `s = 1`.
pub const LIQUIDITY_CLIFF_MONTHS: u16 = 0;

/// `M = ceil((10000 − 1000) / 750) = ceil(9000 / 750) = ceil(12.0) = 12`
/// (9000 = 12 × 750, with no remainder).
pub const LIQUIDITY_TRANCHES_M: u16 = 12;

/// The first monthly tranche: period `s = 1`, day `1 + 30 = 31`.
pub const LIQUIDITY_FIRST_MONTHLY_DAY: i64 = 31;

/// The full release: period `s + M − 1 = 1 + 12 − 1 = 12`, day
/// `1 + 30 × 12 = 1 + 360 = 361`.
pub const LIQUIDITY_FULL_RELEASE_DAY: i64 = 361;

// ---------- V1: «100% TGE» (Marketing) ----------

/// `reference_amount = 50 000 000 $1WIN` = 50_000_000 × 10⁸
/// = 5_000_000_000_000_000 token-units (5×10¹⁵).
pub const V1_REFERENCE: u64 = 5_000_000_000_000_000;

/// Before TGE (`now < tge_ts`) nothing is unlocked - the first branch of §10.2.
pub const V1_VESTED_BEFORE_TGE: u64 = 0;

/// From TGE onwards, forever: the `monthly_bps == 0` branch returns
/// `reference_amount` = 5_000_000_000_000_000 without computing shares. A check that
/// the value would coincide with the share-based one: floor(5×10¹⁵ × 10000 / 10000) =
/// 5×10¹⁵.
pub const V1_VESTED_FROM_TGE: u64 = 5_000_000_000_000_000;

// ---------- V2: the step-by-step unlock (Ecosystem) ----------
//
// `reference_amount = 100,000,000 $1WIN`, `tge_bps = 1000`, `monthly_bps = 500`,
// `cliff = 0`; `s = 1`, `M = 18` (see ECOSYSTEM_*).

/// `reference_amount = 100 000 000 $1WIN` = 10⁸ × 10⁸ = 10¹⁶
/// = 10_000_000_000_000_000 token-units.
pub const V2_REFERENCE: u64 = 10_000_000_000_000_000;

/// `tge_amt = floor(10¹⁶ × 1000 / 10000) = floor(10¹⁹ / 10⁴) = 10¹⁵`
/// = 1_000_000_000_000_000 (10% = 10,000,000 $1WIN), with no remainder.
pub const V2_TGE_AMT: u64 = 1_000_000_000_000_000;

/// `monthly_amt = floor(10¹⁶ × 500 / 10000) = floor(5×10¹⁸ / 10⁴) = 5×10¹⁴`
/// = 500_000_000_000_000 (5% = 5,000,000 $1WIN), with no remainder.
pub const V2_MONTHLY_AMT: u64 = 500_000_000_000_000;

/// Day 1 (`p = 0`): `p < s = 1` ⇒ `months = 0` ⇒ `vested = tge_amt + 0 =
/// 1_000_000_000_000_000` (10%). The same value holds throughout period 0, that is,
/// also at `tge + MONTH − 1` (day 30, the last second before day 31).
pub const V2_VESTED_DAY_1: u64 = 1_000_000_000_000_000;

/// Day 31 (`p = 1`): `months = min(1 − 1 + 1, 18) = 1` ⇒
/// `vested = 10¹⁵ + 1 × 5×10¹⁴ = 1.5×10¹⁵ = 1_500_000_000_000_000` (15%).
pub const V2_VESTED_DAY_31: u64 = 1_500_000_000_000_000;

/// Day 61 (`p = floor(60 / 30) = 2`): `months = min(2 − 1 + 1, 18) = 2` ⇒
/// `vested = 10¹⁵ + 2 × 5×10¹⁴ = 2×10¹⁵ = 2_000_000_000_000_000` (20%).
pub const V2_VESTED_DAY_61: u64 = 2_000_000_000_000_000;

/// Day 151 (`p = floor(150 / 30) = 5`): `months = min(5 − 1 + 1, 18) = 5` ⇒
/// `vested = 10¹⁵ + 5 × 5×10¹⁴ = 10¹⁵ + 2.5×10¹⁵ = 3.5×10¹⁵`
/// = 3_500_000_000_000_000 (35%).
pub const V2_VESTED_DAY_151: u64 = 3_500_000_000_000_000;

/// Day 511 (`p = 17 = s + M − 2`, the pre-terminal period): `months =
/// min(17 − 1 + 1, 18) = 17 = M − 1` ⇒ `vested = 10¹⁵ + 17 × 5×10¹⁴ =
/// 10¹⁵ + 8.5×10¹⁵ = 9.5×10¹⁵ = 9_500_000_000_000_000` (95%, strictly below the
/// reference - §10.3). The value holds until the last second before day 541.
pub const V2_VESTED_DAY_511: u64 = 9_500_000_000_000_000;

/// Day 541 (`p = 18 = s + M − 1`): `months = min(18 − 1 + 1, 18) = 18 = M` ⇒ the
/// terminal branch returns EXACTLY `reference_amount` = 10_000_000_000_000_000
/// (100%). The same value holds at any later `now` (`months` is capped by
/// `min(·, M)`), `now = i64::MAX` included.
pub const V2_VESTED_DAY_541: u64 = 10_000_000_000_000_000;

// ---------- V3: the cliff, the day 270 / day 271 boundary (Treasury) ----------
//
// `reference_amount = 40,000,000 $1WIN`, `tge_bps = 0`, `monthly_bps = 400`,
// `cliff = 9`; `s = 9`, `M = 25` (see TREASURY_*).

/// `reference_amount = 40 000 000 $1WIN` = 4×10⁷ × 10⁸ = 4×10¹⁵
/// = 4_000_000_000_000_000 token-units.
pub const V3_REFERENCE: u64 = 4_000_000_000_000_000;

/// `tge_amt = floor(4×10¹⁵ × 0 / 10000) = 0` (`tge_bps = 0`).
pub const V3_TGE_AMT: u64 = 0;

/// `monthly_amt = floor(4×10¹⁵ × 400 / 10000) = floor(1.6×10¹⁸ / 10⁴) = 1.6×10¹⁴`
/// = 160_000_000_000_000 (4% = 1,600,000 $1WIN), with no remainder.
pub const V3_MONTHLY_AMT: u64 = 160_000_000_000_000;

/// Day 270 (269 days have passed: `p = floor(269 × 86_400 / 2_592_000) =
/// floor(269 / 30) = 8`): `p < s = 9` ⇒ `months = 0` ⇒ `vested = tge_amt = 0`.
/// Throughout the cliff nothing is unlocked.
pub const V3_VESTED_DAY_270: u64 = 0;

/// Day 271 (270 days have passed: `p = floor(270 / 30) = 9 = s`): `months =
/// min(9 − 9 + 1, 25) = 1` ⇒ `vested = 0 + 1 × 1.6×10¹⁴ = 160_000_000_000_000`
/// (4% = 1,600,000 $1WIN) - the first monthly tranche lands exactly on day 271.
pub const V3_VESTED_DAY_271: u64 = 160_000_000_000_000;

/// Day 961 (`p = 32 = s + M − 2`, the pre-terminal period): `months =
/// min(32 − 9 + 1, 25) = 24 = M − 1` ⇒ `vested = 24 × 1.6×10¹⁴ = 3.84×10¹⁵`
/// = 3_840_000_000_000_000 (96%, strictly below the reference). It holds until the
/// last second before day 991.
pub const V3_VESTED_DAY_961: u64 = 3_840_000_000_000_000;

/// Day 991 (`p = 33 = s + M − 1`): `months = min(33 − 9 + 1, 25) = 25 = M` ⇒ the
/// terminal branch returns EXACTLY `reference_amount` = 4_000_000_000_000_000 (100%).
pub const V3_VESTED_DAY_991: u64 = 4_000_000_000_000_000;

// ---------- V4: flooring remainders and the top-up by the last tranche (Community
// Rewards) ----------
//
// A degenerate scale for clarity: `reference_amount = 1000` token units,
// `tge_bps = 0`, `monthly_bps = 834`, `cliff = 0`; `s = 1`, `M = 12` (see COMMUNITY_*).

/// `reference_amount = 1000` token units (0.00001 $1WIN - the scale was chosen so
/// that the flooring loss is visible in whole units).
pub const V4_REFERENCE: u64 = 1_000;

/// `tge_amt = floor(1000 × 0 / 10000) = 0`.
pub const V4_TGE_AMT: u64 = 0;

/// `monthly_amt = floor(1000 × 834 / 10000) = floor(834_000 / 10_000) = floor(83.4) =
/// 83` - each tranche loses 0.4 unit.
pub const V4_MONTHLY_AMT: u64 = 83;

/// 11 months (`p = 11`, day `1 + 30 × 11 = 331`): `months = min(11 − 1 + 1, 12) =
/// 11 < M` ⇒ `vested = 0 + 11 × 83 = 913` - NOT 1000 (the pre-terminal period, where
/// the flooring has not been topped up yet).
pub const V4_VESTED_MONTHS_11: u64 = 913;

/// 12 months (`p = 12 = s + M − 1`, day `1 + 30 × 12 = 361`): `months =
/// min(12 − 1 + 1, 12) = 12 = M` ⇒ the terminal branch returns EXACTLY
/// `reference_amount = 1000`.
pub const V4_VESTED_MONTHS_12: u64 = 1_000;

/// The last tranche = `vested(12 months) − vested(11 months) = 1000 − 913 = 87` token
/// units - MORE than the nominal 83. The breakdown: 12 nominal tranches of 83 would
/// give `12 × 83 = 996 < 1000`; the terminal branch tops up the flooring shortfall
/// `1000 − 996 = 4` units, so the 12th (terminal) tranche = `83 + 4 = 87`.
pub const V4_LAST_TRANCHE: u64 = 87;

/// The naive sum `13 × 83 = 1079` (one tranche past the terminal one) is
/// UNREACHABLE: beyond `M = 12` the `min(months, M)` holds `months` at 12 and the
/// terminal branch holds the payout at `reference = 1000` (the ceiling of §10.3). The
/// reference value for the negative assertion "vested never equals 1079 and never
/// exceeds 1000".
pub const V4_NAIVE_SUM_UNREACHABLE: u64 = 1_079;

// ---------- V5: accrual across skipped periods (Ecosystem) ----------
//
// The parameters of V2. The recipient does not claim until day 151 and then claims
// on day 541.

/// After the day-151 claim: `claimed = vested(151)`. With `claimed = 0` before the
/// claim, `claimable = vested − claimed = vested(151)`, that is, the same value as
/// [`V2_VESTED_DAY_151`] - there is no separate constant for it, or a typo in one of
/// the two would slip through unnoticed.
pub const V5_CLAIMED_AFTER_DAY_151: u64 = V2_VESTED_DAY_151;

/// Day 541, `claimed = 3.5×10¹⁵`: `claimable = 10¹⁶ − 3.5×10¹⁵ = 6.5×10¹⁵`
/// = 6_500_000_000_000_000 - the second claim releases the remainder.
pub const V5_CLAIMABLE_DAY_541: u64 = 6_500_000_000_000_000;

/// After the second claim: `claimed = 3.5×10¹⁵ + 6.5×10¹⁵ = 10¹⁶`
/// = 10_000_000_000_000_000 == `reference_amount` ⇒ the activity is closable
/// (`close_vesting`, I9).
pub const V5_CLAIMED_AFTER_DAY_541: u64 = 10_000_000_000_000_000;

// ---------- Liquidity: the amounts at a reference of 100,000,000 $1WIN ----------
//
// §10.5 fixes the Liquidity schedule but not its `reference_amount`, and the
// architecture carries no numeric example of its own for it. The reference is taken
// to be the same as in V2 (100,000,000 $1WIN = 10¹⁶ units) so that the test
// does not introduce a second funding amount. The shares come from LIQUIDITY_*
// (1000 / 750 / cliff 0, s = 1, M = 12).

/// `reference_amount` = 10¹⁶ token units (the same as V2_REFERENCE).
pub const LIQUIDITY_REFERENCE: u64 = 10_000_000_000_000_000;

/// `tge_amt = floor(10¹⁶ × 1000 / 10000) = 10¹⁵ = 1_000_000_000_000_000` (10%).
pub const LIQUIDITY_TGE_AMT: u64 = 1_000_000_000_000_000;

/// `monthly_amt = floor(10¹⁶ × 750 / 10000) = floor(7.5×10¹⁸ / 10⁴) = 7.5×10¹⁴`
/// = 750_000_000_000_000 (7.5%), with no remainder.
pub const LIQUIDITY_MONTHLY_AMT: u64 = 750_000_000_000_000;

/// Day 31 (`p = 1 = s`): `months = min(1 − 1 + 1, 12) = 1` ⇒
/// `vested = 10¹⁵ + 7.5×10¹⁴ = 1.75×10¹⁵ = 1_750_000_000_000_000` (17.5%).
pub const LIQUIDITY_VESTED_DAY_31: u64 = 1_750_000_000_000_000;

/// Day 331 (`p = 11 = s + M − 2`): `months = min(11 − 1 + 1, 12) = 11 = M − 1` ⇒
/// `vested = 10¹⁵ + 11 × 7.5×10¹⁴ = 10¹⁵ + 8.25×10¹⁵ = 9.25×10¹⁵`
/// = 9_250_000_000_000_000 (92.5%, strictly below the reference). It holds until the
/// last second before day 361.
pub const LIQUIDITY_VESTED_DAY_331: u64 = 9_250_000_000_000_000;

/// Day 361 (`p = 12 = s + M − 1`): `months = 12 = M` ⇒ the terminal branch returns
/// EXACTLY `reference_amount` = 10_000_000_000_000_000.
pub const LIQUIDITY_VESTED_DAY_361: u64 = 10_000_000_000_000_000;

// ---------- The "cliff + TGE share" combination ----------
//
// §10.5 has no schedule with `cliff > 0` and `tge_bps > 0` at once, but the
// validation of §9.5 permits that combination, and in it the `p < s` branch returns
// `tge_amt` rather than 0. The schedule was assembled for this test: V2's reference
// (10¹⁶) and Treasury's cliff (9 months) at `tge_bps = 1000`, `monthly_bps = 400`.

/// `tge_bps = 1000` = 10% at the TGE instant.
pub const MIXED_TGE_BPS: u16 = 1_000;

/// `monthly_bps = 400` = 4% per month.
pub const MIXED_MONTHLY_BPS: u16 = 400;

/// A 9-month cliff ⇒ `s = max(9, 1) = 9`.
pub const MIXED_CLIFF_MONTHS: u16 = 9;

/// `M = ceil((10000 − 1000) / 400) = ceil(9000 / 400) = ceil(22.5) = 23`
/// (`400 × 22 = 8800 < 9000`, `400 × 23 = 9200 ≥ 9000`). The schedule's ceiling:
/// `cliff + M = 9 + 23 = 32 ≤ MAX_SCHEDULE_MONTHS = 120`.
pub const MIXED_TRANCHES_M: u16 = 23;

/// `reference_amount` = 10¹⁶ token units (the same as V2_REFERENCE).
pub const MIXED_REFERENCE: u64 = 10_000_000_000_000_000;

/// `tge_amt = floor(10¹⁶ × 1000 / 10000) = 10¹⁵ = 1_000_000_000_000_000`.
pub const MIXED_TGE_AMT: u64 = 1_000_000_000_000_000;

/// `monthly_amt = floor(10¹⁶ × 400 / 10000) = floor(4×10¹⁸ / 10⁴) = 4×10¹⁴`
/// = 400_000_000_000_000.
pub const MIXED_MONTHLY_AMT: u64 = 400_000_000_000_000;

/// Day 270 (`p = 8 < s = 9`, inside the cliff): `months = 0` ⇒
/// `vested = tge_amt = 1_000_000_000_000_000` - NOT 0: the TGE share is released
/// while the monthly ones are not yet.
pub const MIXED_VESTED_DAY_270: u64 = 1_000_000_000_000_000;

/// Day 271 (`p = 9 = s`): `months = min(9 − 9 + 1, 23) = 1` ⇒
/// `vested = 10¹⁵ + 4×10¹⁴ = 1.4×10¹⁵ = 1_400_000_000_000_000` (14%).
pub const MIXED_VESTED_DAY_271: u64 = 1_400_000_000_000_000;

/// The full release: period `s + M − 1 = 9 + 23 − 1 = 31`, day
/// `1 + 30 × 31 = 1 + 930 = 931`.
pub const MIXED_FULL_RELEASE_DAY: i64 = 931;

// ---------- The "stored state → schedule" adapter ----------
//
// A dedicated schedule for checking `Vesting::schedule()`: all four fields are
// pairwise distinguishable (`4×10¹⁵` / `1000` / `500` / `9`), so swapping any two of
// them in the adapter shows up both in the fields themselves and in the unlock.
// `reference_amount` is the Treasury bucket's reference (V3_REFERENCE), while the
// shares and the cliff were assembled for this check: `s = max(9, 1) = 9`,
// `M = ceil((10000 − 1000) / 500) = ceil(18.0) = 18`, and the ceiling
// `cliff + M = 9 + 18 = 27 ≤ MAX_SCHEDULE_MONTHS = 120`.

/// `tge_bps = 1000` = 10% at the TGE instant.
pub const BRIDGE_TGE_BPS: u16 = 1_000;

/// `monthly_bps = 500` = 5% per month.
pub const BRIDGE_MONTHLY_BPS: u16 = 500;

/// A 9-month cliff ⇒ `s = 9`.
pub const BRIDGE_CLIFF_MONTHS: u16 = 9;

/// Day 301 (`p = floor(300 / 30) = 10`, the second monthly tranche): `months =
/// min(10 − 9 + 1, 18) = 2` ⇒ `vested = tge_amt + 2 × monthly_amt`, where
/// `tge_amt = floor(4×10¹⁵ × 1000 / 10000) = 4×10¹⁴` and
/// `monthly_amt = floor(4×10¹⁵ × 500 / 10000) = 2×10¹⁴`; in total
/// `4×10¹⁴ + 2 × 2×10¹⁴ = 8×10¹⁴ = 800_000_000_000_000` (20% of the reference). The
/// second tranche is used rather than the first: at the first (`months = 1`) the sum
/// `tge_amt + monthly_amt` is symmetric in the shares and would not tell a
/// `tge_bps` ↔ `monthly_bps` swap apart.
pub const BRIDGE_VESTED_DAY_301: u64 = 800_000_000_000_000;

// ---------- Domain edges ----------

/// `M` at `(tge_bps, monthly_bps) = (9999, 1)`:
/// `ceil((10000 − 9999) / 1) = ceil(1 / 1) = 1` - a single tranche tops up
/// everything.
pub const EDGE_M_BPS_9999_1: u16 = 1;

/// `M` at `(0, 1)`: `ceil(10000 / 1) = 10000` - the longest schedule the formula can
/// express at all (the validation of §9.5 rejects it by the
/// `MAX_SCHEDULE_MONTHS = 120` ceiling, but `vested()` is total on it too).
pub const EDGE_M_BPS_0_1: u16 = 10_000;

/// The number of monthly tranches at the pre-terminal period of the `(0, 1)` schedule:
/// `M − 1 = 10000 − 1 = 9999`. The tightest margin of the `months × monthly_amt`
/// product in u128 (§10.4).
pub const EDGE_MONTHS_BPS_0_1: u16 = 9_999;

/// `M` at `monthly_bps = 10000` (and `tge_bps = 0`): `ceil(10000 / 10000) = 1`.
pub const EDGE_M_MONTHLY_10000: u16 = 1;

/// `u64::MAX` = 18_446_744_073_709_551_615 - the largest `reference_amount` that fits
/// into the field at all (there is no business ceiling).
pub const EDGE_MAX_REFERENCE: u64 = 18_446_744_073_709_551_615;

/// The maximum intermediate product of §10.4: `u64::MAX × 10000` =
/// 18_446_744_073_709_551_615 × 10_000 = 184_467_440_737_095_516_150_000
/// ≈ 1.845×10²³. Against `u128::MAX ≈ 3.4×10³⁸` that leaves ~15 orders of magnitude
/// of headroom, so the intermediate never overflows u128 on any valid input.
pub const EDGE_MAX_PRODUCT_U128: u128 = 184_467_440_737_095_516_150_000;

/// `tge_amt` at `reference_amount = u64::MAX`, `tge_bps = 9999`:
/// `floor(18_446_744_073_709_551_615 × 9999 / 10000)`.
/// The numerator: `MAX × 10000 − MAX = 184_467_440_737_095_516_150_000 −
/// 18_446_744_073_709_551_615 = 184_448_993_993_021_806_598_385`.
/// The division: `184_448_993_993_021_806_598_385 / 10_000 =
/// 18_444_899_399_302_180_659` with a remainder of `8_385` (check:
/// `18_444_899_399_302_180_659 × 10_000 = 184_448_993_993_021_806_590_000`,
/// `+ 8_385` gives the numerator).
pub const EDGE_MAX_REF_TGE_AMT_9999: u64 = 18_444_899_399_302_180_659;

/// `monthly_amt` at `reference_amount = u64::MAX`, `monthly_bps = 1`:
/// `floor(18_446_744_073_709_551_615 / 10_000) = 1_844_674_407_370_955` with a
/// remainder of `1_615` (check: `1_844_674_407_370_955 × 10_000 =
/// 18_446_744_073_709_550_000`, `+ 1_615` gives `u64::MAX`).
pub const EDGE_MAX_REF_MONTHLY_AMT_1: u64 = 1_844_674_407_370_955;

/// The naive sum of the `(9999, 1)` schedule's shares at
/// `reference_amount = u64::MAX`: `18_444_899_399_302_180_659 +
/// 1_844_674_407_370_955 = 18_446_744_073_709_551_614 = u64::MAX − 1` - the flooring
/// ate exactly 1 unit. The terminal branch (`months == M = 1`) returns `u64::MAX`,
/// that is 1 unit above the naive sum: the top-up is visible even at the extreme
/// scale.
pub const EDGE_MAX_REF_NAIVE_SUM: u64 = 18_446_744_073_709_551_614;

/// The unlock at the pre-terminal period of the `(0, 1)` schedule at
/// `reference_amount = u64::MAX`: `tge_amt = 0`, `months = 9999`
/// (EDGE_MONTHS_BPS_0_1), `monthly_amt = 1_844_674_407_370_955`
/// (EDGE_MAX_REF_MONTHLY_AMT_1). The product:
/// `1_844_674_407_370_955 × 9999 = 1_844_674_407_370_955 × 10_000 −
/// 1_844_674_407_370_955 = 18_446_744_073_709_550_000 − 1_844_674_407_370_955 =
/// 18_444_899_399_302_179_045`. That leaves
/// `18_446_744_073_709_551_615 − 18_444_899_399_302_179_045 =
/// 1_844_674_407_372_570` to `u64::MAX` - the tightest margin at which the
/// u128 → u64 downcast still passes (§10.4).
pub const EDGE_MAX_REF_VESTED_MONTHS_9999: u64 = 18_444_899_399_302_179_045;

/// `monthly_amt` at `reference_amount = 1`, `monthly_bps = 500`:
/// `floor(1 × 500 / 10000) = floor(0.05) = 0` - the monthly tranches are zero and the
/// entire payout arrives through the terminal branch.
pub const EDGE_TINY_MONTHLY_AMT: u64 = 0;

/// `M` for the "a reference of 1 unit" edge: `ceil(10000 / 500) = 20`
/// (10000 = 20 × 500).
pub const EDGE_TINY_M: u16 = 20;

/// The terminal period of the "a reference of 1 unit" edge (`cliff = 0` ⇒ `s = 1`):
/// `s + M − 1 = 1 + 20 − 1 = 20`, day `1 + 30 × 20 = 601`. There `vested = 1` (the
/// whole reference); at any earlier period it is 0.
pub const EDGE_TINY_TERMINAL_DAY: i64 = 601;
