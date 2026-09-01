//! A parser for emit_cpi events "the way an indexer sees them": it pulls the Vesting
//! program's self-CPI inner instructions out of getTransaction and decodes the
//! payload by the layout
//! `EVENT_IX_TAG_LE (8) || the event's discriminator (8) || Borsh(body)`. The
//! discriminators are cross-checked against the IDL (`make idl`) - the very artifact
//! an external indexer would read the events by.

// DUP: to be merged into svm-core (follow-up) - the module's structure was copied
// from tests/sale-e2e/src/events.rs; it differs in the set of events.

use anchor_lang::{AnchorDeserialize, Discriminator};
use solana_transaction_status_client_types::{
    EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction, UiInstruction, UiMessage,
};
use vesting::events as ev;

/// The Anchor event-CPI discriminator (`anchor.coder.events`): e445a52e51cb9a1d.
pub const EVENT_IX_TAG_LE: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

/// All thirteen events of the program (Arch §11) - against fourteen instructions:
/// `assert_funded` emits no event.
pub enum VestingEvent {
    Initialized(ev::Initialized),
    TgeUpdated(ev::TgeUpdated),
    AdminUpdated(ev::AdminUpdated),
    OperatorUpdated(ev::OperatorUpdated),
    OwnerProposed(ev::OwnerProposed),
    OwnerProposalCancelled(ev::OwnerProposalCancelled),
    OwnerUpdated(ev::OwnerUpdated),
    WithdrawalAuthorityUpdated(ev::WithdrawalAuthorityUpdated),
    VestingCreated(ev::VestingCreated),
    RecipientUpdated(ev::RecipientUpdated),
    VestingCancelled(ev::VestingCancelled),
    Claimed(ev::Claimed),
    VestingClosed(ev::VestingClosed),
    /// A self-CPI instruction carrying the event tag but an unknown discriminator: a
    /// program event absent from the list above. An `Other` showing up in a scenario is a
    /// signal that the set of §11 has drifted apart from the code.
    Other([u8; 8]),
}

impl VestingEvent {
    pub fn name(&self) -> &'static str {
        match self {
            VestingEvent::Initialized(_) => "Initialized",
            VestingEvent::TgeUpdated(_) => "TgeUpdated",
            VestingEvent::AdminUpdated(_) => "AdminUpdated",
            VestingEvent::OperatorUpdated(_) => "OperatorUpdated",
            VestingEvent::OwnerProposed(_) => "OwnerProposed",
            VestingEvent::OwnerProposalCancelled(_) => "OwnerProposalCancelled",
            VestingEvent::OwnerUpdated(_) => "OwnerUpdated",
            VestingEvent::WithdrawalAuthorityUpdated(_) => "WithdrawalAuthorityUpdated",
            VestingEvent::VestingCreated(_) => "VestingCreated",
            VestingEvent::RecipientUpdated(_) => "RecipientUpdated",
            VestingEvent::VestingCancelled(_) => "VestingCancelled",
            VestingEvent::Claimed(_) => "Claimed",
            VestingEvent::VestingClosed(_) => "VestingClosed",
            VestingEvent::Other(_) => "Other",
        }
    }
}

fn decode_one(disc: &[u8], body: &[u8]) -> VestingEvent {
    fn de<T: AnchorDeserialize>(mut body: &[u8]) -> T {
        T::deserialize(&mut body).expect("the event's Borsh payload")
    }
    match disc {
        d if d == ev::Initialized::DISCRIMINATOR => VestingEvent::Initialized(de(body)),
        d if d == ev::TgeUpdated::DISCRIMINATOR => VestingEvent::TgeUpdated(de(body)),
        d if d == ev::AdminUpdated::DISCRIMINATOR => VestingEvent::AdminUpdated(de(body)),
        d if d == ev::OperatorUpdated::DISCRIMINATOR => VestingEvent::OperatorUpdated(de(body)),
        d if d == ev::OwnerProposed::DISCRIMINATOR => VestingEvent::OwnerProposed(de(body)),
        d if d == ev::OwnerProposalCancelled::DISCRIMINATOR => {
            VestingEvent::OwnerProposalCancelled(de(body))
        }
        d if d == ev::OwnerUpdated::DISCRIMINATOR => VestingEvent::OwnerUpdated(de(body)),
        d if d == ev::WithdrawalAuthorityUpdated::DISCRIMINATOR => {
            VestingEvent::WithdrawalAuthorityUpdated(de(body))
        }
        d if d == ev::VestingCreated::DISCRIMINATOR => VestingEvent::VestingCreated(de(body)),
        d if d == ev::RecipientUpdated::DISCRIMINATOR => VestingEvent::RecipientUpdated(de(body)),
        d if d == ev::VestingCancelled::DISCRIMINATOR => VestingEvent::VestingCancelled(de(body)),
        d if d == ev::Claimed::DISCRIMINATOR => VestingEvent::Claimed(de(body)),
        d if d == ev::VestingClosed::DISCRIMINATOR => VestingEvent::VestingClosed(de(body)),
        d => VestingEvent::Other(d.try_into().expect("8 discriminator bytes")),
    }
}

