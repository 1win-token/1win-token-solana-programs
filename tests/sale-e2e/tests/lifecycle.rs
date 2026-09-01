//! An e2e run of the full Sale lifecycle against surfpool: oversubscribed and
//! undersubscribed. The reference values are hand-written literals (their derivation
//! is in the comments), and the events are cross-checked "the way an indexer sees
//! them" (getTransaction inner instructions plus the IDL discriminators).

use sale::errors::SaleError;
use sale_e2e::{anchor_err, assert_idl_event_discriminators, sale_err, E2e, SaleEvent};
use sale_test_common::{
    activate_claim_ix, aggregate_pda, claim_ix, close_aggregates_ix, close_deposits_ix,
    deposit_pda, deposit_vault_pda, init_aggregate_ix, initialize_ix, owner_token_ata,
    proceeds_vault_pda, purchase_ix, refund_vault_pda, sweep_deposit_vault_ix,
    sweep_refund_vault_ix, sweep_token_vault_ix, token_vault_pda, wallet_token_ata,
    wallet_usdc_ata, withdraw_proceeds_ix, InitParams, CLAIM_DURATION_90D,
};
use solana_signer::Signer;

// ---------- The "exactly this event" helpers ----------

fn as_initialized(e: SaleEvent) -> sale::events::Initialized {
    match e {
        SaleEvent::Initialized(x) => x,
        other => panic!("Initialized was expected, got {}", other.name()),
    }
}
fn as_aggregate_initialized(e: SaleEvent) -> sale::events::AggregateInitialized {
    match e {
        SaleEvent::AggregateInitialized(x) => x,
        other => panic!("AggregateInitialized was expected, got {}", other.name()),
    }
}
fn as_purchased(e: SaleEvent) -> sale::events::Purchased {
    match e {
        SaleEvent::Purchased(x) => x,
        other => panic!("Purchased was expected, got {}", other.name()),
    }
}
fn as_claim_activated(e: SaleEvent) -> sale::events::ClaimActivated {
    match e {
        SaleEvent::ClaimActivated(x) => x,
        other => panic!("ClaimActivated was expected, got {}", other.name()),
    }
}
fn as_claimed(e: SaleEvent) -> sale::events::Claimed {
    match e {
        SaleEvent::Claimed(x) => x,
        other => panic!("Claimed was expected, got {}", other.name()),
    }
}
fn as_proceeds_withdrawn(e: SaleEvent) -> sale::events::ProceedsWithdrawn {
    match e {
        SaleEvent::ProceedsWithdrawn(x) => x,
        other => panic!("ProceedsWithdrawn was expected, got {}", other.name()),
    }
}
fn as_swept(e: SaleEvent) -> sale::events::Swept {
    match e {
        SaleEvent::Swept(x) => x,
        other => panic!("Swept was expected, got {}", other.name()),
    }
}
fn as_accounts_closed(e: SaleEvent) -> sale::events::AccountsClosed {
    match e {
        SaleEvent::AccountsClosed(x) => x,
        other => panic!("AccountsClosed was expected, got {}", other.name()),
    }
}

