//! A load measurement of `purchase` serialization on a hot `SaleState` (Arch §13.2):
//! M wallets fire purchase transactions simultaneously, all writing to a single
//! singleton → the validator serializes them by write lock. surfpool's clock is NOT
//! touched - blocks come at their real pace (400 ms per slot).
//!
//! CAVEAT: surfpool is a local simulator with simplified forwarding and no network,
//! stake or QUIC queues; the absolute figures are OPTIMISTIC compared with mainnet
//! and are good only as an order of magnitude. The test's assertions cover only the
//! on-chain effect (the pool and the aggregates); the performance figures are printed
//! rather than asserted.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sale_e2e::E2e;
use sale_test_common::{
    init_aggregate_ix, initialize_ix, purchase_ix, wallet_usdc_ata, InitParams, CLAIM_DURATION_90D,
};
use solana_commitment_config::CommitmentConfig;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_rpc_client::nonblocking::rpc_client::RpcClient as NbClient;
use solana_rpc_client_api::config::{RpcSendTransactionConfig, RpcTransactionConfig};
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_status_client_types::UiTransactionEncoding;

const AMOUNT: u64 = 10_000_000; // $10 — min_per_tx
const SLOT_TIME_MS: f64 = 400.0;

/// The median over a copy (for the measurement's u64/f64 series).
fn median_u64(mut v: Vec<u64>) -> u64 {
    v.sort_unstable();
    v[v.len() / 2]
}
fn median_f64(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).expect("not NaN"));
    v[v.len() / 2]
}

struct RunStats {
    elapsed: Duration,
    retries: usize,
    slots: BTreeMap<u64, usize>,
    cu_median: u64,
}

