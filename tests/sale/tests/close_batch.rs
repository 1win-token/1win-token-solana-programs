//! Batched closing by the Operator: `close_deposits` / `close_aggregates` -
//! iteration 5; §9.13. `remaining_accounts` is entirely untrusted - 6 manual checks
//! per `[account, wallet]` pair, and the rent goes strictly to `body.wallet`.
//!
//! tests-bind-to-real-code: real instructions through the builders; the assertions
//! run against observable effects (the fact of closing, the rent recipient's
//! lamports, the state fields, the `AccountsClosed{count, rent_returned}` payload);
//! each of the 6 checks gets its own negative with a SPECIFIC code.
//!
//! The invariants under test:
//! - §9.13: closing validates the owning program, the type plus discriminator, the
//!   PDA derivation from the body, the account's liveness, the paired rent recipient
//!   and the lamports.
//! - I8 (§3): a batch close does NOT move pool_fixed_amount / participants.
//! - Atomicity: a duplicate in the batch fails the whole tx and the first account
//!   "survives".
//! - The gates: both instructions require the ClaimClosed phase and the Operator.

use anchor_lang::error::ERROR_CODE_OFFSET;
use anchor_lang::{AnchorSerialize, Discriminator};
use sale::errors::SaleError;
use sale::events::AccountsClosed;
use sale::state::{Deposit, MAX_CLOSE_PER_BATCH};
use sale_test_common::{
    activate_claim_ix, aggregate_pda, close_aggregates_ix, close_deposits_ix, custom_error_code,
    decode_event, deposit_pda, expected::DEFAULT_PRICE_USDC, init_aggregate_ix, initialize_ix,
    purchase_ix, refund_vault_pda, sale_state_pda, token_vault_pda, InitParams, TestEnv,
};
use solana_instruction::{AccountMeta, Instruction};
use solana_keypair::Keypair;
use solana_pubkey::Pubkey;
use solana_signer::Signer;

const PRICE: u64 = DEFAULT_PRICE_USDC;
const EMISSION: u64 = 100_000_000_000;
const UNDER_DEP: u64 = 10_000_000;
const UNDER_POOL: u64 = 20_000_000;
const UNDER_DIST: u64 = 66_666_666_666;

fn err(variant: SaleError) -> u32 {
    ERROR_CODE_OFFSET + variant as u32
}

fn custom(result: &litesvm::types::TransactionResult) -> Option<u32> {
    custom_error_code(result)
}

#[allow(clippy::result_large_err)]
fn send(env: &mut TestEnv, ix: Instruction, signer: &Keypair) -> litesvm::types::TransactionResult {
    env.send_ixs(&[ix], &signer.pubkey(), &[signer])
}

fn funded_ata(env: &mut TestEnv, wallet: &Pubkey, amount: u64) -> Pubkey {
    let usdc = env.usdc_mint;
    let ata = env.create_ata(wallet, &usdc);
    env.mint_to(&usdc, &ata, amount);
    ata
}

fn params_with_emission(env: &TestEnv, emission: u64) -> InitParams {
    let mut p = env.valid_init_params();
    p.price_usdc = PRICE;
    p.emission = emission;
    p
}

fn init_with(env: &mut TestEnv, p: &InitParams) {
    let owner = env.actors.owner.insecure_clone();
    let (usdc, win) = (env.usdc_mint, env.win_mint);
    send(env, initialize_ix(owner.pubkey(), usdc, win, p), &owner).expect("initialize");
}

/// Collects the pool: `init_aggregate` plus one `purchase` from each wallet.
fn build_pool_in_sale(env: &mut TestEnv, p: &InitParams, deposits: &[(&Keypair, u64)]) {
    init_with(env, p);
    env.warp_to(p.sale_start_date);
    for (w, amount) in deposits {
        let ata = funded_ata(env, &w.pubkey(), *amount);
        env.expire_blockhash();
        send(env, init_aggregate_ix(w.pubkey()), w).expect("init_aggregate");
        env.expire_blockhash();
        send(env, purchase_ix(w.pubkey(), ata, 0, *amount), w).expect("purchase");
    }
}

