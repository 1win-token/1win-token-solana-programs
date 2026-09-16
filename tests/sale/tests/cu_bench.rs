//! A CU / tx-size benchmark (R6, M2 item 9): measurements of LiteSVM's
//! `compute_units_consumed` and of the bincode size of the close batches' legacy
//! transactions. The thresholds are a regression gate; the tables come from
//! `cargo test cu_bench -- --nocapture`.
//!
//! CU are deterministic given fixed keys, but the PDA/ATA bump search costs 1500 CU
//! per iteration and depends on the test's random keys - the measurement jitters in
//! multiples of 1500 (up to ±13,500 was observed on purchase/claim). The ceilings are
//! therefore ~2× the measurement: they catch structural growth (an extra CPI, a loop),
//! not jitter.
//!
//! LiteSVM does NOT enforce the 1232 B packet limit, but the runtime's cost model
//! keeps program-id-index < 38 (`FILTER_SIZE = 1232 / 32`) - a legacy tx with > 38
//! static keys (≥ 18 distinct pairs) will not execute in SVM either. So the size is
//! measured by bincode-serializing the signed transaction against PACKET_DATA_SIZE,
//! while the CU come from execution: legacy up to 17 distinct pairs, and the full
//! batch of 20 as a v0 tx with an address lookup table (the ALT is forged through
//! `set_account`, like `forge_program_account`: creating the table honestly is not
//! needed in a benchmark).

use sale_test_common::{
    activate_claim_ix, aggregate_pda, claim_ix, close_aggregates_ix, close_deposits_ix,
    deposit_pda, deposit_vault_pda, expected::DEFAULT_PRICE_USDC, init_aggregate_ix, initialize_ix,
    purchase_ix, refund_ix, refund_vault_pda, sweep_deposit_vault_ix, sweep_refund_vault_ix,
    sweep_token_vault_ix, token_vault_pda, withdraw_proceeds_ix, InitParams, TestEnv,
};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{v0, AddressLookupTableAccount, Message, VersionedMessage};
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

// ----- The economics (the same scale as claim.rs) -----

const PRICE: u64 = DEFAULT_PRICE_USDC;
/// An emission of 1000 $1WIN → a soft cap of $30: oversubscription is reachable with
/// a couple of purchases.
const EMISSION: u64 = 100_000_000_000;
const OVER_DEP: u64 = 20_000_000;
const UNDER_DEP: u64 = 10_000_000;
/// An undersubscribed distributable with room to spare (its exact value is irrelevant
/// to CU).
const UNDER_DIST: u64 = 66_666_666_666;

// ----- The execution environment's limits -----

/// The transaction packet limit (IPv6 MTU 1280 − 40 − 8; solana-packet).
const PACKET_DATA_SIZE: u64 = 1232;
/// The maximum CU budget of a single transaction.
const MAX_TX_CU: u64 = 1_400_000;
/// The mainnet block CU budget.
const BLOCK_CU: u64 = 48_000_000;

// ----- The CU ceilings (measured 2026-08-07 × ~2 for bump-search jitter) -----

const CEIL_PURCHASE: u64 = 100_000; // measured 33-44k
const CEIL_CLAIM: u64 = 150_000; // measured 51-87k
const CEIL_ACTIVATE: u64 = 110_000; // measured 37-58k
const CEIL_REFUND: u64 = 110_000; // measured 40-56k (the refund account's init included)
const CEIL_WITHDRAW: u64 = 110_000; // measured 56-66k
const CEIL_SWEEP: u64 = 120_000; // measured 27-61k
/// A batch of MAX_CLOSE_PER_BATCH = 20 pairs (measured 71-110k).
///
/// Every pair is checked by the canonical `find_program_address` against the record's
/// body rather than by a stored bump, so a pair's cost depends on the wallet's
/// address: 1500 CU per search iteration, 2 iterations on average, with a tail up to
/// 9. Across 20 pairs the spread reaches ±30,000 CU. The ceiling is ~2× the
/// measurement, as with the rest: it catches structural growth (an extra CPI, a
/// nested loop) rather than the search's jitter. On mainnet the practical batch is 14
/// pairs (the tx packet limit, see `tx_size_close_batch_legacy`), so 20 pairs here is
/// a synthetic maximum.
const CEIL_CLOSE_20: u64 = 190_000;

#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> litesvm::types::TransactionResult {
    env.expire_blockhash();
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

fn cu_of(r: &litesvm::types::TransactionResult, what: &str) -> u64 {
    r.as_ref()
        .unwrap_or_else(|e| panic!("{what}: the bench tx must pass: {e:?}"))
        .compute_units_consumed
}

