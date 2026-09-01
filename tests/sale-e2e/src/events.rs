//! A parser for emit_cpi events "the way an indexer sees them": it pulls the Sale
//! program's self-CPI inner instructions out of getTransaction and decodes the
//! payload by the layout
//! `EVENT_IX_TAG_LE (8) || the event's discriminator (8) || Borsh(body)`. The
//! discriminators are cross-checked against the IDL (`make idl`) - the very artifact
//! an external indexer would read the events by.

use anchor_lang::{AnchorDeserialize, Discriminator};
use sale::events as ev;
use solana_transaction_status_client_types::{
    EncodedConfirmedTransactionWithStatusMeta, EncodedTransaction, UiInstruction, UiMessage,
};

/// The Anchor event-CPI discriminator (`anchor.coder.events`): e445a52e51cb9a1d.
pub const EVENT_IX_TAG_LE: [u8; 8] = [0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d];

/// The typed Sale lifecycle events (Arch §12).
pub enum SaleEvent {
    Initialized(ev::Initialized),
    AggregateInitialized(ev::AggregateInitialized),
    Purchased(ev::Purchased),
    Refunded(ev::Refunded),
    ClaimActivated(ev::ClaimActivated),
    Claimed(ev::Claimed),
    ProceedsWithdrawn(ev::ProceedsWithdrawn),
    Swept(ev::Swept),
    AccountsClosed(ev::AccountsClosed),
    /// A program event outside the lifecycle (parameters / roles / lists).
    Other([u8; 8]),
}

impl SaleEvent {
    pub fn name(&self) -> &'static str {
        match self {
            SaleEvent::Initialized(_) => "Initialized",
            SaleEvent::AggregateInitialized(_) => "AggregateInitialized",
            SaleEvent::Purchased(_) => "Purchased",
            SaleEvent::Refunded(_) => "Refunded",
            SaleEvent::ClaimActivated(_) => "ClaimActivated",
            SaleEvent::Claimed(_) => "Claimed",
            SaleEvent::ProceedsWithdrawn(_) => "ProceedsWithdrawn",
            SaleEvent::Swept(_) => "Swept",
            SaleEvent::AccountsClosed(_) => "AccountsClosed",
            SaleEvent::Other(_) => "Other",
        }
    }
}

fn decode_one(disc: &[u8], body: &[u8]) -> SaleEvent {
    fn de<T: AnchorDeserialize>(mut body: &[u8]) -> T {
        T::deserialize(&mut body).expect("the event's Borsh payload")
    }
    match disc {
        d if d == ev::Initialized::DISCRIMINATOR => SaleEvent::Initialized(de(body)),
        d if d == ev::AggregateInitialized::DISCRIMINATOR => {
            SaleEvent::AggregateInitialized(de(body))
        }
        d if d == ev::Purchased::DISCRIMINATOR => SaleEvent::Purchased(de(body)),
        d if d == ev::Refunded::DISCRIMINATOR => SaleEvent::Refunded(de(body)),
        d if d == ev::ClaimActivated::DISCRIMINATOR => SaleEvent::ClaimActivated(de(body)),
        d if d == ev::Claimed::DISCRIMINATOR => SaleEvent::Claimed(de(body)),
        d if d == ev::ProceedsWithdrawn::DISCRIMINATOR => SaleEvent::ProceedsWithdrawn(de(body)),
        d if d == ev::Swept::DISCRIMINATOR => SaleEvent::Swept(de(body)),
        d if d == ev::AccountsClosed::DISCRIMINATOR => SaleEvent::AccountsClosed(de(body)),
        d => SaleEvent::Other(d.try_into().expect("8 discriminator bytes")),
    }
}

/// Extracts ALL Sale emit_cpi events from a confirmed transaction the same way an
/// indexer would: a json-encoded getTransaction, the inner instructions, resolving
/// the program id through account_keys, and a base58 decode of the data.
pub fn parse_sale_events(tx: &EncodedConfirmedTransactionWithStatusMeta) -> Vec<SaleEvent> {
    let EncodedTransaction::Json(ui_tx) = &tx.transaction.transaction else {
        panic!("a json-encoded getTransaction was expected");
    };
    let UiMessage::Raw(raw) = &ui_tx.message else {
        panic!("a raw message was expected (encoding Json)");
    };
    let sale_id = sale::id().to_string();
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
            if *program_id != sale_id {
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

/// Cross-checks the discriminators of every lifecycle event against the IDL
/// (target/idl/sale.json): an indexer works off the IDL, and the test proves the
/// bytes on the transaction are the same ones.
///
/// Reading the IDL is gated by the freshness check: `target/idl/sale.json` is a
/// derived artifact (`make idl`), and against a stale file the cross-check would
/// agree on the previous event surface, proving nothing.
pub fn assert_idl_event_discriminators() {
    let idl_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/idl/sale.json");
    sale_test_common::assert_fresh_vs_sources(
        std::path::Path::new(idl_path),
        sale_test_common::SALE_CRATE_DIR,
        "the program IDL",
        "run `make idl`: the target rebuilds sale.so and generates the IDL from the program's types; against a stale IDL the event discriminators would be checked against the previous surface",
    );
    let idl: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(idl_path)
            .unwrap_or_else(|e| panic!("no IDL at {idl_path}: {e}. Run `make idl` first")),
    )
    .expect("the IDL parses as JSON");

    let idl_disc = |name: &str| -> Vec<u8> {
        idl["events"]
            .as_array()
            .expect("IDL.events")
            .iter()
            .find(|e| e["name"] == name)
            .unwrap_or_else(|| panic!("event {name} is absent from the IDL"))["discriminator"]
            .as_array()
            .expect("IDL discriminator")
            .iter()
            .map(|v| v.as_u64().expect("bytes") as u8)
            .collect()
    };

    let pairs: [(&str, &[u8]); 9] = [
        ("Initialized", ev::Initialized::DISCRIMINATOR),
        (
            "AggregateInitialized",
            ev::AggregateInitialized::DISCRIMINATOR,
        ),
        ("Purchased", ev::Purchased::DISCRIMINATOR),
        ("Refunded", ev::Refunded::DISCRIMINATOR),
        ("ClaimActivated", ev::ClaimActivated::DISCRIMINATOR),
        ("Claimed", ev::Claimed::DISCRIMINATOR),
        ("ProceedsWithdrawn", ev::ProceedsWithdrawn::DISCRIMINATOR),
        ("Swept", ev::Swept::DISCRIMINATOR),
        ("AccountsClosed", ev::AccountsClosed::DISCRIMINATOR),
    ];
    for (name, rust_disc) in pairs {
        assert_eq!(
            idl_disc(name),
            rust_disc,
            "discriminator of {name}: IDL != events.rs"
        );
    }
}