fn fund_token_vault(env: &mut TestEnv, amount: u64) {
    let win = env.win_mint;
    env.mint_to(&win, &token_vault_pda(), amount);
}

/// Collects the pool (undersubscribed) and fixes it. It stays in SaleEnded after
/// fixation.
fn activate_undersubscribed(env: &mut TestEnv, p: &InitParams, deposits: &[(&Keypair, u64)]) {
    build_pool_in_sale(env, p, deposits);
    env.warp_to(p.sale_end_date);
    fund_token_vault(env, UNDER_DIST);
    let admin = env.actors.admin.insecure_clone();
    let usdc = env.usdc_mint;
    env.expire_blockhash();
    send(env, activate_claim_ix(admin.pubkey(), usdc, None), &admin).expect("activate_claim");
}

fn deadline_of(p: &InitParams) -> i64 {
    p.claim_start_date + p.claim_duration
}

/// The same as `activate_undersubscribed`, plus a warp past the claim deadline: both
/// batch closes work only in the ClaimClosed phase.
fn activate_and_reach_claim_closed(
    env: &mut TestEnv,
    p: &InitParams,
    deposits: &[(&Keypair, u64)],
) {
    activate_undersubscribed(env, p, deposits);
    env.warp_to(deadline_of(p));
}

/// The bytes of a program-owned `Deposit` account: the discriminator plus
/// Borsh(body).
fn deposit_account_bytes(d: &Deposit) -> Vec<u8> {
    let mut bytes = Deposit::DISCRIMINATOR.to_vec();
    d.serialize(&mut bytes).expect("serialize Deposit");
    bytes
}

// ===================================================================
// POSITIVE close_deposits: closing N deposits, with the rent going to each
// body.wallet
// ===================================================================

