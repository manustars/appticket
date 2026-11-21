# Core and add-ons talk over HTTP, with events stored by the core

Core and add-ons exchange JSON over HTTP in three ways, with no message broker:

- **Events, core to add-on** ("a message arrived", "a ticket was resolved"): the core records each
  event in its own database and moves on; a delivery component sends it to the add-on and retries
  while the add-on is off, so nothing is lost and nobody waits.
- **Actions, add-on to core**: the add-on calls the core's API, for example to create a proposal or
  an asset, or to open a ticket from a monitoring alert.
- **Reads, core to add-on** (an asset's details): a call with a short timeout; when the add-on does
  not answer, the core shows the details as unavailable.

## Contract and trust

The contract is the add-on API specification (OpenAPI) and the event schema in `sdk/`; a Rust SDK is
its first user, and SDKs for other languages are generated from the same specification when needed.
Registering an add-on creates two secrets: the core signs every event with a secret shared with that
add-on, and the add-on calls the core with a token of its own, limited to what that add-on may do
and revoked when the add-on is switched off.

## Considered options

- **Only calls that wait for an answer.** The core would stall for as long as the slowest add-on;
  the Assistant can take tens of seconds per message.
- **A message broker (RabbitMQ, NATS).** Same behaviour as stored events, but one more service to
  install, upgrade and watch, for volumes a help desk does not reach.
- **gRPC.** Only a different encoding for the same patterns, and harder for add-ons written in any
  language.