#[test]
#[ignore = "e2e load: requires surfpool, run it through make e2e"]
fn purchase_write_lock_serialization_bench() {
    let m: usize = std::env::var("E2E_LOAD_WALLETS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let runs: usize = std::env::var("E2E_LOAD_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    // paused = false: the real pace of blocks is the subject of the measurement.
    let env = E2e::bootstrap(8909, 0, false);
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();

    let t0 = env.now();
    let start = t0 + 5;
    let end = t0 + 86_400;
    let params = InitParams {
        owner: owner.pubkey(),
        admin: env.actors.admin.pubkey(),
        operator: env.actors.operator.pubkey(),
        price_usdc: 30_000,
        emission: 13_000_000_000_000_000,
        min_per_tx: AMOUNT,
        max_per_tx: 10_000_000_000,
        sale_start_date: start,
        sale_end_date: end,
        claim_duration: CLAIM_DURATION_90D,
        claim_start_date: end + 1_000,
    };
    env.send(
        &[initialize_ix(owner.pubkey(), usdc, win, &params)],
        &owner.pubkey(),
        &[&owner],
    );

    // --- preparing M wallets: SOL, a USDC ATA and a balance covering every run ---
    let wallets: Vec<Keypair> = (0..m).map(|_| Keypair::new()).collect();
    for w in &wallets {
        env.airdrop(&w.pubkey(), 1_000_000_000);
    }
    for chunk in wallets.chunks(5) {
        let mut ixs = Vec::with_capacity(chunk.len() * 2);
        for w in chunk {
            ixs.push(
                spl_associated_token_account_interface::instruction::create_associated_token_account(
                    &env.payer.pubkey(),
                    &w.pubkey(),
                    &usdc,
                    &spl_token_interface::id(),
                ),
            );
            ixs.push(env.mint_to_ix(
                &usdc,
                &wallet_usdc_ata(&w.pubkey(), &usdc),
                AMOUNT * runs as u64,
            ));
        }
        env.send(&ixs, &env.payer.pubkey(), &[&env.payer]);
    }

    // we wait for the Sale phase by the Clock sysvar (the clock runs on its own)
    while env.now() <= start {
        std::thread::sleep(Duration::from_millis(200));
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let nb = Arc::new(NbClient::new_with_timeout_and_commitment(
        env.surf.rpc_url.clone(),
        Duration::from_secs(30),
        CommitmentConfig::processed(),
    ));

    // the aggregates: the same simultaneous writes into SaleState, but this is setup,
    // not the measurement
    let agg_txs: Vec<VersionedTransaction> = {
        let bh = env.rpc.get_latest_blockhash().expect("blockhash");
        wallets
            .iter()
            .map(|w| {
                let msg = Message::new_with_blockhash(
                    &[init_aggregate_ix(w.pubkey())],
                    Some(&w.pubkey()),
                    &bh,
                );
                VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[w])
                    .expect("the signature")
            })
            .collect()
    };
    let (_, _, setup_retries) = rt.block_on(send_all_and_confirm(&nb, agg_txs, |i, bh| {
        let w = &wallets[i];
        let msg =
            Message::new_with_blockhash(&[init_aggregate_ix(w.pubkey())], Some(&w.pubkey()), &bh);
        VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[w]).expect("the signature")
    }));
    assert_eq!(env.sale_state().participants, m as u64);
    println!("[load] setup: {m} aggregates created (retries: {setup_retries})");

    // --- the measured runs ---
    let mut stats: Vec<RunStats> = Vec::with_capacity(runs);
    for run in 0..runs {
        let pool_before = env.sale_state().deposits_usdc_contributed;
        let build = |i: usize, bh: solana_hash::Hash| {
            let w = &wallets[i];
            let ata = wallet_usdc_ata(&w.pubkey(), &usdc);
            let msg = Message::new_with_blockhash(
                &[purchase_ix(w.pubkey(), ata, run as u64, AMOUNT)],
                Some(&w.pubkey()),
                &bh,
            );
            VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[w])
                .expect("the signature")
        };
        let bh = env.rpc.get_latest_blockhash().expect("blockhash");
        let txs: Vec<VersionedTransaction> = (0..m).map(|i| build(i, bh)).collect();

        let (elapsed, sigs, retries) = rt.block_on(send_all_and_confirm(&nb, txs, build));

        // tying it to a real effect: the pool grew by exactly M × AMOUNT
        assert_eq!(
            env.sale_state().deposits_usdc_contributed,
            pool_before + m as u64 * AMOUNT,
            "run {run}: the pool did not add up"
        );
        assert_eq!(
            env.wallet_aggregate(&wallets[0].pubkey()).deposit_count,
            run as u64 + 1
        );

        let (slots, cus) = rt.block_on(collect_slots_and_cu(&nb, &sigs));
        let mut hist: BTreeMap<u64, usize> = BTreeMap::new();
        for s in &slots {
            *hist.entry(*s).or_default() += 1;
        }
        let cu_median = median_u64(cus);
        println!(
            "[load] run {run}: {m} purchases in {:.2}s ({:.1} tx/s), retries {retries}, slots {}, median CU {cu_median}",
            elapsed.as_secs_f64(),
            m as f64 / elapsed.as_secs_f64(),
            hist.len(),
        );
        print!("[load]   purchases per slot:");
        for (slot, n) in &hist {
            print!(" {slot}:{n}");
        }
        println!();
        stats.push(RunStats {
            elapsed,
            retries,
            slots: hist,
            cu_median,
        });
    }

    // --- the summary (medians across the runs) ---
    let tps = median_f64(
        stats
            .iter()
            .map(|s| m as f64 / s.elapsed.as_secs_f64())
            .collect(),
    );
    let cu = median_u64(stats.iter().map(|s| s.cu_median).collect());
    let per_slot_max = stats
        .iter()
        .flat_map(|s| s.slots.values().copied())
        .max()
        .unwrap_or(0);
    let retries_total: usize = stats.iter().map(|s| s.retries).sum();
    let blocks_per_sec = 1_000.0 / SLOT_TIME_MS;
    let block_limit_tps = (48_000_000 / cu) as f64 * blocks_per_sec;
    let hot_account_tps = (12_000_000 / cu) as f64 * blocks_per_sec;
    println!("[load] TOTAL (the median of {runs} runs): {tps:.1} purchases/s sustained");
    println!("[load]   CU of a single purchase (median): {cu}");
    println!("[load]   the maximum purchases in one slot: {per_slot_max}");
    println!("[load]   retries in total: {retries_total}");
    println!(
        "[load]   the theoretical ceiling: {block_limit_tps:.0} tx/s by the 48M CU block limit; {hot_account_tps:.0} tx/s by the 12M CU per-writable-account limit (the hot SaleState)"
    );
    println!(
        "[load]   surfpool is a local simulator, so the figures are optimistic compared with mainnet (an order of magnitude)"
    );
}