#[test]
fn close_deposits_closes_and_returns_rent_to_body_wallets() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // Two wallets with one deposit at index 0 each. We fix the pool and move past the
    // claim deadline.
    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    assert!(env.deposit_exists(&a.pubkey(), 0) && env.deposit_exists(&b.pubkey(), 0));

    let (dep_a, dep_b) = (deposit_pda(&a.pubkey(), 0), deposit_pda(&b.pubkey(), 0));
    let rent_a = env.lamports_of(&dep_a);
    let rent_b = env.lamports_of(&dep_b);
    let a_before = env.lamports_of(&a.pubkey());
    let b_before = env.lamports_of(&b.pubkey());

    let pairs = [(dep_a, a.pubkey()), (dep_b, b.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    r.as_ref().expect("close_deposits passes");

    // Both deposits are closed.
    assert!(!env.deposit_exists(&a.pubkey(), 0), "A's deposit is closed");
    assert!(!env.deposit_exists(&b.pubkey(), 0), "B's deposit is closed");
    // The rent went to each one's body.wallet (not to the signing operator).
    assert_eq!(
        env.lamports_of(&a.pubkey()) - a_before,
        rent_a,
        "A's deposit rent → wallet A"
    );
    assert_eq!(
        env.lamports_of(&b.pubkey()) - b_before,
        rent_b,
        "B's deposit rent → wallet B"
    );
    // The AccountsClosed payload: count == 2, rent_returned == the total rent.
    let ev: AccountsClosed = decode_event(&r).expect("the AccountsClosed event");
    assert_eq!(ev.count, 2, "AccountsClosed.count == 2");
    assert_eq!(
        ev.rent_returned,
        rent_a + rent_b,
        "AccountsClosed.rent_returned == Σ of the rents"
    );
}

// ===================================================================
// POSITIVE close_aggregates: past the deadline, the rent goes to body.wallet
// ===================================================================

#[test]
fn close_aggregates_closes_after_deadline_returns_rent() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_undersubscribed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(deadline_of(&p)); // ClaimClosed - the aggregates can be closed
    assert!(env.aggregate_exists(&a.pubkey()) && env.aggregate_exists(&b.pubkey()));

    let (agg_a, agg_b) = (aggregate_pda(&a.pubkey()), aggregate_pda(&b.pubkey()));
    let rent_a = env.lamports_of(&agg_a);
    let rent_b = env.lamports_of(&agg_b);
    let a_before = env.lamports_of(&a.pubkey());
    let b_before = env.lamports_of(&b.pubkey());

    let pairs = [(agg_a, a.pubkey()), (agg_b, b.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_aggregates_ix(operator.pubkey(), &pairs),
        &operator,
    );
    r.as_ref().expect("close_aggregates passes");

    assert!(
        !env.aggregate_exists(&a.pubkey()),
        "A's aggregate is closed"
    );
    assert!(
        !env.aggregate_exists(&b.pubkey()),
        "B's aggregate is closed"
    );
    assert_eq!(
        env.lamports_of(&a.pubkey()) - a_before,
        rent_a,
        "the rent → A"
    );
    assert_eq!(
        env.lamports_of(&b.pubkey()) - b_before,
        rent_b,
        "the rent → B"
    );
    let ev: AccountsClosed = decode_event(&r).expect("the AccountsClosed event");
    assert_eq!(ev.count, 2, "AccountsClosed.count == 2");
    assert_eq!(
        ev.rent_returned,
        rent_a + rent_b,
        "rent_returned == Σ of the rents"
    );
}

// ===================================================================
// I8 (MANDATORY NEG): a batch close does NOT move pool_fixed_amount / participants
// ===================================================================

#[test]
fn batch_close_does_not_move_pool_fixed_or_participants() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    // A snapshot of the invariant fields BEFORE both closes.
    let fixed_before = env.sale_state().pool_fixed_amount;
    let participants_before = env.sale_state().participants;
    assert_eq!(
        fixed_before, UNDER_POOL,
        "precondition: pool_fixed_amount == the pool"
    );
    assert_eq!(participants_before, 2, "precondition: 2 participants");

    // close_deposits (past the claim deadline).
    env.expire_blockhash();
    send(
        &mut env,
        close_deposits_ix(
            operator.pubkey(),
            &[
                (deposit_pda(&a.pubkey(), 0), a.pubkey()),
                (deposit_pda(&b.pubkey(), 0), b.pubkey()),
            ],
        ),
        &operator,
    )
    .expect("close_deposits");

    // close_aggregates (the same window).
    env.expire_blockhash();
    send(
        &mut env,
        close_aggregates_ix(
            operator.pubkey(),
            &[
                (aggregate_pda(&a.pubkey()), a.pubkey()),
                (aggregate_pda(&b.pubkey()), b.pubkey()),
            ],
        ),
        &operator,
    )
    .expect("close_aggregates");

    // I8: neither field changed (the close_* context is not mut on sale_state).
    let st = env.sale_state();
    assert_eq!(
        st.pool_fixed_amount, fixed_before,
        "pool_fixed_amount is unchanged (I8)"
    );
    assert_eq!(
        st.participants, participants_before,
        "participants is unchanged (I8)"
    );
}

// ===================================================================
// CHECK 1 - owner ≠ program_id: a System-owned account in the slot →
// InvalidAccountInBatch
// ===================================================================

#[test]
fn close_deposits_system_owned_account_rejected() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    // b.pubkey() is an ordinary System account (a wallet), not program-owned → check 1.
    let pairs = [(b.pubkey(), a.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::InvalidAccountInBatch)),
        "a System-owned account in the slot → InvalidAccountInBatch: {r:?}"
    );
    // A's real deposit is untouched.
    assert!(env.deposit_exists(&a.pubkey(), 0), "A's deposit is intact");
}

// ===================================================================
// CHECK 2 - a wrong discriminator: a WalletAggregate in close_deposits →
// InvalidAccountInBatch
// ===================================================================