/// 2.56x oversubscription on the terms of E1 (Arch §10.5): a pool of $10M against a
/// soft cap of $3.9M.
///
/// The hand-written reference values:
/// - pool = 10^13, EMI = 1.3×10^16, price = 30_000, participants = 4 (W4 contributes
///   nothing);
/// - proceeds = SOFT_CAP − participants = 3_900_000_000_000 − 4 = 3_899_999_999_996;
/// - refund_vault = pool − proceeds = 6_100_000_000_004;
/// - W1 (net 10^10): tokens = 10^10×1.3×10^16/10^13 = 1.3×10^13; spent = 3.9×10^9;
///   refund = 6.1×10^9 (= E1);
/// - W2 (net 4.99×10^12): tokens = 4.99×10^12×1.3×10^16/10^13 = 6.487×10^15 exactly;
///   spent = 6.487×10^15×3×10^4/10^8 = 1.9461×10^12 exactly; refund = 3.0439×10^12;
/// - W3 (net 5×10^12): tokens = 6.5×10^15; spent = 1.95×10^12; refund = 3.05×10^12;
/// - Σ tokens = 1.3×10^13 + 6.487×10^15 + 6.5×10^15 = 1.3×10^16 = EMI (the vault
///   drains to zero);
/// - the refund_vault remainder after the claims = 6_100_000_000_004 − (6.1×10^9 +
///   3.0439×10^12 + 3.05×10^12) = 4 (the rounding reserve) → the sweep.
#[test]
#[ignore = "e2e: requires surfpool, run it through make e2e"]
fn oversubscribed_full_lifecycle_on_surfpool() {
    const PRICE: u64 = 30_000;
    const EMISSION: u64 = 13_000_000_000_000_000;
    const W1_HALF: u64 = 5_000_000_000;
    const W1_NET: u64 = 10_000_000_000;
    const W2_NET: u64 = 4_990_000_000_000;
    const W3_NET: u64 = 5_000_000_000_000;
    const POOL: u64 = 10_000_000_000_000;
    const PROCEEDS: u64 = 3_899_999_999_996;
    const REFUND_VAULT_FUNDED: u64 = 6_100_000_000_004;
    const W1_TOKENS: u64 = 13_000_000_000_000;
    const W1_REFUND: u64 = 6_100_000_000;
    const W2_TOKENS: u64 = 6_487_000_000_000_000;
    const W2_REFUND: u64 = 3_043_900_000_000;
    const W3_TOKENS: u64 = 6_500_000_000_000_000;
    const W3_REFUND: u64 = 3_050_000_000_000;
    const DUST: u64 = 4;

    assert_idl_event_discriminators();

    let env = E2e::bootstrap(8905, 4, true);
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let w1 = env.actors.users[0].insecure_clone();
    let w2 = env.actors.users[1].insecure_clone();
    let w3 = env.actors.users[2].insecure_clone();
    let w4 = env.actors.users[3].insecure_clone();

    let t0 = env.now();
    let start = t0 + 1_000;
    let end = t0 + 2_000;
    let claim_start_date = t0 + 3_000;
    let deadline = claim_start_date + CLAIM_DURATION_90D;
    let params = InitParams {
        owner: owner.pubkey(),
        admin: admin.pubkey(),
        operator: operator.pubkey(),
        price_usdc: PRICE,
        emission: EMISSION,
        min_per_tx: 10_000_000,
        max_per_tx: 5_000_000_000_000,
        sale_start_date: start,
        sale_end_date: end,
        claim_duration: CLAIM_DURATION_90D,
        claim_start_date,
    };

    // --- initialize (the signer = the upgrade authority = owner) ---
    let sig = env.send(
        &[initialize_ix(owner.pubkey(), usdc, win, &params)],
        &owner.pubkey(),
        &[&owner],
    );
    let ev = as_initialized(env.single_event(&sig));
    assert_eq!(ev.owner, owner.pubkey());
    assert_eq!(ev.admin, admin.pubkey());
    assert_eq!(ev.operator, operator.pubkey());
    assert_eq!(ev.usdc_mint, usdc);
    assert_eq!(ev.token_mint, win);
    assert_eq!(ev.price_usdc, PRICE);
    assert_eq!(ev.emission, EMISSION);
    assert_eq!(ev.min_per_tx, 10_000_000);
    assert_eq!(ev.max_per_tx, 5_000_000_000_000);
    assert_eq!(ev.sale_start_date, start);
    assert_eq!(ev.sale_end_date, end);
    assert_eq!(ev.claim_duration, CLAIM_DURATION_90D);
    assert_eq!(ev.claim_start_date, claim_start_date);
    assert!(env.account_exists(&deposit_vault_pda()));
    assert!(env.account_exists(&token_vault_pda()));
    assert_eq!(env.token_balance(&deposit_vault_pda()), 0);

    // --- PreSale: the buyer's path is closed (the negative counterpart to the positive
    // below) ---
    env.send_expect_custom(
        &[init_aggregate_ix(w1.pubkey())],
        &w1.pubkey(),
        &[&w1],
        sale_err(SaleError::WrongPhase),
    );

    // --- Sale: the aggregates plus the purchases ---
    env.warp_to(start + 1);
    for w in [&w1, &w2, &w3, &w4] {
        let sig = env.send(&[init_aggregate_ix(w.pubkey())], &w.pubkey(), &[w]);
        let ev = as_aggregate_initialized(env.single_event(&sig));
        assert_eq!(ev.wallet, w.pubkey());
    }

    for (w, net) in [(&w1, W1_NET), (&w2, W2_NET), (&w3, W3_NET)] {
        let ata = env.create_ata(&w.pubkey(), &usdc);
        env.mint_to(&usdc, &ata, net);
    }

    // (wallet, index, amount, the expected net afterwards, the expected total afterwards)
    let purchases: [(&solana_keypair::Keypair, u64, u64, u64, u64); 4] = [
        (&w1, 0, W1_HALF, W1_HALF, 5_000_000_000),
        (&w1, 1, W1_HALF, W1_NET, 10_000_000_000),
        (&w2, 0, W2_NET, W2_NET, 5_000_000_000_000),
        (&w3, 0, W3_NET, W3_NET, 10_000_000_000_000),
    ];
    for (w, index, amount, exp_net, exp_total) in purchases {
        let ata = wallet_usdc_ata(&w.pubkey(), &usdc);
        let sig = env.send(
            &[purchase_ix(w.pubkey(), ata, index, amount)],
            &w.pubkey(),
            &[w],
        );
        let ev = as_purchased(env.single_event(&sig));
        assert_eq!(ev.wallet, w.pubkey());
        assert_eq!(ev.index, index);
        assert_eq!(ev.amount, amount);
        assert_eq!(ev.net, exp_net);
        assert_eq!(ev.total, exp_total);
    }
    assert_eq!(env.token_balance(&deposit_vault_pda()), POOL);
    assert_eq!(env.token_balance(&wallet_usdc_ata(&w1.pubkey(), &usdc)), 0);
    let state = env.sale_state();
    assert_eq!(state.deposits_usdc_contributed, POOL);
    assert_eq!(state.participants, 4);
    assert!(!state.pool_fixed);
    for (w, count) in [(&w1, 2u64), (&w2, 1), (&w3, 1)] {
        let agg = env.wallet_aggregate(&w.pubkey());
        assert_eq!(agg.deposit_count, count);
        assert!(env.account_exists(&deposit_pda(&w.pubkey(), 0)));
    }

    // --- SaleEnded: purchasing is closed ---
    env.warp_to(end);
    env.send_expect_custom(
        &[purchase_ix(
            w2.pubkey(),
            wallet_usdc_ata(&w2.pubkey(), &usdc),
            1,
            10_000_000,
        )],
        &w2.pubkey(),
        &[&w2],
        sale_err(SaleError::WrongPhase),
    );

    // --- M2: minting the whole emission plus activate_claim in ONE transaction ---
    let mint_and_activate = env.build_tx(
        &[
            env.mint_to_ix(&win, &token_vault_pda(), EMISSION),
            activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        ],
        &env.payer.pubkey(),
        &[&env.payer, &admin],
    );
    let tx_size = bincode::serialize(&mint_and_activate).expect("size").len();
    assert!(
        tx_size <= 1232,
        "the mint plus the activation did not fit into one tx: {tx_size}"
    );
    let sig = env.send_prebuilt(&mint_and_activate);
    let cu = Option::<u64>::from(
        env.get_tx(&sig)
            .transaction
            .meta
            .expect("meta")
            .compute_units_consumed,
    );
    println!("[m2] mint+activate in one tx: {tx_size} bytes, CU = {cu:?}");

    let ev = as_claim_activated(env.single_event(&sig));
    assert_eq!(ev.pool_fixed_amount, POOL);
    assert_eq!(ev.distributable, EMISSION);
    assert_eq!(ev.proceeds_vault_amount, PROCEEDS);
    assert_eq!(ev.refund_vault_amount, REFUND_VAULT_FUNDED);
    assert_eq!(ev.claim_start_date, claim_start_date);
    assert_eq!(ev.claim_deadline, deadline);
    assert_eq!(ev.price_usdc, PRICE);
    assert_eq!(ev.emission, EMISSION);

    assert_eq!(env.token_balance(&proceeds_vault_pda()), PROCEEDS);
    assert_eq!(env.token_balance(&refund_vault_pda()), REFUND_VAULT_FUNDED);
    assert_eq!(env.token_balance(&deposit_vault_pda()), 0);
    assert_eq!(env.token_balance(&token_vault_pda()), EMISSION);
    let state = env.sale_state();
    assert!(state.pool_fixed);
    assert_eq!(state.pool_fixed_amount, POOL);

    // --- Before claim_start_date: claiming and withdrawing the proceeds are closed ---
    env.send_expect_custom(
        &[claim_ix(w1.pubkey(), usdc, win, true)],
        &w1.pubkey(),
        &[&w1],
        sale_err(SaleError::WrongPhase),
    );
    env.send_expect_custom(
        &[withdraw_proceeds_ix(
            owner.pubkey(),
            usdc,
            operator.pubkey(),
        )],
        &owner.pubkey(),
        &[&owner],
        sale_err(SaleError::ClaimNotStarted),
    );

    // --- Claim: the proceeds plus the claims ---
    env.warp_to(claim_start_date + 1);

    // a non-Owner does not withdraw the proceeds
    env.send_expect_custom(
        &[withdraw_proceeds_ix(
            admin.pubkey(),
            usdc,
            operator.pubkey(),
        )],
        &admin.pubkey(),
        &[&admin],
        sale_err(SaleError::Unauthorized),
    );
    let proceeds_rent = env.lamports(&proceeds_vault_pda());
    let operator_before = env.lamports(&operator.pubkey());
    let sig = env.send(
        &[withdraw_proceeds_ix(
            owner.pubkey(),
            usdc,
            operator.pubkey(),
        )],
        &owner.pubkey(),
        &[&owner],
    );
    let ev = as_proceeds_withdrawn(env.single_event(&sig));
    assert_eq!(ev.amount, PROCEEDS);
    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc)),
        PROCEEDS
    );
    assert!(!env.account_exists(&proceeds_vault_pda()));
    assert_eq!(
        env.lamports(&operator.pubkey()),
        operator_before + proceeds_rent
    );

    // the claims: tokens plus a refund, and the aggregate is closed
    for (w, tokens, refund) in [
        (&w1, W1_TOKENS, W1_REFUND),
        (&w2, W2_TOKENS, W2_REFUND),
        (&w3, W3_TOKENS, W3_REFUND),
    ] {
        let sig = env.send(&[claim_ix(w.pubkey(), usdc, win, true)], &w.pubkey(), &[w]);
        let ev = as_claimed(env.single_event(&sig));
        assert_eq!(ev.wallet, w.pubkey());
        assert_eq!(ev.tokens, tokens);
        assert_eq!(ev.refund, refund);
        assert_eq!(
            env.token_balance(&wallet_token_ata(&w.pubkey(), &win)),
            tokens
        );
        assert_eq!(
            env.token_balance(&wallet_usdc_ata(&w.pubkey(), &usdc)),
            refund
        );
        assert!(!env.account_exists(&aggregate_pda(&w.pubkey())));
    }
    assert_eq!(env.token_balance(&token_vault_pda()), 0); // Σ tokens == EMI
    assert_eq!(env.token_balance(&refund_vault_pda()), DUST);

    // a repeated claim: the aggregate is closed - the replay guard (I5)
    let replay = [
        solana_compute_budget_interface::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
        claim_ix(w1.pubkey(), usdc, win, true),
    ];
    env.send_expect_custom(
        &replay,
        &w1.pubkey(),
        &[&w1],
        anchor_err(anchor_lang::error::ErrorCode::AccountNotInitialized),
    );

    // an empty aggregate (W4, net = 0): there is nothing to claim
    env.send_expect_custom(
        &[claim_ix(w4.pubkey(), usdc, win, false)],
        &w4.pubkey(),
        &[&w4],
        sale_err(SaleError::NothingToClaim),
    );

    let pairs = [
        (deposit_pda(&w1.pubkey(), 0), w1.pubkey()),
        (deposit_pda(&w1.pubkey(), 1), w1.pubkey()),
        (deposit_pda(&w2.pubkey(), 0), w2.pubkey()),
        (deposit_pda(&w3.pubkey(), 0), w3.pubkey()),
    ];

    // --- before the deadline both the sweep and the batch closing of deposits are
    // closed ---
    env.send_expect_custom(
        &[sweep_refund_vault_ix(owner.pubkey(), usdc, admin.pubkey())],
        &owner.pubkey(),
        &[&owner],
        sale_err(SaleError::DeadlineNotPassed),
    );
    env.send_expect_custom(
        &[close_deposits_ix(operator.pubkey(), &pairs)],
        &operator.pubkey(),
        &[&operator],
        sale_err(SaleError::DeadlineNotPassed),
    );

    // --- ClaimClosed: the sweeps plus closing the deposits and aggregates ---
    env.warp_to(deadline);
    let vault_rent = env.lamports(&refund_vault_pda());
    let admin_before = env.lamports(&admin.pubkey());

    let sig = env.send(
        &[sweep_refund_vault_ix(owner.pubkey(), usdc, admin.pubkey())],
        &owner.pubkey(),
        &[&owner],
    );
    let ev = as_swept(env.single_event(&sig));
    assert_eq!(ev.vault, refund_vault_pda());
    assert_eq!(ev.amount, DUST);

    let sig = env.send(
        &[sweep_deposit_vault_ix(owner.pubkey(), usdc, admin.pubkey())],
        &owner.pubkey(),
        &[&owner],
    );
    let ev = as_swept(env.single_event(&sig));
    assert_eq!(ev.vault, deposit_vault_pda());
    assert_eq!(ev.amount, 0);

    let sig = env.send(
        &[sweep_token_vault_ix(owner.pubkey(), win, admin.pubkey())],
        &owner.pubkey(),
        &[&owner],
    );
    let ev = as_swept(env.single_event(&sig));
    assert_eq!(ev.vault, token_vault_pda());
    assert_eq!(ev.amount, 0);

    for vault in [refund_vault_pda(), deposit_vault_pda(), token_vault_pda()] {
        assert!(!env.account_exists(&vault));
    }
    // 3.9e12 − 4 (the proceeds) + 4 (the dust from refund_vault) = exactly the soft cap
    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc)),
        3_900_000_000_000
    );
    assert_eq!(
        env.token_balance(&owner_token_ata(&owner.pubkey(), &win)),
        0
    );
    assert_eq!(env.lamports(&admin.pubkey()), admin_before + vault_rent * 3);

    // --- the batch closing of the deposits ---
    env.send_expect_custom(
        &[close_deposits_ix(admin.pubkey(), &pairs)],
        &admin.pubkey(),
        &[&admin],
        sale_err(SaleError::Unauthorized),
    );
    let deposit_rent = env.lamports(&deposit_pda(&w1.pubkey(), 0));
    let balances_before = [&w1, &w2, &w3].map(|w| env.lamports(&w.pubkey()));
    let sig = env.send(
        &[close_deposits_ix(operator.pubkey(), &pairs)],
        &operator.pubkey(),
        &[&operator],
    );
    let ev = as_accounts_closed(env.single_event(&sig));
    assert_eq!(ev.count, 4);
    assert_eq!(ev.rent_returned, deposit_rent * 4);
    for (deposit, _) in &pairs {
        assert!(!env.account_exists(deposit));
    }
    assert_eq!(
        env.lamports(&w1.pubkey()),
        balances_before[0] + deposit_rent * 2
    );
    assert_eq!(
        env.lamports(&w2.pubkey()),
        balances_before[1] + deposit_rent
    );
    assert_eq!(
        env.lamports(&w3.pubkey()),
        balances_before[2] + deposit_rent
    );

    // W4's aggregate (with no contribution) is closed by a batch past the deadline
    let aggregate_rent = env.lamports(&aggregate_pda(&w4.pubkey()));
    let w4_before = env.lamports(&w4.pubkey());
    let sig = env.send(
        &[close_aggregates_ix(
            operator.pubkey(),
            &[(aggregate_pda(&w4.pubkey()), w4.pubkey())],
        )],
        &operator.pubkey(),
        &[&operator],
    );
    let ev = as_accounts_closed(env.single_event(&sig));
    assert_eq!(ev.count, 1);
    assert_eq!(ev.rent_returned, aggregate_rent);
    assert!(!env.account_exists(&aggregate_pda(&w4.pubkey())));
    assert_eq!(env.lamports(&w4.pubkey()), w4_before + aggregate_rent);

    println!("[e2e] oversubscribed: the full cycle passed on surfpool");
}

