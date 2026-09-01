//! The e2e environment: surfpool + the program at declare_id + actors + mints +
//! sending and confirming transactions through a real RPC.

use std::time::{Duration, Instant};

use anchor_lang::AnchorDeserialize;
use solana_commitment_config::CommitmentConfig;
use solana_instruction::Instruction;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_rpc_client::rpc_client::RpcClient;
use solana_rpc_client_api::client_error::Error as ClientError;
use solana_rpc_client_api::config::{RpcSendTransactionConfig, RpcTransactionConfig};
use solana_signature::Signature;
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use solana_transaction_error::TransactionError;
use solana_transaction_status_client_types::{
    EncodedConfirmedTransactionWithStatusMeta, UiTransactionEncoding,
};

use crate::events::{parse_sale_events, SaleEvent};
use crate::surfnet::Surfnet;

/// SOL held by each actor (as ACTOR_LAMPORTS in tests/sale): rent and fees must never
/// become a hidden cause of failures.
pub const ACTOR_LAMPORTS: u64 = 100_000_000_000;

/// The path to the program artifact - the same literal the freshness gate guards
/// (`sale_test_common::assert_artifact_fresh`): the crate keeps no copy of the path
/// of its own, or the gate could end up checking a different file from the one that
/// goes into surfpool.
const SALE_SO_PATH: &str = sale_test_common::SALE_SO_PATH;

pub struct Actors {
    pub owner: Keypair,
    pub admin: Keypair,
    pub operator: Keypair,
    pub users: Vec<Keypair>,
}

/// The e2e environment: surfpool with sale.so at declare_id (upgrade authority =
/// owner), funded actors, and the USDC (dec 6) and $1WIN (dec 8) mints.
pub struct E2e {
    pub surf: Surfnet,
    pub rpc: RpcClient,
    /// The infrastructure's fee payer and the mint authority of both mints (as in
    /// common).
    pub payer: Keypair,
    pub actors: Actors,
    pub usdc_mint: Pubkey,
    pub win_mint: Pubkey,
}

impl E2e {
    /// Brings surfpool up on `port`, stops the slot ticker when `paused` (deterministic
    /// time through warp_to), seats sale.so at declare_id, funds the actors and creates
    /// the mints with genuine instructions.
    ///
    /// The artifact freshness gate is passed BEFORE surfpool starts: a stale `sale.so`
    /// would mean running against the previous version of the program, and there is no
    /// point spending a minute of validator time on that.
    pub fn bootstrap(port: u16, users: usize, paused: bool) -> Self {
        sale_test_common::assert_artifact_fresh();

        let surf = Surfnet::start(port);

        let rpc = RpcClient::new_with_timeout_and_commitment(
            surf.rpc_url.clone(),
            Duration::from_secs(30),
            CommitmentConfig::processed(),
        );

        let payer = Keypair::new();
        let owner = Keypair::new();
        let admin = Keypair::new();
        let operator = Keypair::new();
        let users: Vec<Keypair> = (0..users).map(|_| Keypair::new()).collect();

        let env = Self {
            surf,
            rpc,
            payer,
            actors: Actors {
                owner,
                admin,
                operator,
                users,
            },
            usdc_mint: Pubkey::default(),
            win_mint: Pubkey::default(),
        };

        for kp in env.actors.users.iter().chain([
            &env.actors.owner,
            &env.actors.admin,
            &env.actors.operator,
            &env.payer,
        ]) {
            env.airdrop(&kp.pubkey(), ACTOR_LAMPORTS);
        }

        env.surf.deploy_program_as(
            &sale::id(),
            SALE_SO_PATH,
            &env.payer,
            &env.actors.owner.pubkey(),
        );
        // Proof that what executes at declare_id is sale.so itself (rather than surfpool's
        // program-cache stub) - taken before the clock is stopped.
        env.wait_program_dispatch();
        if paused {
            env.surf.pause_clock();
        }

        let usdc = env.create_mint(6);
        let win = env.create_mint(8);
        Self {
            usdc_mint: usdc,
            win_mint: win,
            ..env
        }
    }