#[test]
fn close_deposits_wrong_discriminator_rejected() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    // A's aggregate is program-owned (check 1 passes), but its WalletAggregate
    // discriminator ≠ Deposit → Account::<Deposit>::try_from fails → check 2 →
    // InvalidAccountInBatch.
    let pairs = [(aggregate_pda(&a.pubkey()), a.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::InvalidAccountInBatch)),
        "a WalletAggregate in close_deposits → InvalidAccountInBatch: {r:?}"
    );
    assert!(env.aggregate_exists(&a.pubkey()), "A's aggregate is intact");
}

#[test]
fn close_deposits_sale_state_pda_rejected() {
    // The SaleState singleton is program-owned (check 1 passes) and its discriminator is
    // not Deposit → check 2. On top of that: even past the type check, SaleState is not
    // Closable.
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    let pairs = [(sale_state_pda(), a.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::InvalidAccountInBatch)),
        "the SaleState PDA in close_deposits → InvalidAccountInBatch: {r:?}"
    );
    assert!(env.sale_state_exists(), "SaleState was not closed");
}

// ===================================================================
// CHECK 3 - a PDA/body mismatch: we forge a program-owned Deposit whose body belongs
// to ANOTHER address → create_program_address(body) ≠ acc.key →
// InvalidAccountInBatch.
// ===================================================================

#[test]
fn close_deposits_pda_body_mismatch_rejected() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    // The body of A's real deposit (a valid discriminator, wallet=A, index=0).
    let body_a = env.deposit_entry(&a.pubkey(), 0);
    // We forge that same Deposit at a FOREIGN address (B's deposit address - it exists,
    // but we overwrite it with a program-owned account carrying A's body). Re-deriving
    // from the body yields the canonical address of A's deposit ≠ B's deposit address →
    // check 3.
    let forged_addr = deposit_pda(&b.pubkey(), 0);
    env.forge_program_account(&forged_addr, deposit_account_bytes(&body_a));

    // The pair: (forged_addr, wallet=A). check1 (program-owned) ✓, check2 (Deposit) ✓,
    // check3: find_program_address([deposit, A, 0]) == deposit_pda(A,0) ≠ forged_addr.
    let pairs = [(forged_addr, a.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::InvalidAccountInBatch)),
        "the body does not derive to its own address → InvalidAccountInBatch: {r:?}"
    );
    // A's real deposit (at its own canonical address) is untouched.
    assert!(env.deposit_exists(&a.pubkey(), 0), "A's deposit is intact");
    // The forged account at B's address is intact too (revert).
    assert!(
        env.account_exists(&forged_addr),
        "the forged account is intact (revert)"
    );
}

// ===================================================================
// CHECK 3 - a non-canonical address: the same seeds also have other off-curve bumps,
// each yielding its own valid address. Derivation from the body computes only the
// canonical one, so a record at any other address is rejected.
// ===================================================================

/// The highest off-curve bump below the canonical one: it yields a valid PDA for the
/// same seeds but a different address.
fn non_canonical_pda(seeds: &[&[u8]], canonical_bump: u8) -> (Pubkey, u8) {
    for b in (0..canonical_bump).rev() {
        let bump = [b];
        let mut full: Vec<&[u8]> = seeds.to_vec();
        full.push(&bump);
        if let Ok(addr) = Pubkey::create_program_address(&full, &sale::id()) {
            return (addr, b);
        }
    }
    panic!("no non-canonical off-curve bump was found");
}

