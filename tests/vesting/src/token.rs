//! Token helpers: classic SPL Token plus ATA (Token-2022 is forbidden by the spec -
//! a mint under it is created only as the subject of a negative test).
//!
//! Mints and ATAs are created by GENUINE spl-token / ATA instructions against the
//! programs built into LiteSVM (`spl_token-3.5.0`,
//! `spl_associated_token_account-1.1.1`) - the same path production clients take.
//! `set_account` with Pack serialization is kept for state unreachable by honest
//! means.

// DUP: to be merged into svm-core (follow-up). The module is a copy of
// tests/sale/src/token.rs; the differences: `forge_program_account`
// writes the Vesting program as the owner, and `create_token2022_mint` was added.

use crate::env::TestEnv;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_token_interface::state::{Account as TokenAccount, Mint};

/// The Token-2022 program id - the program is built into LiteSVM
/// (`litesvm::programs`). A literal rather than a crate constant: `solana_sdk_ids`
/// does not carry it, and a dependency on `spl-token-2022` for a single address
/// costs more than the address itself.
pub const TOKEN_2022_ID: Pubkey =
    Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

impl TestEnv {
    /// Creates the ATA of `wallet` for `mint` (the payer is the environment's `payer`)
    /// and returns its address.
    pub fn create_ata(&mut self, wallet: &Pubkey, mint: &Pubkey) -> Pubkey {
        let payer = self.payer.insecure_clone();
        let ix =
            spl_associated_token_account_interface::instruction::create_associated_token_account(
                &payer.pubkey(),
                wallet,
                mint,
                &spl_token_interface::id(),
            );
        self.send_ixs(&[ix], &payer.pubkey(), &[&payer])
            .expect("ATA creation");
        spl_associated_token_account_interface::address::get_associated_token_address(wallet, mint)
    }

    /// Mints `amount` units into the `destination` token account (the mint authority is
    /// the environment's `payer`).
    pub fn mint_to(&mut self, mint: &Pubkey, destination: &Pubkey, amount: u64) {
        let payer = self.payer.insecure_clone();
        let ix = spl_token_interface::instruction::mint_to(
            &spl_token_interface::id(),
            mint,
            destination,
            &payer.pubkey(),
            &[],
            amount,
        )
        .expect("the mint_to instruction");
        self.send_ixs(&[ix], &payer.pubkey(), &[&payer])
            .expect("mint_to");
    }

    /// Creates an extra SPL mint with an arbitrary `decimals` (the mint authority is the
    /// environment's `payer`). Used by the `InvalidMintDecimals` negative: a mint with
    /// decimals != 8.
    pub fn create_mint_for_test(&mut self, decimals: u8) -> Pubkey {
        let payer = self.payer.insecure_clone();
        create_mint(&mut self.svm, &payer, decimals)
    }

    /// Creates a plain mint (no extensions) under the **Token-2022** program using
    /// genuine instructions against the `spl_token_2022` built into LiteSVM.
    ///
    /// Used by the positive test of the fact that Token-2022 is forbidden by the spec,
    /// and that what rejects it is not a check in the body but the
    /// `anchor_spl::token::Mint` type - it requires the `spl_token` owner.
    ///
    /// The `InitializeMint2` encoding and the size of a plain mint are identical in both
    /// programs, so the data is assembled by the classic interface's builder and the
    /// program id is swapped afterwards: the builder itself refuses a foreign program id
    /// (`IncorrectProgramId`).
    pub fn create_token2022_mint(&mut self, decimals: u8) -> Pubkey {
        let payer = self.payer.insecure_clone();
        let mint = Keypair::new();
        let rent = self.svm.minimum_balance_for_rent_exemption(Mint::LEN);
        let create_ix = solana_system_interface::instruction::create_account(
            &payer.pubkey(),
            &mint.pubkey(),
            rent,
            Mint::LEN as u64,
            &TOKEN_2022_ID,
        );
        let mut init_ix = spl_token_interface::instruction::initialize_mint2(
            &spl_token_interface::id(),
            &mint.pubkey(),
            &payer.pubkey(),
            None,
            decimals,
        )
        .expect("the initialize_mint2 instruction");
        init_ix.program_id = TOKEN_2022_ID;
        self.send_ixs(&[create_ix, init_ix], &payer.pubkey(), &[&payer, &mint])
            .expect("Token-2022 mint creation");
        mint.pubkey()
    }

    /// Forges the balance of an SPL token account. Needed for the
    /// `InsufficientVaultBalance` negative: an activity vault cannot be understated by
    /// honest means - the only ways out of it are a claim or a cancellation, and both
    /// stay consistent with the bookkeeping.
    pub fn set_token_amount(&mut self, token_account: &Pubkey, amount: u64) {
        let mut acc = self
            .svm
            .get_account(token_account)
            .unwrap_or_else(|| panic!("token account {token_account} does not exist"));
        let mut parsed = TokenAccount::unpack(&acc.data)
            .unwrap_or_else(|e| panic!("{token_account} is not an SPL token account: {e}"));
        parsed.amount = amount;
        TokenAccount::pack(parsed, &mut acc.data).expect("re-pack token account");
        self.svm
            .set_account(*token_account, acc)
            .expect("set_account(token)");
    }