/// Sends all the transactions simultaneously (skip_preflight) and polls the statuses
/// in batches; anything stuck (an expired blockhash or a drop) is reassembled with a
/// fresh blockhash through `rebuild(i, blockhash)` and resent. Returns (the time to
/// full confirmation, the final signatures, and the number of resends).
async fn send_all_and_confirm(
    client: &Arc<NbClient>,
    txs: Vec<VersionedTransaction>,
    rebuild: impl Fn(usize, solana_hash::Hash) -> VersionedTransaction,
) -> (Duration, Vec<Signature>, usize) {
    const RESEND_AFTER: Duration = Duration::from_secs(8);
    const MAX_ATTEMPTS: usize = 5;
    let send_cfg = RpcSendTransactionConfig {
        skip_preflight: true,
        max_retries: Some(0),
        ..Default::default()
    };

    let n = txs.len();
    let started = Instant::now();

    let mut sigs: Vec<Signature> = Vec::with_capacity(n);
    let mut handles = Vec::with_capacity(n);
    for tx in &txs {
        sigs.push(*tx.signatures.first().expect("the signature"));
        let c = Arc::clone(client);
        let tx = tx.clone();
        handles.push(tokio::spawn(async move {
            // a drop on sending is caught by the poller as "it did not land" → a resend
            let _ = c.send_transaction_with_config(&tx, send_cfg).await;
        }));
    }
    for h in handles {
        h.await.expect("send task");
    }

    let mut confirmed = vec![false; n];
    let mut attempt_at = vec![Instant::now(); n];
    let mut attempts = vec![1usize; n];
    let mut retries = 0usize;
    loop {
        let pending: Vec<usize> = (0..n).filter(|i| !confirmed[*i]).collect();
        if pending.is_empty() {
            break;
        }
        for chunk in pending.chunks(200) {
            let chunk_sigs: Vec<Signature> = chunk.iter().map(|i| sigs[*i]).collect();
            let statuses = client
                .get_signature_statuses(&chunk_sigs)
                .await
                .expect("get_signature_statuses")
                .value;
            for (idx, st) in chunk.iter().zip(statuses) {
                if let Some(st) = st {
                    assert!(
                        st.err.is_none(),
                        "the purchase failed on-chain: {:?}",
                        st.err
                    );
                    confirmed[*idx] = true;
                }
            }
        }
        for i in (0..n).filter(|i| !confirmed[*i]).collect::<Vec<_>>() {
            if attempt_at[i].elapsed() > RESEND_AFTER {
                assert!(
                    attempts[i] < MAX_ATTEMPTS,
                    "transaction {i} did not land within {MAX_ATTEMPTS} attempts"
                );
                let bh = client.get_latest_blockhash().await.expect("blockhash");
                let tx = rebuild(i, bh);
                sigs[i] = *tx.signatures.first().expect("the signature");
                let _ = client.send_transaction_with_config(&tx, send_cfg).await;
                attempt_at[i] = Instant::now();
                attempts[i] += 1;
                retries += 1;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (started.elapsed(), sigs, retries)
}

/// Each transaction's slot and consumed CU - from getTransaction (the way an indexer
/// reads them).
async fn collect_slots_and_cu(client: &Arc<NbClient>, sigs: &[Signature]) -> (Vec<u64>, Vec<u64>) {
    let cfg = || RpcTransactionConfig {
        encoding: Some(UiTransactionEncoding::Json),
        commitment: Some(CommitmentConfig::confirmed()),
        max_supported_transaction_version: Some(0),
    };
    let mut handles = Vec::with_capacity(sigs.len());
    for sig in sigs {
        let c = Arc::clone(client);
        let sig = *sig;
        let cfg = cfg();
        handles.push(tokio::spawn(async move {
            let tx = c
                .get_transaction_with_config(&sig, cfg)
                .await
                .expect("getTransaction");
            let cu = Option::<u64>::from(tx.transaction.meta.expect("meta").compute_units_consumed)
                .expect("compute_units_consumed");
            (tx.slot, cu)
        }));
    }
    let mut slots = Vec::with_capacity(sigs.len());
    let mut cus = Vec::with_capacity(sigs.len());
    for h in handles {
        let (s, c) = h.await.expect("task");
        slots.push(s);
        cus.push(c);
    }
    (slots, cus)
}