#[test]
fn close_deposits_non_canonical_pda_rejected() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    // The same seed namespace as A's deposit at index 0, but with a non-canonical bump.
    let index_le = 0u64.to_le_bytes();
    let wallet_a = a.pubkey();
    let seeds: [&[u8]; 3] = [b"deposit", wallet_a.as_ref(), &index_le];
    let (canonical, canonical_bump) = Pubkey::find_program_address(&seeds, &sale::id());
    let (nc_addr, nc_bump) = non_canonical_pda(&seeds, canonical_bump);
    assert_ne!(nc_addr, canonical, "the non-canonical address differs");
    assert!(nc_bump < canonical_bump);

    // A body matching exactly those seeds: wallet = A, index = 0. The address is
    // self-consistent with its own bump, but the canonical address of those same seeds
    // is a different one.
    let body = Deposit {
        wallet: wallet_a,
        amount: UNDER_DEP,
        index: 0,
    };
    env.forge_program_account(&nc_addr, deposit_account_bytes(&body));

    let pairs = [(nc_addr, wallet_a)];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::InvalidAccountInBatch)),
        "a non-canonical address → InvalidAccountInBatch: {r:?}"
    );
    assert!(
        env.account_exists(&nc_addr),
        "the non-canonical account was not closed (revert)"
    );
    assert!(
        env.deposit_exists(&wallet_a, 0),
        "A's real deposit is intact"
    );
}

// ===================================================================
// CHECK 4 - a dead or closed account in the slot → InvalidAccountInBatch
// ===================================================================

#[test]
fn close_deposits_dead_account_rejected() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    // The empty (never created) PDA address of deposit index 5: there is no account →
    // a System-owned blank → check 1 (owner ≠ program). data_is_empty holds as well.
    let dead = deposit_pda(&a.pubkey(), 5);
    assert!(
        !env.account_exists(&dead),
        "precondition: the address is empty"
    );

    let pairs = [(dead, a.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::InvalidAccountInBatch)),
        "a dead account in the slot → InvalidAccountInBatch: {r:?}"
    );
    assert!(env.deposit_exists(&a.pubkey(), 0), "A's deposit is intact");
}

// ===================================================================
// CHECK 5 - a rent redirect: the pair (depositA, walletB) ≠ body.wallet →
// InvalidAccountInBatch
// ===================================================================

#[test]
fn close_deposits_wrong_paired_wallet_rejected() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    // depositA is paired with walletB (≠ body.wallet = A) → check 5 (the rent would go
    // to the wrong party).
    let pairs = [(deposit_pda(&a.pubkey(), 0), b.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::InvalidAccountInBatch)),
        "the rent is not body.wallet → InvalidAccountInBatch: {r:?}"
    );
    assert!(
        env.deposit_exists(&a.pubkey(), 0),
        "A's deposit is intact (the rent was not siphoned off)"
    );
}

// ===================================================================
// CHECK 6a - an odd number of remaining_accounts → InvalidAccountInBatch
// ===================================================================

#[test]
fn close_deposits_odd_account_count_rejected() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    // The builder with no pairs plus one unpaired remaining account (an odd total).
    let mut ix = close_deposits_ix(operator.pubkey(), &[]);
    ix.accounts
        .push(AccountMeta::new(deposit_pda(&a.pubkey(), 0), false));

    env.expire_blockhash();
    let r = send(&mut env, ix, &operator);
    assert_eq!(
        custom(&r),
        Some(err(SaleError::InvalidAccountInBatch)),
        "an odd remaining_accounts → InvalidAccountInBatch: {r:?}"
    );
    assert!(env.deposit_exists(&a.pubkey(), 0), "A's deposit is intact");
}

// ===================================================================
// CHECK 6b - more pairs than MAX_CLOSE_PER_BATCH → BatchTooLarge
// ===================================================================

#[test]
fn close_deposits_batch_too_large_rejected() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    // MAX+1 pairs of ONE repeated dummy key: the length check fires BEFORE the accounts
    // are validated (require pairs ≤ MAX ahead of the loop) → BatchTooLarge. Repeating a
    // single key keeps the message within the size limit (keys are deduplicated), while
    // remaining_accounts.len() stays 2×(MAX+1) = 42 for the program.
    let dummy = Pubkey::new_unique();
    let over: Vec<(Pubkey, Pubkey)> = (0..=MAX_CLOSE_PER_BATCH).map(|_| (dummy, dummy)).collect();
    assert_eq!(
        over.len(),
        MAX_CLOSE_PER_BATCH + 1,
        "precondition: MAX+1 pairs"
    );

    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &over),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::BatchTooLarge)),
        "pairs > MAX_CLOSE_PER_BATCH → BatchTooLarge: {r:?}"
    );
    assert!(
        env.deposit_exists(&a.pubkey(), 0),
        "A's real deposit is intact"
    );
}

