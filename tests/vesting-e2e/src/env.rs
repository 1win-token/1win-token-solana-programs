//! The e2e environment: surfpool + the program at declare_id + actors + the $1WIN
//! mint + sending and confirming transactions through a real RPC.

// DUP: to be merged into svm-core (follow-up) - the module's structure was copied
// from tests/sale-e2e/src/env.rs; the differences are the program
// under test (vesting.so), the set of actors (Vesting has no admin role, Arch §8), a
// single mint instead of two, and the Vesting state readers.

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

use crate::events::{parse_vesting_events, VestingEvent};
use crate::surfnet::Surfnet;

/// SOL held by each actor (as ACTOR_LAMPORTS in tests/vesting): rent and fees must
/// never become a hidden cause of failures.
pub const ACTOR_LAMPORTS: u64 = 100_000_000_000;

/// The decimals of $1WIN (Arch §10.1). A literal rather than the program's constant:
/// there is no point checking it against itself.
pub const WIN_DECIMALS: u8 = 8;

/// Solana's packet size limit in bytes: a larger transaction never reaches the
/// validator at all.
pub const PACKET_LIMIT: usize = 1232;

/// The path to the program artifact - the same literal the freshness gate guards
/// (`vesting_test_common::assert_artifact_fresh`): the crate keeps no copy of the
/// path of its own, or the gate could end up checking a different file from the one
/// that goes into surfpool.
const VESTING_SO_PATH: &str = vesting_test_common::VESTING_SO_PATH;

/// The actors of the Vesting role model (Arch §8) plus the activity recipients.
pub struct Actors {
    /// Owner: `initialize` (it is also the environment's upgrade authority), the roles,
    /// and creating / cancelling / closing activities.
    pub owner: Keypair,
    /// Admin: the TGE date and an activity's recipient.
    pub admin: Keypair,
    /// Operator: claims on behalf of a recipient.
    pub operator: Keypair,
    /// The config's `withdrawal_authority`: the single target for the remainder and the
    /// rent (I12) - it is both the `rent_collector` and the owner of
    /// `beneficiary_token`.
    pub withdrawal: Keypair,
    /// The activity recipients - one per bucket of the scenario.
    pub recipients: Vec<Keypair>,
}

/// The e2e environment: surfpool with vesting.so at declare_id (upgrade authority =
/// owner), funded actors and the $1WIN mint (8 decimals, mint authority = `payer`).
pub struct E2e {
    pub surf: Surfnet,
    pub rpc: RpcClient,
    /// The infrastructure's fee payer and the $1WIN mint authority (in production the
    /// mint is external; owning the authority in a test is a deliberate simplification,
    /// as in tests/vesting).
    pub payer: Keypair,
    pub actors: Actors,
    pub win_mint: Pubkey,
}

impl E2e {
    /// Brings surfpool up on `port`, stops the slot ticker when `paused` (deterministic
    /// time through warp_to), seats vesting.so at declare_id, funds the actors and
    /// creates $1WIN with genuine instructions.
    ///
    /// The artifact freshness gate is passed BEFORE surfpool starts: a stale
    /// `vesting.so` would mean running against the previous version of the program, and
    /// there is no point spending a minute of validator time on that.
    pub fn bootstrap(port: u16, recipients: usize, paused: bool) -> Self {
        vesting_test_common::assert_artifact_fresh();

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
        let withdrawal = Keypair::new();
        let recipients: Vec<Keypair> = (0..recipients).map(|_| Keypair::new()).collect();

        let env = Self {
            surf,
            rpc,
            payer,
            actors: Actors {
                owner,
                admin,
                operator,
                withdrawal,
                recipients,
            },
            win_mint: Pubkey::default(),
        };

        for kp in env.actors.recipients.iter().chain([
            &env.actors.owner,
            &env.actors.admin,
            &env.actors.operator,
            &env.actors.withdrawal,
            &env.payer,
        ]) {
            env.airdrop(&kp.pubkey(), ACTOR_LAMPORTS);
        }

        env.surf.deploy_program_as(
            &vesting::id(),
            VESTING_SO_PATH,
            &env.payer,
            &env.actors.owner.pubkey(),
        );
        // Proof that what executes at declare_id is vesting.so itself (rather than
        // surfpool's program-cache stub) - taken before the clock is stopped.
        env.wait_program_dispatch();
        if paused {
            env.surf.pause_clock();
        }

        let win_mint = env.create_mint(WIN_DECIMALS);
        Self { win_mint, ..env }
    }