    /// Waits until sale.so is what executes at the declare_id address: simulating an
    /// instruction with empty data must fail with an Anchor dispatcher code (surfpool's
    /// stub answers with success - which means "not loaded yet").
    fn wait_program_dispatch(&self) {
        let probe = Instruction {
            program_id: sale::id(),
            accounts: vec![],
            data: vec![],
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let tx = self.build_tx(
                std::slice::from_ref(&probe),
                &self.payer.pubkey(),
                &[&self.payer],
            );
            let sim = self
                .rpc
                .simulate_transaction(&tx)
                .expect("simulate_transaction")
                .value;
            if let Some(TransactionError::InstructionError(
                _,
                solana_instruction_error::InstructionError::Custom(code),
            )) = sim.err.clone().map(TransactionError::from)
            {
                // the code was born inside sale.so's dispatch - the program is loaded
                assert_eq!(
                    code,
                    anchor_lang::error::ErrorCode::InstructionFallbackNotFound as u32,
                    "an unexpected dispatch code"
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "sale.so did not become executable within 15s (logs: {:?})",
                sim.logs
            );
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    pub fn now(&self) -> i64 {
        self.surf.clock().unix_timestamp
    }

    pub fn warp_to(&self, unix_ts: i64) {
        self.surf.warp_to(unix_ts);
    }

    // ---------- Transactions ----------

    pub fn build_tx(
        &self,
        ixs: &[Instruction],
        payer: &Pubkey,
        signers: &[&Keypair],
    ) -> VersionedTransaction {
        let blockhash = self.rpc.get_latest_blockhash().expect("latest blockhash");
        let msg = Message::new_with_blockhash(ixs, Some(payer), &blockhash);
        VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &signers.to_vec())
            .expect("transaction signatures")
    }

    /// Sends with preflight and waits for the transaction to show up in getTransaction;
    /// panics on any error. Returns the signature - the events and CU are asserted
    /// against it.
    pub fn send(&self, ixs: &[Instruction], payer: &Pubkey, signers: &[&Keypair]) -> Signature {
        let tx = self.build_tx(ixs, payer, signers);
        self.send_prebuilt(&tx)
    }

    /// Same as [`Self::send`], but for a pre-assembled transaction (size measurement).
    pub fn send_prebuilt(&self, tx: &VersionedTransaction) -> Signature {
        let sig = self
            .rpc
            .send_transaction_with_config(
                tx,
                RpcSendTransactionConfig {
                    skip_preflight: false,
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("the transaction was rejected: {e}"));
        let confirmed = self.get_tx(&sig);
        let meta = confirmed.transaction.meta.as_ref().expect("meta");
        assert!(
            meta.err.is_none(),
            "the transaction failed on-chain: {:?}",
            meta.err
        );
        sig
    }

    /// Sends a transaction EXPECTING a rejection with a specific
    /// `InstructionError::Custom` code (the Anchor dispatcher codes and SaleError).
    pub fn send_expect_custom(
        &self,
        ixs: &[Instruction],
        payer: &Pubkey,
        signers: &[&Keypair],
        expected_code: u32,
    ) {
        let tx = self.build_tx(ixs, payer, signers);
        let err = self
            .rpc
            .send_transaction_with_config(
                &tx,
                RpcSendTransactionConfig {
                    skip_preflight: false,
                    ..Default::default()
                },
            )
            .expect_err("the transaction was supposed to fail");
        let code =
            custom_code(&err).unwrap_or_else(|| panic!("no Custom code in the error: {err}"));
        assert_eq!(code, expected_code, "error code: {err}");
    }

    /// A confirmed transaction with its meta (json encoding - the way an indexer reads
    /// it).
    pub fn get_tx(&self, sig: &Signature) -> EncodedConfirmedTransactionWithStatusMeta {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let cfg = RpcTransactionConfig {
                encoding: Some(UiTransactionEncoding::Json),
                commitment: Some(CommitmentConfig::confirmed()),
                max_supported_transaction_version: Some(0),
            };
            match self.rpc.get_transaction_with_config(sig, cfg) {
                Ok(tx) => return tx,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => panic!("transaction {sig} was not confirmed within 15s: {e}"),
            }
        }
    }

    pub fn events_of(&self, sig: &Signature) -> Vec<SaleEvent> {
        parse_sale_events(&self.get_tx(sig))
    }

    /// Exactly one lifecycle event in the transaction (the usual Sale case).
    pub fn single_event(&self, sig: &Signature) -> SaleEvent {
        let mut events = self.events_of(sig);
        if events.len() != 1 {
            panic!(
                "one event was expected, got {} ({:?}); raw tx: {}",
                events.len(),
                events.iter().map(|e| e.name()).collect::<Vec<_>>(),
                serde_json::to_string(&self.get_tx(sig)).unwrap_or_default()
            );
        }
        events.remove(0)
    }

    // ---------- Actors / SOL ----------

    pub fn airdrop(&self, to: &Pubkey, lamports: u64) {
        let before = self.lamports(to);
        self.rpc.request_airdrop(to, lamports).expect("airdrop");
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.lamports(to) < before + lamports {
            assert!(
                Instant::now() < deadline,
                "the airdrop did not arrive within 10s"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn lamports(&self, addr: &Pubkey) -> u64 {
        self.rpc.get_balance(addr).expect("get_balance")
    }

    pub fn account_exists(&self, addr: &Pubkey) -> bool {
        self.rpc
            .get_account_with_commitment(addr, CommitmentConfig::processed())
            .expect("get_account")
            .value
            .is_some()
    }

    // ---------- SPL: mints, ATAs, balances ----------

    /// Creates an SPL mint with genuine instructions (mint authority = payer).
    pub fn create_mint(&self, decimals: u8) -> Pubkey {
        let mint = Keypair::new();
        let rent = self
            .rpc
            .get_minimum_balance_for_rent_exemption(spl_token_interface::state::Mint::LEN)
            .expect("rent for mint");
        let create_ix = solana_system_interface::instruction::create_account(
            &self.payer.pubkey(),
            &mint.pubkey(),
            rent,
            spl_token_interface::state::Mint::LEN as u64,
            &spl_token_interface::id(),
        );
        let init_ix = spl_token_interface::instruction::initialize_mint2(
            &spl_token_interface::id(),
            &mint.pubkey(),
            &self.payer.pubkey(),
            None,
            decimals,
        )
        .expect("initialize_mint2");
        self.send(
            &[create_ix, init_ix],
            &self.payer.pubkey(),
            &[&self.payer, &mint],
        );
        mint.pubkey()
    }

    pub fn create_ata(&self, wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
        let ix =
            spl_associated_token_account_interface::instruction::create_associated_token_account(
                &self.payer.pubkey(),
                wallet,
                mint,
                &spl_token_interface::id(),
            );
        self.send(&[ix], &self.payer.pubkey(), &[&self.payer]);
        spl_associated_token_account_interface::address::get_associated_token_address(wallet, mint)
    }

    pub fn mint_to_ix(&self, mint: &Pubkey, dest: &Pubkey, amount: u64) -> Instruction {
        spl_token_interface::instruction::mint_to(
            &spl_token_interface::id(),
            mint,
            dest,
            &self.payer.pubkey(),
            &[],
            amount,
        )
        .expect("mint_to ix")
    }

    pub fn mint_to(&self, mint: &Pubkey, dest: &Pubkey, amount: u64) {
        let ix = self.mint_to_ix(mint, dest, amount);
        self.send(&[ix], &self.payer.pubkey(), &[&self.payer]);
    }

    /// A token account's balance; panics if it is absent (fail-closed, as in common).
    pub fn token_balance(&self, token_account: &Pubkey) -> u64 {
        let acc = self
            .rpc
            .get_account(token_account)
            .unwrap_or_else(|e| panic!("token account {token_account} does not exist: {e}"));
        spl_token_interface::state::Account::unpack(&acc.data)
            .unwrap_or_else(|e| panic!("{token_account} is not an SPL token account: {e}"))
            .amount
    }

    // ---------- State readers ----------

    pub fn sale_state(&self) -> sale::state::SaleState {
        self.read_anchor(&sale_test_common::sale_state_pda())
    }

    pub fn wallet_aggregate(&self, wallet: &Pubkey) -> sale::state::WalletAggregate {
        self.read_anchor(&sale_test_common::aggregate_pda(wallet))
    }

    fn read_anchor<T: AnchorDeserialize>(&self, addr: &Pubkey) -> T {
        let acc = self
            .rpc
            .get_account(addr)
            .unwrap_or_else(|e| panic!("account {addr} does not exist: {e}"));
        T::deserialize(&mut &acc.data[8..]).expect("the Anchor account deserializes")
    }
}

/// Extracts `Custom(code)` from a client error (preflight or execution).
pub fn custom_code(err: &ClientError) -> Option<u32> {
    match err.get_transaction_error() {
        Some(TransactionError::InstructionError(
            _,
            solana_instruction_error::InstructionError::Custom(code),
        )) => Some(code),
        _ => None,
    }
}