fn row(name: &str, cu: u64, ceiling: u64) {
    println!("| {name:<42} | {cu:>7} | < {ceiling:>7} |");
    assert!(cu > 0, "{name}: a zero CU measurement");
    assert!(cu < ceiling, "{name}: CU {cu} ≥ the ceiling {ceiling}");
}

fn funded_ata(env: &mut TestEnv, wallet: &Pubkey, amount: u64) -> Pubkey {
    let usdc = env.usdc_mint;
    let ata = env.create_ata(wallet, &usdc);
    env.mint_to(&usdc, &ata, amount);
    ata
}

fn init_sale(env: &mut TestEnv) -> InitParams {
    let mut p = env.valid_init_params();
    p.price_usdc = PRICE;
    p.emission = EMISSION;
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    send(env, initialize_ix(owner.pubkey(), usdc, win, &p), &owner).expect("initialize");
    p
}

fn fund_token_vault(env: &mut TestEnv, amount: u64) {
    let win = env.win_mint;
    env.mint_to(&win, &token_vault_pda(), amount);
}

// ===================================================================
// The CU of individual instructions: the deposit path → oversubscription →
// claim(refund>0) → withdraw_proceeds → sweep ×3. Plus the ceiling of purchases per
// block.
// ===================================================================

#[test]
fn cu_bench_purchase_claim_over_withdraw_sweeps() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let win = env.win_mint;
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = init_sale(&mut env);
    env.warp_to(p.sale_start_date);

    println!("== CU of individual instructions (oversubscribed) ==");

    // A: two deposits ($20 + $20) - the first and a repeated purchase.
    let ata_a = funded_ata(&mut env, &a.pubkey(), 2 * OVER_DEP);
    send(&mut env, init_aggregate_ix(a.pubkey()), &a).expect("init_aggregate A");
    let r = send(&mut env, purchase_ix(a.pubkey(), ata_a, 0, OVER_DEP), &a);
    row(
        "purchase (a wallet's first deposit)",
        cu_of(&r, "purchase#0"),
        CEIL_PURCHASE,
    );
    let r = send(&mut env, purchase_ix(a.pubkey(), ata_a, 1, OVER_DEP), &a);
    let cu_hot = cu_of(&r, "purchase#1");
    row(
        "purchase (a repeat, with a warm SaleState)",
        cu_hot,
        CEIL_PURCHASE,
    );

    // B: one deposit ($20). A pool of $60 > the soft cap of $30 → oversubscribed.
    let ata_b = funded_ata(&mut env, &b.pubkey(), OVER_DEP);
    send(&mut env, init_aggregate_ix(b.pubkey()), &b).expect("init_aggregate B");
    send(&mut env, purchase_ix(b.pubkey(), ata_b, 0, OVER_DEP), &b).expect("purchase B");

    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, EMISSION);
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        &admin,
    );
    row(
        "activate_claim (oversubscribed)",
        cu_of(&r, "activate over"),
        CEIL_ACTIVATE,
    );

    env.warp_to(p.claim_start_date);
    // The wallet has no $1WIN ATA → init_if_needed creates it inside claim.
    let r = send(&mut env, claim_ix(a.pubkey(), usdc, win, true), &a);
    row(
        "claim (refund > 0, plus the $1WIN ATA)",
        cu_of(&r, "claim over"),
        CEIL_CLAIM,
    );

    let r = send(
        &mut env,
        withdraw_proceeds_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    row("withdraw_proceeds", cu_of(&r, "withdraw"), CEIL_WITHDRAW);

    // ClaimClosed; deposit_vault holds a stray transfer (the sweep scenario).
    env.warp_to(p.claim_start_date + p.claim_duration);
    env.mint_to(&usdc, &deposit_vault_pda(), 123);
    let r = send(
        &mut env,
        sweep_refund_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    row("sweep_refund_vault", cu_of(&r, "sweep refund"), CEIL_SWEEP);
    let r = send(
        &mut env,
        sweep_deposit_vault_ix(owner.pubkey(), usdc, owner.pubkey()),
        &owner,
    );
    row(
        "sweep_deposit_vault",
        cu_of(&r, "sweep deposit"),
        CEIL_SWEEP,
    );
    let r = send(
        &mut env,
        sweep_token_vault_ix(owner.pubkey(), win, owner.pubkey()),
        &owner,
    );
    row("sweep_token_vault", cu_of(&r, "sweep token"), CEIL_SWEEP);

    println!(
        "the theoretical ceiling of purchases per block: {BLOCK_CU} / {cu_hot} = {}",
        BLOCK_CU / cu_hot
    );
}

// ===================================================================
// The CU of refund plus the undersubscribed branches: activate_claim(None),
// claim(refund == 0)
// ===================================================================

#[test]
fn cu_bench_refund_and_undersubscribed_branches() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let win = env.win_mint;
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = init_sale(&mut env);
    env.warp_to(p.sale_start_date);

    println!("== CU of individual instructions (undersubscribed) ==");

    for w in [&a, &b] {
        let ata = funded_ata(&mut env, &w.pubkey(), UNDER_DEP);
        send(&mut env, init_aggregate_ix(w.pubkey()), w).expect("init_aggregate");
        send(&mut env, purchase_ix(w.pubkey(), ata, 0, UNDER_DEP), w).expect("purchase");
    }

    // The AML refund of B's deposit during the Sale phase.
    let r = send(
        &mut env,
        refund_ix(operator.pubkey(), b.pubkey(), 0, usdc),
        &operator,
    );
    row("refund", cu_of(&r, "refund"), CEIL_REFUND);

    // After the refund the pool is $10 ≤ the soft cap of $30 → undersubscribed, and
    // refund_vault is not created.
    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, UNDER_DIST);
    let r = send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, None),
        &admin,
    );
    row(
        "activate_claim (undersubscribed)",
        cu_of(&r, "activate under"),
        CEIL_ACTIVATE,
    );

    env.warp_to(p.claim_start_date);
    let r = send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a);
    row(
        "claim (refund == 0, plus the $1WIN ATA)",
        cu_of(&r, "claim under"),
        CEIL_CLAIM,
    );
}

