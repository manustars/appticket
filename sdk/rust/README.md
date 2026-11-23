# hd-addon-sdk

The Rust SDK for add-ons of the help desk core, following the contract in [`../README.md`](../README.md).
Apache-2.0, like the rest of `sdk/`.

- `events`: the envelope the core posts to `<url>/events`, with typed data for `message.received`
  and `ticket.opened`; unknown kinds come through as `Event::Other`.
- `signature`: `Verifier` checks `hd-signature` (HMAC-SHA-256 over `<hd-timestamp>.<raw body>`) in
  constant time and refuses timestamps more than five minutes off; its clock can be replaced in
  tests. `sign` computes the header, for testing an add-on.
- `Client`: the add-on API (`whoami`, `customers`, `customer_assets`, `customer_services`,
  `open_tickets_of_requester`, `open_tickets_of_customer`, `ticket`, `propose`). Errors carry the
  core's sentence. Answers keep only the fields an add-on needs and ignore the rest. `whoami`
  lists the `languages` of the agents' UI; each `Customer` carries its `external_references`.
- `proposals`: `Content` and `Triage`, the same JSON as the core, and `Rationale`, one sentence
  per language (`{"en": …, "it": …}`) that `propose` sends; `in_english` makes one with only an
  English sentence, which agents in other languages then read.
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
