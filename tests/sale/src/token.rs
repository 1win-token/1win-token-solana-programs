//! Token helpers: classic SPL Token plus ATA (NOT Token-2022 - forbidden by the
//! spec).
//!
//! Decision (iteration 0): mints and ATAs are created by GENUINE spl-token / ATA
//! instructions against the programs built into LiteSVM (`spl_token-3.5.0`,
//! `spl_associated_token_account-1.1.1`) - the same path production clients take.
//! `set_account` with Pack serialization is kept for future tests that need state
//! "unreachable by honest means".

use crate::env::TestEnv;
use litesvm::LiteSVM;
use solana_keypair::Keypair;
use solana_program_pack::Pack;
use solana_pubkey::Pubkey;
use solana_signer::Signer;
use spl_token_interface::state::{Account as TokenAccount, Mint};

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

    /// Mints `amount` units into the `destination` token account (the mint authority
    /// is the environment's `payer`).
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

    /// Creates an extra SPL mint with an arbitrary `decimals` (the mint authority is
    /// the environment's `payer`). Used by the `initialize` negatives: a USDC mint with
    /// decimals != 6, a token mint with decimals != 9.
    pub fn create_mint_for_test(&mut self, decimals: u8) -> Pubkey {
        let payer = self.payer.insecure_clone();
        create_mint(&mut self.svm, &payer, decimals)
    }

    /// Forges the balance of an SPL token account (for the `DepositVaultUnderfunded`
    /// negative: the vault cannot be understated by honest means - the purchase/refund
    /// bookkeeping is symmetric).
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

    /// Forges an account at address `address`, OWNED by the Sale program, holding raw
    /// `data` (usually `discriminator || Borsh(body)`). The rent-exempt lamport balance
    /// is derived from the data length.
    ///
    /// Needed for the check-3 negative in `close_batch` (a PDA/body mismatch): create a
    /// program-owned account carrying a valid `Deposit` discriminator but a body that
    /// does NOT derive to its own address. Unreachable by honest means - PDAs are only
    /// ever initialized at their canonical address.
    pub fn forge_program_account(&mut self, address: &Pubkey, data: Vec<u8>) {
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        let account = solana_account::Account {
            lamports,
            data,
            owner: sale::id(),
            executable: false,
            rent_epoch: 0,
        };
        self.svm
            .set_account(*address, account)
            .expect("set_account(forged program account)");
    }

    /// The account's lamports (for the "rent landed on body.wallet" assertions). 0 if
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
    /// observable effect of the funding transaction's final link: no further minting is
    /// possible afterwards. Panics if the mint is absent or does not parse as an SPL
    /// mint (fail-closed).
    pub fn mint_authority(&self, mint: &Pubkey) -> Option<Pubkey> {
        let account = self
            .svm
            .get_account(mint)
            .unwrap_or_else(|| panic!("mint {mint} does not exist"));
        Mint::unpack(&account.data)
            .unwrap_or_else(|e| panic!("{mint} is not an SPL mint: {e}"))
            .mint_authority
            .into()
    }
}

/// Creates an SPL mint with the given decimals using genuine instructions
/// (`create_account` + `initialize_mint2`); the mint authority is `authority` and
/// there is no freeze authority.
///
/// A free function rather than a method: it is called from the `TestEnv`
/// constructor before the struct is assembled.
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