// ===================================================================
// The CU of close_deposits / close_aggregates at N pairs = 1, 5, 10, 17, 20.
// N ≤ 17 runs as a legacy tx (it fits the cost model's 38-static-key limit); N = 20
// distinct pairs (44 keys) requires a v0 tx plus an ALT. Additionally, a legacy
// profile of the full batch: 20 deposits of ONE wallet (25 keys).
// ===================================================================

/// The partitions of distinct pairs: up to 17 as legacy, 20 as v0 + ALT.
const BATCH_NS_LEGACY: [usize; 4] = [1, 5, 10, 17];
const BATCH_N_V0: usize = 20;

#[test]
fn cu_bench_close_batches() {
    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let p = init_sale(&mut env);
    env.warp_to(p.sale_start_date);

    // 53 wallets × $10 (the partitions 1+5+10+17+20) plus wallet X with 20 deposits: a
    // pool of $730 → oversubscribed.
    let total: usize = BATCH_NS_LEGACY.iter().sum::<usize>() + BATCH_N_V0;
    let mut wallets: Vec<Keypair> = Vec::with_capacity(total);
    for _ in 0..total {
        let w = env.new_user();
        let ata = funded_ata(&mut env, &w.pubkey(), UNDER_DEP);
        send(&mut env, init_aggregate_ix(w.pubkey()), &w).expect("init_aggregate");
        send(&mut env, purchase_ix(w.pubkey(), ata, 0, UNDER_DEP), &w).expect("purchase");
        wallets.push(w);
    }
    let x = env.new_user();
    let ata_x = funded_ata(&mut env, &x.pubkey(), BATCH_N_V0 as u64 * UNDER_DEP);
    send(&mut env, init_aggregate_ix(x.pubkey()), &x).expect("init_aggregate X");
    for i in 0..BATCH_N_V0 as u64 {
        send(&mut env, purchase_ix(x.pubkey(), ata_x, i, UNDER_DEP), &x).expect("purchase X");
    }

    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, EMISSION);
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        &admin,
    )
    .expect("activate_claim");

    // Both closes become available only past the claim deadline (ClaimClosed).
    env.warp_to(p.claim_start_date + p.claim_duration);

    println!("== CU close_deposits ==");
    let dep_cu = close_partitions(
        &mut env,
        &wallets,
        |w| (deposit_pda(&w.pubkey(), 0), w.pubkey()),
        |pairs| close_deposits_ix(operator.pubkey(), pairs),
        &operator,
    );
    // The full batch of a single wallet - a legacy profile that is executable on
    // mainnet.
    let same_pairs: Vec<(Pubkey, Pubkey)> = (0..BATCH_N_V0 as u64)
        .map(|i| (deposit_pda(&x.pubkey(), i), x.pubkey()))
        .collect();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &same_pairs),
        &operator,
    );
    let cu_same = cu_of(&r, "close_deposits same-wallet");
    println!("| N = 20 pairs of one wallet (legacy) | {cu_same:>7} CU |");
    assert!(
        cu_same < CEIL_CLOSE_20,
        "same-wallet 20 pairs ≥ the ceiling"
    );

    println!("== CU close_aggregates ==");
    let agg_cu = close_partitions(
        &mut env,
        &wallets,
        |w| (aggregate_pda(&w.pubkey()), w.pubkey()),
        |pairs| close_aggregates_ix(operator.pubkey(), pairs),
        &operator,
    );

    for (name, cus) in [("close_deposits", dep_cu), ("close_aggregates", agg_cu)] {
        let (cu_1, cu_20) = (cus[0], cus[cus.len() - 1]);
        assert!(cu_20 > cu_1, "{name}: the CU do not grow with the batch");
        assert!(
            cu_20 < CEIL_CLOSE_20,
            "{name}: 20 pairs {cu_20} ≥ {CEIL_CLOSE_20}"
        );
        // A linear extrapolation: how many pairs fit into 1.4M CU (a v0 tx + ALT).
        let slope = (cu_20 - cu_1) / 19;
        let base = cu_1 - slope;
        println!(
            "{name}: ~{slope} CU/pair, so ~{} pairs fit into {MAX_TX_CU} CU",
            (MAX_TX_CU - base) / slope
        );
    }
}

