# The Assistant: a Rust add-on that only proposes, with a configurable model

The Assistant reads incoming messages and proposes what to do: on an inbox message, open a ticket
for a customer, attach it to a ticket or discard it; on a ticket, a priority, the assets involved
and a draft reply. It never acts by itself: every proposal waits for an agent, and an accepted
draft reply goes into the composer to be read and sent by the agent. There is at most one open
proposal per kind on a message or ticket; a newer one replaces it.

It is an add-on like any other (ADR 0005), written in Rust in `addons/assistant/` and built on the
Rust SDK in `sdk/`. It calls its language model over HTTP, so the model is a setting: an external
service such as the Claude API by default, or a model running on the help desk's own servers.

## Considered options

- **Python, for the official Claude SDK.** There is no official Rust SDK, but the API is plain
  HTTP with JSON and the Assistant needs few calls; one language keeps one toolchain and one CI,
  and gives the Rust SDK a real user from the start.
- **Only an external model, or only a local one.** Sending customers' mail to an external service
  is a data-protection decision for each help desk, so it is theirs to make; the add-on warns about
  it in its configuration.
- **Letting the Assistant act on its own.** Trust has to be earned per kind of proposal; granting
  autonomy later is easy, taking it back is not.
