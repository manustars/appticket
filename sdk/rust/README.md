# hd-addon-sdk

The Rust SDK for add-ons of the help desk core, following the contract in [`../README.md`](../README.md).
Apache-2.0, like the rest of `sdk/`.

- `events`: the envelope the core posts to `<url>/events`, with typed data for `message.received`,
  `ticket.opened` and `ticket.state_changed`; unknown kinds come through as `Event::Other`.
  `TicketStateChanged` carries the old and new `TicketState` and `changed_by`, a
  `StateChangedBy`: an `Agent` with their `id` and `name`, a `Message` that reopened the ticket,
  or `Other` for a kind of changer this version does not know.
- `signature`: `Verifier` checks `hd-signature` (HMAC-SHA-256 over `<hd-timestamp>.<raw body>`) in
  constant time and refuses timestamps more than five minutes off; its clock can be replaced in
  tests. `sign` computes the header, for testing an add-on.
- `Client`: the add-on API (`whoami`, `customers`, `customer_assets`, `customer_services`,
  `open_tickets_of_requester`, `open_tickets_of_customer`, `ticket`, `propose`, `open_ticket` and
  `add_note` for the tickets the add-on opens, `supply_asset`, `update_asset`, `retire_asset`
  for the assets the add-on is the source of, and `unlink_asset` for those it is linked to). Errors
  carry the core's sentence. Answers keep only the fields an add-on needs and ignore the rest.
  `whoami` lists the `languages` of the agents' UI;
  each `Customer` carries its `external_references`. `api::SuppliedAsset` is an asset as the add-on
  supplies it, known by its own `reference`; `supply_asset` answers `Supplied::Asset`, telling
  whether it `created` the asset or updated the one with that reference, or
  `Supplied::LinkProposed` with the `LinkProposal` the core recorded instead, when the customer
  already has an asset with the same serial number or name. Once agents accept it, the add-on is
  linked to that asset: its reference reaches it, unchanged, and an `Asset` lists its
  `linked_addons` by name.
- `proposals`: `Content` and `Triage`, the same JSON as the core, and `Rationale`, one sentence
  per language (`{"en": …, "it": …}`) that `propose` sends; `in_english` makes one with only an
  English sentence, which agents in other languages then read.
- `tickets`: `NewTicket`, the ticket without a requester that `open_ticket` sends.
- `server` (feature `axum`): `events_router` turns a handler into a `POST /events` endpoint that
  only passes verified envelopes on.

```rust
use hd_addon_sdk::{Client, Content, Target, Verifier, server::events_router};

let core = Client::new("https://helpdesk.example.com", &token)?;
let app = events_router(Verifier::new(event_secret), move |envelope| async move {
    // Answer within 10 seconds: start slow work in the background.
    Ok(())
});
```

The [Assistant](../../addons/assistant) is built on it.