    /// Waits until vesting.so is what executes at the declare_id address: simulating an
    /// instruction with empty data must fail with an Anchor dispatcher code (surfpool's
    /// stub answers with success - which means "not loaded yet").
    fn wait_program_dispatch(&self) {
        let probe = Instruction {
            program_id: vesting::id(),
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
                // the code was born inside vesting.so's dispatch - the program is loaded
                assert_eq!(
                    code,
                    anchor_lang::error::ErrorCode::InstructionFallbackNotFound as u32,
                    "an unexpected dispatch code"
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "vesting.so did not become executable within 15s (logs: {:?})",
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

    /// Warps the Clock to day `day` of the schedule of an activity whose TGE is at
    /// `tge_ts` (Arch §10.1, day 1 = TGE itself). Returns the timestamp that was set.
    /// `DAY_SECS` comes from the reference crate - as in the LiteSVM suite.
    pub fn warp_to_day(&self, tge_ts: i64, day: i64) -> i64 {
        assert!(day >= 1, "schedule day numbering starts at 1: {day}");
        let target = (day - 1)
            .checked_mul(vesting_expected::vesting::DAY_SECS)
            .and_then(|offset| tge_ts.checked_add(offset))
            .unwrap_or_else(|| panic!("day {day} from tge_ts {tge_ts} does not fit into i64"));
        self.warp_to(target);
        target
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
    /// `InstructionError::Custom` code (the Anchor dispatcher codes, VestingError and the
    /// SPL codes).
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

    /// The serialized transaction's size in BYTES - against [`PACKET_LIMIT`]. It is
    /// measured on the very transaction that goes to the RPC.
    pub fn tx_size(tx: &VersionedTransaction) -> usize {
        bincode::serialize(tx)
            .expect("transaction serialization")
            .len()
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

    /// The compute units a transaction spent (for the run's record).
    pub fn compute_units(&self, sig: &Signature) -> Option<u64> {
        Option::<u64>::from(
            self.get_tx(sig)
                .transaction
                .meta
                .expect("meta")
                .compute_units_consumed,
        )
    }

    pub fn events_of(&self, sig: &Signature) -> Vec<VestingEvent> {
        parse_vesting_events(&self.get_tx(sig))
    }

    /// Exactly one event in the transaction (the usual Vesting case: one program
    /// instruction per transaction emits one event).
    pub fn single_event(&self, sig: &Signature) -> VestingEvent {
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

    // ---------- SPL: the mint, ATAs, balances ----------

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

    /// A token account's balance; panics if it is absent (fail-closed, as in
    /// tests/vesting).
    pub fn token_balance(&self, token_account: &Pubkey) -> u64 {
        let acc = self
            .rpc
            .get_account(token_account)
            .unwrap_or_else(|e| panic!("token account {token_account} does not exist: {e}"));
        spl_token_interface::state::Account::unpack(&acc.data)
            .unwrap_or_else(|e| panic!("{token_account} is not an SPL token account: {e}"))
            .amount
    }

    /// A recipient's ATA balance under $1WIN; 0 if the account does not exist yet (the
    /// distinction between "no account" and "an empty account" is carried by
    /// [`Self::account_exists`]).
    pub fn recipient_ata_balance(&self, recipient: &Pubkey) -> u64 {
        let ata = vesting_test_common::recipient_ata(recipient, &self.win_mint);
        if self.account_exists(&ata) {
            self.token_balance(&ata)
        } else {
            0
        }
    }

    fn mint_state(&self, mint: &Pubkey) -> spl_token_interface::state::Mint {
        let acc = self
            .rpc
            .get_account(mint)
            .unwrap_or_else(|e| panic!("mint {mint} does not exist: {e}"));
        spl_token_interface::state::Mint::unpack(&acc.data)
            .unwrap_or_else(|e| panic!("{mint} is not an SPL mint: {e}"))
    }

    /// The mint's `mint_authority`: `None` after `SetAuthority(MintTokens, None)`.
    pub fn mint_authority(&self, mint: &Pubkey) -> Option<Pubkey> {
        self.mint_state(mint).mint_authority.into()
    }

    /// The mint's `supply` - how much $1WIN has been issued in total.
    pub fn mint_supply(&self, mint: &Pubkey) -> u64 {
        self.mint_state(mint).supply
    }

    // ---------- State readers ----------

    pub fn vesting_config(&self) -> vesting::state::VestingConfig {
        self.read_anchor(&vesting_test_common::config_pda())
    }

    pub fn vesting(&self, activity_id: u64) -> vesting::state::Vesting {
        self.read_anchor(&vesting_test_common::vesting_pda(activity_id))
    }

    /// An activity vault's balance in token units (it panics if the vault is absent - the
    /// fact of closing is checked by [`Self::account_exists`]).
    pub fn vault_balance(&self, activity_id: u64) -> u64 {
        self.token_balance(&vesting_test_common::vault_pda(activity_id))
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