/// Extracts ALL Vesting emit_cpi events from a confirmed transaction the same way an
/// indexer would: a json-encoded getTransaction, the inner instructions, resolving
/// the program id through account_keys, and a base58 decode of the data.
pub fn parse_vesting_events(tx: &EncodedConfirmedTransactionWithStatusMeta) -> Vec<VestingEvent> {
    let EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction else {
        panic!("a json-encoded getTransaction was expected");
    };
    let UiMessage::Raw(raw) = &ui_tx.message else {
        panic!("a raw message was expected (encoding Json)");
    };
    let vesting_id = vesting::id().to_string();
    let meta = tx
        .transaction
        .meta
        .as_ref()
        .expect("the transaction's meta");
    let inner: Option<Vec<_>> = meta.inner_instructions.clone().into();

    let mut out = Vec::new();
    for group in inner.unwrap_or_default() {
        for ins in &group.instructions {
            let UiInstruction::Compiled(c) = ins else {
                continue;
            };
            let program_id = raw
                .account_keys
                .get(c.program_id_index as usize)
                .expect("program_id_index within the bounds of account_keys");
            if *program_id != vesting_id {
                continue;
            }
            let data = bs58::decode(&c.data).into_vec().expect("base58 data");
            if data.len() < 16 || data[..8] != EVENT_IX_TAG_LE {
                continue;
            }
            out.push(decode_one(&data[8..16], &data[16..]));
        }
    }
    out
}

/// Cross-checks the discriminators of ALL thirteen events of §11 against the IDL
/// (target/idl/vesting.json): an indexer works off the IDL, and the test proves the
/// bytes on the transaction are the same ones. Completeness is asserted too: the IDL
/// holds exactly thirteen events, meaning none was added past this list.
///
/// Reading the IDL is gated by the freshness check: `target/idl/vesting.json` is a
/// derived artifact (`make idl`), and against a stale file the cross-check would
/// agree on the previous event surface, proving nothing.
pub fn assert_idl_event_discriminators() {
    let idl_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/idl/vesting.json");
    vesting_test_common::assert_fresh_vs_sources(
        std::path::Path::new(idl_path),
        "the program IDL",
        "run `make idl`: the target rebuilds vesting.so and generates the IDL from the program's types; against a stale IDL the event discriminators would be checked against the previous surface",
    );
    let idl: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(idl_path)
            .unwrap_or_else(|e| panic!("no IDL at {idl_path}: {e}. Run `make idl` first")),
    )
    .expect("the IDL parses as JSON");

    let events = idl["events"].as_array().expect("IDL.events");
    let idl_disc = |name: &str| -> Vec<u8> {
        events
            .iter()
            .find(|e| e["name"] == name)
            .unwrap_or_else(|| panic!("event {name} is absent from the IDL"))["discriminator"]
            .as_array()
            .expect("IDL discriminator")
            .iter()
            .map(|v| v.as_u64().expect("bytes") as u8)
            .collect()
    };

    let pairs: [(&str, &[u8]); 13] = [
        ("Initialized", ev::Initialized::DISCRIMINATOR),
        ("TgeUpdated", ev::TgeUpdated::DISCRIMINATOR),
        ("AdminUpdated", ev::AdminUpdated::DISCRIMINATOR),
        ("OperatorUpdated", ev::OperatorUpdated::DISCRIMINATOR),
        ("OwnerProposed", ev::OwnerProposed::DISCRIMINATOR),
        (
            "OwnerProposalCancelled",
            ev::OwnerProposalCancelled::DISCRIMINATOR,
        ),
        ("OwnerUpdated", ev::OwnerUpdated::DISCRIMINATOR),
        (
            "WithdrawalAuthorityUpdated",
            ev::WithdrawalAuthorityUpdated::DISCRIMINATOR,
        ),
        ("VestingCreated", ev::VestingCreated::DISCRIMINATOR),
        ("RecipientUpdated", ev::RecipientUpdated::DISCRIMINATOR),
        ("VestingCancelled", ev::VestingCancelled::DISCRIMINATOR),
        ("Claimed", ev::Claimed::DISCRIMINATOR),
        ("VestingClosed", ev::VestingClosed::DISCRIMINATOR),
    ];
    for (name, rust_disc) in pairs {
        assert_eq!(
            idl_disc(name),
            rust_disc,
            "discriminator of {name}: IDL != events.rs"
        );
    }
    assert_eq!(
        events.len(),
        pairs.len(),
        "the IDL holds {} events but {} are checked - the set of §11 has drifted apart from the code",
        events.len(),
        pairs.len()
    );
}