/// Closes the partitions: [1, 5, 10, 17] in a legacy tx and [20] in a v0 tx + ALT.
/// Prints and returns the CU in partition order.
fn close_partitions(
    env: &mut TestEnv,
    wallets: &[Keypair],
    pair_of: impl Fn(&Keypair) -> (Pubkey, Pubkey),
    ix_of: impl Fn(&[(Pubkey, Pubkey)]) -> Instruction,
    operator: &Keypair,
) -> Vec<u64> {
    let mut cus = Vec::new();
    let mut offset = 0;
    for n in BATCH_NS_LEGACY {
        let pairs: Vec<(Pubkey, Pubkey)> =
            wallets[offset..offset + n].iter().map(&pair_of).collect();
        offset += n;
        let r = send(env, ix_of(&pairs), operator);
        let cu = cu_of(&r, "the close batch (legacy)");
        println!("| N = {n:>2} pairs (legacy)  | {cu:>7} CU |");
        cus.push(cu);
    }
    let pairs: Vec<(Pubkey, Pubkey)> = wallets[offset..offset + BATCH_N_V0]
        .iter()
        .map(&pair_of)
        .collect();
    let (r, tx_size) = send_v0_with_alt(env, ix_of(&pairs), operator, &pairs);
    let cu = cu_of(&r, "the close batch (v0+ALT)");
    println!("| N = {BATCH_N_V0} pairs (v0+ALT)  | {cu:>7} CU | tx {tx_size} B |");
    cus.push(cu);
    cus
}

/// Sends an instruction as a v0 transaction: all the pairs' keys go through a forged
/// ALT (`deactivation_slot = u64::MAX`, `last_extended_slot = 0` - active). Returns
/// the result and the v0 tx's bincode size.
fn send_v0_with_alt(
    env: &mut TestEnv,
    ix: Instruction,
    payer: &Keypair,
    pairs: &[(Pubkey, Pubkey)],
) -> (litesvm::types::TransactionResult, u64) {
    let addresses: Vec<Pubkey> = pairs.iter().flat_map(|(a, w)| [*a, *w]).collect();

    // The ALT state layout: bincode(ProgramState::LookupTable(meta)) in the first 56
    // bytes (LOOKUP_TABLE_META_SIZE), followed by the raw addresses at 32 bytes each.
    let mut data = vec![0u8; 56 + 32 * addresses.len()];
    data[0] = 1; // the ProgramState::LookupTable discriminant
    data[4..12].copy_from_slice(&u64::MAX.to_le_bytes()); // deactivation_slot
    for (i, addr) in addresses.iter().enumerate() {
        data[56 + 32 * i..56 + 32 * (i + 1)].copy_from_slice(addr.as_ref());
    }
    let table_key = Pubkey::new_unique();
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            table_key,
            solana_account::Account {
                lamports,
                data,
                owner: solana_sdk_ids::address_lookup_table::id(),
                executable: false,
                rent_epoch: 0,
            },
        )
        .expect("set_account(ALT)");

    let alt = AddressLookupTableAccount {
        key: table_key,
        addresses,
    };
    env.expire_blockhash();
    let msg = v0::Message::try_compile(&payer.pubkey(), &[ix], &[alt], env.svm.latest_blockhash())
        .expect("compile v0");
    let tx = VersionedTransaction::try_new(VersionedMessage::V0(msg), &[payer])
        .expect("signing the v0 tx");
    let tx_size = bincode::serialized_size(&tx).expect("the bincode size of the v0 tx");
    (env.svm.send_transaction(tx), tx_size)
}