// ===================================================================
// A DUPLICATE in the batch: [depA, walletA, depA, walletA] - the second occurrence
// fails on check 1 (after the first close the account is already System-owned) → an
// atomic rollback.
// ===================================================================

#[test]
fn close_deposits_duplicate_in_batch_reverts_atomically() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    let dep_a = deposit_pda(&a.pubkey(), 0);
    let a_before = env.lamports_of(&a.pubkey());

    // The same deposit twice: the first close makes it System-owned; the second hits
    // check 1.
    let pairs = [(dep_a, a.pubkey()), (dep_a, a.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::InvalidAccountInBatch)),
        "a duplicate in the batch → InvalidAccountInBatch: {r:?}"
    );
    // Atomicity: A's deposit SURVIVED (the first close rolled back along with the
    // second).
    assert!(
        env.deposit_exists(&a.pubkey(), 0),
        "A's deposit survived (an atomic rollback)"
    );
    assert_eq!(
        env.lamports_of(&a.pubkey()),
        a_before,
        "the rent did not move (rollback)"
    );
}

// ===================================================================
// GATE close_deposits: intake is under way (the pool is not fixed) →
// DeadlineNotPassed
// ===================================================================

#[test]
fn close_deposits_during_sale_rejected() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // The pool is collected but NOT fixed.
    build_pool_in_sale(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    assert!(
        !env.sale_state().pool_fixed,
        "precondition: the pool is not fixed"
    );

    let pairs = [(deposit_pda(&a.pubkey(), 0), a.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::DeadlineNotPassed)),
        "close_deposits during the Sale phase → DeadlineNotPassed: {r:?}"
    );
    assert!(env.deposit_exists(&a.pubkey(), 0), "A's deposit is intact");
}

// ===================================================================
// GATE close_deposits: after fixation but before the claim deadline it is rejected;
// once the deadline arrives the same call passes and closes the deposit. Both
// directions in one environment.
// ===================================================================

#[test]
fn close_deposits_rejected_until_deadline_then_closes() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // The pool is fixed and now == sale_end_date → the SaleEnded phase.
    activate_undersubscribed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    assert!(
        env.sale_state().pool_fixed,
        "precondition: the pool is fixed"
    );

    let pairs = [(deposit_pda(&a.pubkey(), 0), a.pubkey())];

    // Immediately after fixation (this used to pass here).
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::DeadlineNotPassed)),
        "close_deposits right after fixation → DeadlineNotPassed: {r:?}"
    );
    assert!(env.deposit_exists(&a.pubkey(), 0), "A's deposit is intact");

    // In the Claim phase - also rejected.
    env.warp_to(p.claim_start_date);
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::DeadlineNotPassed)),
        "close_deposits during the Claim phase → DeadlineNotPassed: {r:?}"
    );
    assert!(env.deposit_exists(&a.pubkey(), 0), "A's deposit is intact");

    // Exactly at the deadline the same call passes: the deposit is closed and the rent
    // is with wallet A.
    env.warp_to(deadline_of(&p));
    let rent_a = env.lamports_of(&deposit_pda(&a.pubkey(), 0));
    let a_before = env.lamports_of(&a.pubkey());
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    r.as_ref().expect("close_deposits at the deadline passes");
    assert!(!env.deposit_exists(&a.pubkey(), 0), "A's deposit is closed");
    assert_eq!(
        env.lamports_of(&a.pubkey()) - a_before,
        rent_a,
        "A's deposit rent → wallet A"
    );
    let ev: AccountsClosed = decode_event(&r).expect("the AccountsClosed event");
    assert_eq!(ev.count, 1, "AccountsClosed.count == 1");
    assert_eq!(
        ev.rent_returned, rent_a,
        "rent_returned == the deposit's rent"
    );
}

