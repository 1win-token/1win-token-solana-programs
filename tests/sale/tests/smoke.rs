//! Infrastructure smoke tests - the executable form of the R3 spike.
//!
//! They prove: the `.so` loads and EXECUTES under the ProgramData fixture of §9.1;
//! the behaviour of the Anchor 1.0.2 dispatcher is pinned by specific codes; and the
//! clock, blockhash and token helpers work as advertised.

use anchor_lang::error::ErrorCode;
use sale_test_common::{custom_error_code, TestEnv, ACTOR_LAMPORTS, SLOTS_PER_SECOND};
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_loader_v3_interface::state::UpgradeableLoaderState;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_sdk_ids::bpf_loader_upgradeable;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;
use spl_token_interface::state::Mint;

// ---------- The ProgramData fixture (§9.1) ----------

#[test]
fn program_account_matches_upgradeable_loader_layout() {
    let env = TestEnv::new();

    let program = env
        .svm
        .get_account(&sale::id())
        .expect("the program account exists");
    assert!(program.executable, "the program account is executable");
    assert_eq!(
        program.owner,
        bpf_loader_upgradeable::id(),
        "the program account's owner is the BPF Loader Upgradeable"
    );

    let state: UpgradeableLoaderState =
        bincode::deserialize(&program.data).expect("the data parses as UpgradeableLoaderState");
    match state {
        UpgradeableLoaderState::Program {
            programdata_address,
        } => assert_eq!(
            programdata_address,
            TestEnv::programdata_address(),
            "the pointer to ProgramData is the canonical PDA"
        ),
        other => panic!("UpgradeableLoaderState::Program was expected, got {other:?}"),
    }
}

#[test]
fn programdata_fixture_parses_with_expected_upgrade_authority() {
    let env = TestEnv::new();

    let programdata = env
        .svm
        .get_account(&TestEnv::programdata_address())
        .expect("ProgramData exists at the canonical PDA");
    assert_eq!(programdata.owner, bpf_loader_upgradeable::id());

    let metadata_len = UpgradeableLoaderState::size_of_programdata_metadata();
    assert!(
        programdata.data.len() > metadata_len,
        "the ELF lies behind the ProgramData metadata"
    );

    // bincode reads the metadata from the start of the buffer and the ELF tail does not
    // interfere - the same way Anchor and agave deserialize ProgramData.
    let state: UpgradeableLoaderState =
        bincode::deserialize(&programdata.data).expect("the ProgramData metadata parses");
    match state {
        UpgradeableLoaderState::ProgramData {
            upgrade_authority_address,
            ..
        } => assert_eq!(
            upgrade_authority_address,
            Some(env.actors.owner.pubkey()),
            "the default upgrade authority is the owner actor"
        ),
        other => panic!("UpgradeableLoaderState::ProgramData was expected, got {other:?}"),
    }
}

#[test]
fn programdata_fixture_honors_custom_upgrade_authority() {
    let custom = Pubkey::new_unique();
    let env = TestEnv::with_upgrade_authority(custom);

    let programdata = env
        .svm
        .get_account(&TestEnv::programdata_address())
        .expect("ProgramData exists");
    let state: UpgradeableLoaderState =
        bincode::deserialize(&programdata.data).expect("the metadata parses");
    match state {
        UpgradeableLoaderState::ProgramData {
            upgrade_authority_address,
            ..
        } => assert_eq!(upgrade_authority_address, Some(custom)),
        other => panic!("ProgramData was expected, got {other:?}"),
    }
}

// ---------- The Anchor 1.0.2 dispatcher (behaviour canaries) ----------

#[test]
fn unknown_discriminator_fails_with_instruction_fallback_not_found() {
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();

    // A non-existent 8-byte discriminator. A program without a fallback function must
    // answer InstructionFallbackNotFound - and receiving that code is itself proof that
    // sale.so EXECUTED under the ProgramData fixture (the code is born in the dispatch
    // inside the program, not in the runner).
    let ix = Instruction::new_with_bytes(sale::id(), &[0xAB; 8], vec![]);
    let result = env.send_ixs(&[ix], &user.pubkey(), &[&user]);

    let expected = u32::from(ErrorCode::InstructionFallbackNotFound);
    assert_eq!(
        expected, 101,
        "the nominal value of the Anchor 1.0.2 dispatcher code"
    );
    assert_eq!(
        custom_error_code(&result),
        Some(expected),
        "an unknown discriminator → Custom(101), the full result: {result:?}"
    );
}

#[test]
fn short_instruction_data_fails_with_instruction_fallback_not_found() {
    let mut env = TestEnv::new();
    let user = env.actors.users[0].insecure_clone();

    // A DISPATCHER CANARY. On Anchor 1.0.2 the dispatch compares the data through
    // `data.starts_with(DISCRIMINATOR)` for every instruction and, finding no match,
    // falls into the fallback → InstructionFallbackNotFound (101); there is NO separate
    // "fewer than 8 bytes of data" branch (InstructionMissing = 100). If this test fails
    // after an Anchor bump, the dispatch itself changed: revisit every negative around
    // the 100/101/102 codes deliberately instead of patching the assertion to fit the
    // new code.
    for data in [&[][..], &[0xAB, 0xCD, 0xEF, 0x01][..]] {
        let ix = Instruction::new_with_bytes(sale::id(), data, vec![]);
        let result = env.send_ixs(&[ix], &user.pubkey(), &[&user]);
        assert_eq!(
            custom_error_code(&result),
            Some(u32::from(ErrorCode::InstructionFallbackNotFound)),
            "data of {} bytes → Custom(101), the full result: {result:?}",
            data.len()
        );
    }
}

// ---------- Clock helpers ----------