    /// Forges an account at address `address`, OWNED by the Vesting program, holding raw
    /// `data` (usually `discriminator || Borsh(body)`). The rent-exempt lamport balance
    /// is derived from the data length.
    ///
    /// Needed for state unreachable by honest means: a decoy program-owned account
    /// carrying a valid `Vesting` discriminator but a body or an address that does not
    /// derive from the canonical seeds (PDAs are only ever initialized at their
    /// canonical address), and forging the config bypassing `initialize`.
    pub fn forge_program_account(&mut self, address: &Pubkey, data: Vec<u8>) {
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        let account = solana_account::Account {
            lamports,
            data,
            owner: vesting::id(),
            executable: false,
            rent_epoch: 0,
        };
        self.svm
            .set_account(*address, account)
            .expect("set_account(forged program account)");
    }

    /// The account's lamports (for the "rent landed on rent_collector" assertions). 0 if
    /// the account does not exist.
    pub fn lamports_of(&self, address: &Pubkey) -> u64 {
        self.svm.get_balance(address).unwrap_or(0)
    }

    /// The token account's balance in units. Panics if the account does not exist or
    /// does not parse as an SPL token account - a missing account must never be read
    /// silently as "zero" (fail-closed).
    pub fn token_balance(&self, token_account: &Pubkey) -> u64 {
        let account = self
            .svm
            .get_account(token_account)
            .unwrap_or_else(|| panic!("token account {token_account} does not exist"));
        TokenAccount::unpack(&account.data)
            .unwrap_or_else(|e| panic!("{token_account} is not an SPL token account: {e}"))
            .amount
    }

    /// The mint's `mint_authority`: `None` after `SetAuthority(MintTokens, None)`. The
    /// observable effect of the funding transaction's final link (Arch §2): no further
    /// minting is possible afterwards. Panics if the mint is absent or does not parse as
    /// an SPL mint (fail-closed).
    pub fn mint_authority(&self, mint: &Pubkey) -> Option<Pubkey> {
        let acc = self
            .svm
            .get_account(mint)
            .unwrap_or_else(|| panic!("mint {mint} does not exist"));
        Mint::unpack(&acc.data)
            .unwrap_or_else(|e| panic!("{mint} is not an SPL mint: {e}"))
            .mint_authority
            .into()
    }

    /// The mint's `supply` - how much $1WIN has been issued in total. Backs the
    /// assertion "exactly as much was minted as went into the vaults".
    pub fn mint_supply(&self, mint: &Pubkey) -> u64 {
        let acc = self
            .svm
            .get_account(mint)
            .unwrap_or_else(|| panic!("mint {mint} does not exist"));
        Mint::unpack(&acc.data)
            .unwrap_or_else(|e| panic!("{mint} is not an SPL mint: {e}"))
            .supply
    }

    /// The `(mint, owner)` of the SPL token account at an address - used to assert an
    /// activity vault's binding (mint == token_mint, owner == vault_authority).
    pub fn token_account_mint_owner(&self, token_account: &Pubkey) -> (Pubkey, Pubkey) {
        let acc = self
            .svm
            .get_account(token_account)
            .unwrap_or_else(|| panic!("token account {token_account} does not exist"));
        let parsed = TokenAccount::unpack(&acc.data)
            .unwrap_or_else(|e| panic!("{token_account} is not an SPL token account: {e}"));
        (parsed.mint, parsed.owner)
    }
}

/// Creates an SPL mint with the given decimals using genuine instructions
/// (`create_account` + `initialize_mint2`); the mint authority is `authority` and
/// there is no freeze authority.
///
/// A free function rather than a method: it is called from the `TestEnv` constructor
/// before the struct is assembled.
pub(crate) fn create_mint(svm: &mut LiteSVM, authority: &Keypair, decimals: u8) -> Pubkey {
    let mint = Keypair::new();
    let rent = svm.minimum_balance_for_rent_exemption(Mint::LEN);
    let create_ix = solana_system_interface::instruction::create_account(
        &authority.pubkey(),
        &mint.pubkey(),
        rent,
        Mint::LEN as u64,
        &spl_token_interface::id(),
    );
    let init_ix = spl_token_interface::instruction::initialize_mint2(
        &spl_token_interface::id(),
        &mint.pubkey(),
        &authority.pubkey(),
        None,
        decimals,
    )
    .expect("the initialize_mint2 instruction");

    let blockhash = svm.latest_blockhash();
    let msg = solana_message::Message::new_with_blockhash(
        &[create_ix, init_ix],
        Some(&authority.pubkey()),
        &blockhash,
    );
    let tx = solana_transaction::versioned::VersionedTransaction::try_new(
        solana_message::VersionedMessage::Legacy(msg),
        &[authority, &mint],
    )
    .expect("the mint creation signatures");
    svm.send_transaction(tx).expect("mint creation");
    mint.pubkey()
}