// ===================================================================
// GATE close_aggregates: before the deadline (the Claim phase) → DeadlineNotPassed
// ===================================================================

#[test]
fn close_aggregates_before_deadline_rejected() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_undersubscribed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(p.claim_start_date); // the Claim phase, before the deadline

    let pairs = [(aggregate_pda(&a.pubkey()), a.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_aggregates_ix(operator.pubkey(), &pairs),
        &operator,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::DeadlineNotPassed)),
        "close_aggregates before the deadline → DeadlineNotPassed: {r:?}"
    );
    assert!(env.aggregate_exists(&a.pubkey()), "A's aggregate is intact");
}

// ===================================================================
// GATE role: a non-operator → Unauthorized (both instructions)
// ===================================================================

#[test]
fn close_by_non_operator_rejected_unauthorized() {
    let mut env = TestEnv::new();
    let admin = env.actors.admin.insecure_clone(); // NOT the operator
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_and_reach_claim_closed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);

    // close_deposits with admin's signature in the operator slot → has_one operator →
    // Unauthorized.
    let dep_pairs = [(deposit_pda(&a.pubkey(), 0), a.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(admin.pubkey(), &dep_pairs),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::Unauthorized)),
        "close_deposits by a non-operator → Unauthorized: {r:?}"
    );

    // close_aggregates likewise.
    let agg_pairs = [(aggregate_pda(&a.pubkey()), a.pubkey())];
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_aggregates_ix(admin.pubkey(), &agg_pairs),
        &admin,
    );
    assert_eq!(
        custom(&r),
        Some(err(SaleError::Unauthorized)),
        "close_aggregates by a non-operator → Unauthorized: {r:?}"
    );
    assert!(env.deposit_exists(&a.pubkey(), 0), "A's deposit is intact");
    assert!(env.aggregate_exists(&a.pubkey()), "A's aggregate is intact");
}

// ===================================================================
// The deadline separator (M2): at now == claim_deadline a claim gives
// ClaimDeadlinePassed while close_aggregates passes. Both branches in one
// environment.
// ===================================================================

#[test]
fn at_exact_deadline_claim_rejected_but_close_aggregates_succeeds() {
    use sale_test_common::{claim_ix, wallet_token_ata};

    let mut env = TestEnv::new();
    let usdc = env.usdc_mint;
    let win = env.win_mint;
    let operator = env.actors.operator.insecure_clone();
    let a = env.actors.users[0].insecure_clone();
    let b = env.actors.users[1].insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    activate_undersubscribed(&mut env, &p, &[(&a, UNDER_DEP), (&b, UNDER_DEP)]);
    env.warp_to(deadline_of(&p)); // now == claim_deadline (ClaimClosed by boundary)

    // Direction 1: a claim EXACTLY at the deadline → ClaimDeadlinePassed.
    env.expire_blockhash();
    let rc = send(&mut env, claim_ix(a.pubkey(), usdc, win, false), &a);
    assert_eq!(
        custom(&rc),
        Some(err(SaleError::ClaimDeadlinePassed)),
        "a claim at claim_deadline → ClaimDeadlinePassed: {rc:?}"
    );
    assert!(
        env.aggregate_exists(&a.pubkey()),
        "A's aggregate was not closed by the claim"
    );
    assert!(
        !env.account_exists(&wallet_token_ata(&a.pubkey(), &win)),
        "A's tokens were not released (revert)"
    );

    // Direction 2: close_aggregates EXACTLY at the deadline → it passes.
    env.expire_blockhash();
    send(
        &mut env,
        close_aggregates_ix(
            operator.pubkey(),
            &[(aggregate_pda(&a.pubkey()), a.pubkey())],
        ),
        &operator,
    )
    .expect("close_aggregates at claim_deadline passes");
    assert!(
        !env.aggregate_exists(&a.pubkey()),
        "A's aggregate is closed (the same instant)"
    );

    // The undersubscribed refund_vault was never created - it does not affect the
    // separator.
    assert!(
        !env.account_exists(&refund_vault_pda()),
        "refund_vault is absent (undersubscribed)"
    );
}