// ===================================================================
// The legacy tx size of a close batch: the actual maximum number of pairs within
// 1232 B. Distinct pairs cost 2 new keys (64 B); pairs of one wallet cost +1 key
// (32 B).
// ===================================================================

/// The bincode size of a signed legacy tx of `close_deposits(pairs)` (plus,
/// optionally, two ComputeBudget instructions - the real mainnet sending profile).
fn close_tx_size(
    env: &TestEnv,
    operator: &Keypair,
    pairs: &[(Pubkey, Pubkey)],
    with_budget: bool,
) -> u64 {
    let mut ixs = Vec::new();
    if with_budget {
        let cb = solana_sdk_ids::compute_budget::id();
        // SetComputeUnitLimit(u32) = tag 2, SetComputeUnitPrice(u64) = tag 3.
        ixs.push(Instruction::new_with_bytes(cb, &[2, 0, 0, 0, 0], vec![]));
        ixs.push(Instruction::new_with_bytes(
            cb,
            &[3, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![],
        ));
    }
    ixs.push(close_deposits_ix(operator.pubkey(), pairs));
    let msg =
        Message::new_with_blockhash(&ixs, Some(&operator.pubkey()), &env.svm.latest_blockhash());
    let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[operator])
        .expect("signing the bench tx");
    bincode::serialized_size(&tx).expect("the tx's bincode size")
}

/// The largest N in 1..=30 for which a tx with `pairs_of(N)` fits into 1232 B.
fn max_fitting(
    env: &TestEnv,
    operator: &Keypair,
    with_budget: bool,
    pairs_of: impl Fn(usize) -> Vec<(Pubkey, Pubkey)>,
) -> usize {
    let mut max_fit = 0;
    for n in 1..=30 {
        let size = close_tx_size(env, operator, &pairs_of(n), with_budget);
        let fits = size <= PACKET_DATA_SIZE;
        println!(
            "| N = {n:>2} pairs | {size:>4} B | {} |",
            if fits { "fits" } else { "does NOT fit" }
        );
        if fits {
            max_fit = n;
        }
    }
    max_fit
}

#[test]
fn tx_size_close_batch_legacy() {
    let env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();

    // Distinct pairs: every (account, wallet) is two new pubkeys.
    println!("== the tx size of a close batch, DISTINCT pairs (legacy) ==");
    let distinct = |n: usize| -> Vec<(Pubkey, Pubkey)> {
        (0..n)
            .map(|_| (Pubkey::new_unique(), Pubkey::new_unique()))
            .collect()
    };
    let max_distinct = max_fitting(&env, &operator, false, distinct);
    assert_eq!(
        max_distinct, 14,
        "the maximum number of distinct pairs in a legacy tx"
    );

    // The same wallet with different deposits: the wallet is deduplicated - +32 B per
    // pair.
    println!("== the tx size of a close batch, pairs of ONE wallet (legacy) ==");
    let wallet = Pubkey::new_unique();
    let same_wallet = |n: usize| -> Vec<(Pubkey, Pubkey)> {
        (0..n).map(|_| (Pubkey::new_unique(), wallet)).collect()
    };
    let max_same = max_fitting(&env, &operator, false, same_wallet);
    assert_eq!(
        max_same, 28,
        "the maximum number of one wallet's pairs in a legacy tx"
    );

    // The mainnet profile: + SetComputeUnitLimit + SetComputeUnitPrice.
    let size_14_budget = close_tx_size(&env, &operator, &distinct(14), true);
    println!("14 distinct pairs + 2 ComputeBudget ixs: {size_14_budget} B");
    assert!(
        size_14_budget <= PACKET_DATA_SIZE,
        "14 distinct pairs must fit together with the ComputeBudget instructions"
    );

    // close_aggregates has byte-for-byte the same shape (the same context, 8 B of data):
    // a separate table is unnecessary, and the equality is pinned at N = 14.
    let pairs = distinct(14);
    let agg_ix = close_aggregates_ix(operator.pubkey(), &pairs);
    let msg = Message::new_with_blockhash(
        &[agg_ix],
        Some(&operator.pubkey()),
        &env.svm.latest_blockhash(),
    );
    let agg_tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[&operator])
        .expect("signing the bench tx");
    assert_eq!(
        bincode::serialized_size(&agg_tx).expect("the tx's bincode size"),
        close_tx_size(&env, &operator, &pairs, false),
        "close_aggregates and close_deposits are identical in tx size"
    );
}