/// Undersubscribed: a pool of $30k << the soft cap of $3.9M. It mints
/// floor(pool×10^8/price), activates WITHOUT refund_vault, and the claims carry no
/// refunds.
///
/// The hand-written reference values:
/// - pool = 3×10^10; DIST = 3×10^10×10^8/3×10^4 = 10^14 exactly;
/// - W1 (net 10^10): tokens = 10^10×10^8/3×10^4 = 33_333_333_333_333 (floor .33);
/// - W2 (net 2×10^10): tokens = 66_666_666_666_666 (floor .67);
/// - Σ = 99_999_999_999_999 = DIST − 1 → 1 unit of dust in token_vault → the sweep.
#[test]
#[ignore = "e2e: requires surfpool, run it through make e2e"]
fn undersubscribed_full_lifecycle_on_surfpool() {
    const PRICE: u64 = 30_000;
    const EMISSION: u64 = 13_000_000_000_000_000;
    const W1_NET: u64 = 10_000_000_000;
    const W2_NET: u64 = 20_000_000_000;
    const POOL: u64 = 30_000_000_000;
    const DIST: u64 = 100_000_000_000_000;
    const W1_TOKENS: u64 = 33_333_333_333_333;
    const W2_TOKENS: u64 = 66_666_666_666_666;
    const TOKEN_DUST: u64 = 1;

    assert_idl_event_discriminators();

    let env = E2e::bootstrap(8907, 3, true);
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    let owner = env.actors.owner.insecure_clone();
    let admin = env.actors.admin.insecure_clone();
    let operator = env.actors.operator.insecure_clone();
    let w1 = env.actors.users[0].insecure_clone();
    let w2 = env.actors.users[1].insecure_clone();
    let w3 = env.actors.users[2].insecure_clone();

    let t0 = env.now();
    let start = t0 + 1_000;
    let end = t0 + 2_000;
    let claim_start_date = t0 + 3_000;
    let deadline = claim_start_date + CLAIM_DURATION_90D;
    let params = InitParams {
        owner: owner.pubkey(),
        admin: admin.pubkey(),
        operator: operator.pubkey(),
        price_usdc: PRICE,
        emission: EMISSION,
        min_per_tx: 10_000_000,
        max_per_tx: 20_000_000_000,
        sale_start_date: start,
        sale_end_date: end,
        claim_duration: CLAIM_DURATION_90D,
        claim_start_date,
    };
    env.send(
        &[initialize_ix(owner.pubkey(), usdc, win, &params)],
        &owner.pubkey(),
        &[&owner],
    );

    env.warp_to(start + 1);
    for w in [&w1, &w2, &w3] {
        env.send(&[init_aggregate_ix(w.pubkey())], &w.pubkey(), &[w]);
    }
    for (w, net) in [(&w1, W1_NET), (&w2, W2_NET)] {
        let ata = env.create_ata(&w.pubkey(), &usdc);
        env.mint_to(&usdc, &ata, net);
        let sig = env.send(&[purchase_ix(w.pubkey(), ata, 0, net)], &w.pubkey(), &[w]);
        let ev = as_purchased(env.single_event(&sig));
        assert_eq!(ev.net, net);
    }
    assert_eq!(env.token_balance(&deposit_vault_pda()), POOL);
    assert_eq!(env.sale_state().deposits_usdc_contributed, POOL);

    env.warp_to(end);

    // fail-closed: refund_vault is not relied on when undersubscribed; the mint in the
    // same tx rolls back along with it (the bundle's atomicity)
    env.send_expect_custom(
        &[
            env.mint_to_ix(&win, &token_vault_pda(), DIST),
            activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        ],
        &env.payer.pubkey(),
        &[&env.payer, &admin],
        sale_err(SaleError::RefundVaultMismatch),
    );
    assert_eq!(env.token_balance(&token_vault_pda()), 0); // the mint rolled back
    assert!(!env.account_exists(&refund_vault_pda()));

    // M2: minting DIST plus the activation without refund_vault in one tx
    let mint_and_activate = env.build_tx(
        &[
            env.mint_to_ix(&win, &token_vault_pda(), DIST),
            activate_claim_ix(admin.pubkey(), usdc, None),
        ],
        &env.payer.pubkey(),
        &[&env.payer, &admin],
    );
    let tx_size = bincode::serialize(&mint_and_activate).expect("size").len();
    assert!(
        tx_size <= 1232,
        "the mint plus the activation did not fit into one tx: {tx_size}"
    );
    let sig = env.send_prebuilt(&mint_and_activate);
    let cu = Option::<u64>::from(
        env.get_tx(&sig)
            .transaction
            .meta
            .expect("meta")
            .compute_units_consumed,
    );
    println!("[m2] undersubscribed, mint+activate in one tx: {tx_size} bytes, CU = {cu:?}");

    let ev = as_claim_activated(env.single_event(&sig));
    assert_eq!(ev.pool_fixed_amount, POOL);
    assert_eq!(ev.distributable, DIST);
    assert_eq!(ev.proceeds_vault_amount, POOL);
    assert_eq!(ev.refund_vault_amount, 0);
    assert_eq!(env.token_balance(&proceeds_vault_pda()), POOL);
    assert_eq!(env.token_balance(&deposit_vault_pda()), 0);
    assert_eq!(env.token_balance(&token_vault_pda()), DIST);
    assert!(!env.account_exists(&refund_vault_pda()));

    // --- Claim ---
    env.warp_to(claim_start_date + 1);
    let sig = env.send(
        &[withdraw_proceeds_ix(
            owner.pubkey(),
            usdc,
            operator.pubkey(),
        )],
        &owner.pubkey(),
        &[&owner],
    );
    assert_eq!(as_proceeds_withdrawn(env.single_event(&sig)).amount, POOL);
    assert_eq!(
        env.token_balance(&wallet_usdc_ata(&owner.pubkey(), &usdc)),
        POOL
    );

    for (w, tokens) in [(&w1, W1_TOKENS), (&w2, W2_TOKENS)] {
        let sig = env.send(&[claim_ix(w.pubkey(), usdc, win, false)], &w.pubkey(), &[w]);
        let ev = as_claimed(env.single_event(&sig));
        assert_eq!(ev.tokens, tokens);
        assert_eq!(ev.refund, 0);
        assert_eq!(
            env.token_balance(&wallet_token_ata(&w.pubkey(), &win)),
            tokens
        );
        // there is no refund: the USDC ATA stays empty
        assert_eq!(env.token_balance(&wallet_usdc_ata(&w.pubkey(), &usdc)), 0);
        assert!(!env.account_exists(&aggregate_pda(&w.pubkey())));
    }
    assert_eq!(env.token_balance(&token_vault_pda()), TOKEN_DUST);

    let pairs = [
        (deposit_pda(&w1.pubkey(), 0), w1.pubkey()),
        (deposit_pda(&w2.pubkey(), 0), w2.pubkey()),
    ];

    // --- ClaimClosed ---
    env.warp_to(deadline);

    let sig = env.send(
        &[close_deposits_ix(operator.pubkey(), &pairs)],
        &operator.pubkey(),
        &[&operator],
    );
    assert_eq!(as_accounts_closed(env.single_event(&sig)).count, 2);

    // refund_vault never existed when undersubscribed - the sweep does not apply (§9.14)
    env.send_expect_custom(
        &[sweep_refund_vault_ix(owner.pubkey(), usdc, admin.pubkey())],
        &owner.pubkey(),
        &[&owner],
        anchor_err(anchor_lang::error::ErrorCode::AccountNotInitialized),
    );

    let sig = env.send(
        &[sweep_deposit_vault_ix(owner.pubkey(), usdc, admin.pubkey())],
        &owner.pubkey(),
        &[&owner],
    );
    assert_eq!(as_swept(env.single_event(&sig)).amount, 0);

    let sig = env.send(
        &[sweep_token_vault_ix(owner.pubkey(), win, admin.pubkey())],
        &owner.pubkey(),
        &[&owner],
    );
    let ev = as_swept(env.single_event(&sig));
    assert_eq!(ev.vault, token_vault_pda());
    assert_eq!(ev.amount, TOKEN_DUST);
    assert_eq!(
        env.token_balance(&owner_token_ata(&owner.pubkey(), &win)),
        TOKEN_DUST
    );
    assert!(!env.account_exists(&deposit_vault_pda()));
    assert!(!env.account_exists(&token_vault_pda()));

    let sig = env.send(
        &[close_aggregates_ix(
            operator.pubkey(),
            &[(aggregate_pda(&w3.pubkey()), w3.pubkey())],
        )],
        &operator.pubkey(),
        &[&operator],
    );
    assert_eq!(as_accounts_closed(env.single_event(&sig)).count, 1);
    assert!(!env.account_exists(&aggregate_pda(&w3.pubkey())));

    println!("[e2e] undersubscribed: the full cycle passed on surfpool");
}