// ===================================================================
// The reachable maximum batch in ONE legacy transaction (the tx-size ceiling).
// MAX_CLOSE_PER_BATCH = 20 is the PROGRAM's upper bound; the actual maximum number
// of DISTINCT pairs in one legacy tx is bounded by the 1232 B packet limit: the R6
// measurement (cu_bench.rs) shows 14 pairs = 1169 B fit while 15 = 1235 B do not.
// LiteSVM does not enforce the packet limit (17 pairs would execute here; ≥ 18 fail
// on the cost model's FILTER_SIZE - 38 static keys), so the byte boundary is checked
// by serialization in cu_bench, while this test covers EXACTLY the maximum reachable
// on mainnet in one call and asserts both the execution and the event payload for
// the whole batch. Beyond 14 distinct pairs it is a v0 tx + ALT.

/// The measured maximum number of distinct pairs in one legacy tx (cu_bench, R6).
const MAX_ACHIEVABLE_PAIRS: usize = 14;
/// The batch under test does not exceed the program's limit (checked at compile
/// time).
const _: () = assert!(MAX_ACHIEVABLE_PAIRS <= MAX_CLOSE_PER_BATCH);

#[test]
fn close_deposits_full_achievable_batch_in_one_tx() {
    let mut env = TestEnv::new();
    let operator = env.actors.operator.insecure_clone();
    let p = params_with_emission(&env, EMISSION);

    // N wallets with one $10 deposit each. A pool of N×$10 > the soft cap of $30 at
    // N ≥ 4 → oversubscribed (activate then requires refund_vault).
    init_with(&mut env, &p);
    env.warp_to(p.sale_start_date);
    let mut wallets = Vec::new();
    for _ in 0..MAX_ACHIEVABLE_PAIRS {
        let w = env.new_user();
        let ata = funded_ata(&mut env, &w.pubkey(), UNDER_DEP);
        env.expire_blockhash();
        send(&mut env, init_aggregate_ix(w.pubkey()), &w).expect("init_aggregate");
        env.expire_blockhash();
        send(&mut env, purchase_ix(w.pubkey(), ata, 0, UNDER_DEP), &w).expect("purchase");
        wallets.push(w);
    }

    env.warp_to(p.sale_end_date);
    fund_token_vault(&mut env, EMISSION); // oversubscribed → distributable = the emission
    let admin = env.actors.admin.insecure_clone();
    let usdc = env.usdc_mint;
    env.expire_blockhash();
    send(
        &mut env,
        activate_claim_ix(admin.pubkey(), usdc, Some(refund_vault_pda())),
        &admin,
    )
    .expect("activate_claim (oversubscribed)");
    env.warp_to(deadline_of(&p));

    let pairs: Vec<(Pubkey, Pubkey)> = wallets
        .iter()
        .map(|w| (deposit_pda(&w.pubkey(), 0), w.pubkey()))
        .collect();
    let total_rent: u64 = pairs.iter().map(|(d, _)| env.lamports_of(d)).sum();

    // One tx closes the whole batch of distinct pairs → both the size and the CU fit.
    env.expire_blockhash();
    let r = send(
        &mut env,
        close_deposits_ix(operator.pubkey(), &pairs),
        &operator,
    );
    r.as_ref()
        .expect("close_deposits on the reachable batch passes in one tx");

    // Every deposit is closed and the event sees the whole batch.
    for w in &wallets {
        assert!(!env.deposit_exists(&w.pubkey(), 0), "the deposit is closed");
    }
    let ev: AccountsClosed = decode_event(&r).expect("the AccountsClosed event");
    assert_eq!(
        ev.count as usize, MAX_ACHIEVABLE_PAIRS,
        "AccountsClosed.count == the whole batch"
    );
    assert_eq!(
        ev.rent_returned, total_rent,
        "rent_returned == Σ of the batch's rents"
    );
}