#[test]
fn warp_to_sets_timestamp_and_advances_slot_deterministically() {
    let mut env = TestEnv::new();
    let before = env.clock();

    let target = before.unix_timestamp + 1_000;
    env.warp_to(target);

    let after = env.clock();
    assert_eq!(
        after.unix_timestamp, target,
        "the timestamp was set exactly"
    );
    // warp_to's determinism contract: exactly SLOTS_PER_SECOND slots per second.
    assert_eq!(
        after.slot,
        before.slot + 1_000 * SLOTS_PER_SECOND,
        "the slot grows consistently with the timestamp"
    );

    // A repeated forward warp - monotonicity holds.
    env.warp_to(target + 50);
    let final_clock = env.clock();
    assert_eq!(final_clock.unix_timestamp, target + 50);
    assert!(final_clock.slot > after.slot, "the slot grows strictly");
}

#[test]
#[should_panic(expected = "warp_to only moves forward")]
fn warp_to_rejects_time_rollback() {
    let mut env = TestEnv::new();
    let now = env.now();
    env.warp_to(now + 100);
    env.warp_to(now + 50); // backwards relative to the current time - a panic
}

#[test]
fn expire_blockhash_rotates_latest_blockhash() {
    let mut env = TestEnv::new();
    let before = env.svm.latest_blockhash();
    env.expire_blockhash();
    assert_ne!(
        env.svm.latest_blockhash(),
        before,
        "expire_blockhash changes latest_blockhash"
    );
}

// ---------- Mints and token helpers ----------

#[test]
fn mints_created_with_spec_decimals_and_test_authority() {
    let env = TestEnv::new();

    let usdc = env.svm.get_account(&env.usdc_mint).expect("the USDC mint");
    let usdc_mint = Mint::unpack(&usdc.data).expect("parses as a Mint");
    assert_eq!(usdc_mint.decimals, 6, "USDC: 6 decimals (TRD)");

    let win = env.svm.get_account(&env.win_mint).expect("the 1WIN mint");
    let win_mint = Mint::unpack(&win.data).expect("parses as a Mint");
    assert_eq!(win_mint.decimals, 8, "$1WIN: 8 decimals (TRD)");

    for (label, mint) in [("USDC", &usdc_mint), ("1WIN", &win_mint)] {
        assert!(mint.is_initialized, "{label}: the mint is initialized");
        assert_eq!(
            Option::from(mint.mint_authority),
            Some(env.payer.pubkey()),
            "{label}: the mint authority is the environment's payer"
        );
        assert!(
            mint.freeze_authority.is_none(),
            "{label}: there is no freeze authority"
        );
    }
}

#[test]
fn mint_to_and_token_balance_roundtrip() {
    let mut env = TestEnv::new();
    let wallet = env.actors.users[0].pubkey();
    let (usdc_mint, win_mint) = (env.usdc_mint, env.win_mint);

    let usdc_ata = env.create_ata(&wallet, &usdc_mint);
    env.mint_to(&usdc_mint, &usdc_ata, 1_234_567);
    assert_eq!(env.token_balance(&usdc_ata), 1_234_567);

    let win_ata = env.create_ata(&wallet, &win_mint);
    env.mint_to(&win_mint, &win_ata, 5_000_000_000);
    assert_eq!(env.token_balance(&win_ata), 5_000_000_000);
}

// ---------- Actors ----------

#[test]
fn actors_and_new_user_are_funded() {
    let mut env = TestEnv::new();

    // The roles and users spend nothing when the environment is created - their balance
    // is exact.
    for kp in [&env.actors.owner, &env.actors.admin, &env.actors.operator]
        .into_iter()
        .chain(env.actors.users.iter())
    {
        assert_eq!(
            env.svm.get_balance(&kp.pubkey()),
            Some(ACTOR_LAMPORTS),
            "the actor is funded"
        );
    }

    // In the constructor the payer pays the rent of two mints and the fees - its balance
    // is below the exact nominal but stays "practically full".
    let payer_balance = env
        .svm
        .get_balance(&env.payer.pubkey())
        .expect("the payer exists");
    assert!(
        payer_balance > ACTOR_LAMPORTS - 100_000_000,
        "the payer is funded (net of the mints' rent): {payer_balance}"
    );

    let extra: Keypair = env.new_user();
    assert_eq!(env.svm.get_balance(&extra.pubkey()), Some(ACTOR_LAMPORTS));
}

// ---------- LiteSVM replay semantics ----------

#[test]
fn identical_tx_replay_rejected_until_blockhash_expires() {
    let mut env = TestEnv::new();
    let from = env.actors.users[0].insecure_clone();
    let to = env.actors.users[1].pubkey();
    let transfer =
        || solana_system_interface::instruction::transfer(&from.pubkey(), &to, 1_000_000);

    // The first send - it executes.
    env.send_ixs(&[transfer()], &from.pubkey(), &[&from])
        .expect("the first send passes");

    // The same blockhash plus the same signers ⇒ a byte-for-byte identical tx: LiteSVM's
    // history rejects it as AlreadyProcessed BEFORE execution. That is precisely why
    // replay tests of the program's guards MUST refresh the blockhash - otherwise they
    // verify the runner's deduplication rather than the guard.
    let replayed = env
        .send_ixs(&[transfer()], &from.pubkey(), &[&from])
        .expect_err("an identical tx without expire_blockhash is rejected");
    assert_eq!(replayed.err, TransactionError::AlreadyProcessed);

    // A fresh blockhash ⇒ a new signature ⇒ the transaction reaches execution.
    env.expire_blockhash();
    env.send_ixs(&[transfer()], &from.pubkey(), &[&from])
        .expect("after expire_blockhash the repeat executes");
}
