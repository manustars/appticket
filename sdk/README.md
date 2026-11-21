# Add-on contract

An add-on is a separate program that the help desk core switches on and off. This folder holds the
contract between the two; it is Apache-2.0 so that add-ons can carry any license. Design and
reasons: [ADR 0005](../docs/adr/0005-add-on-communication.md).

## Registering

An administrator registers the add-on on the core:

```sh
hd addon add assistant --url https://assistant.example.com --event message.received
```

The core prints two secrets once: the add-on's **token**, for its calls to the core, and its
**event secret**, to check the events it receives. `hd addon disable <name>` switches it off: its
token is refused and its events wait; `hd addon enable <name>` delivers them.

## Events: core to add-on

The core sends each event as `POST <url>/events` with a JSON body:

```json
{
  "id": "6f1c…",
  "kind": "message.received",
  "created_at": "2026-11-21T09:00:00Z",
  "data": { … }
}
```

and these headers:

| Header | Value |
|---|---|
| `hd-event-id` | The event's id; the same on every retry, so the add-on can skip duplicates. |
| `hd-timestamp` | Seconds since the Unix epoch when this attempt was sent. |
| `hd-signature` | `sha256=` and the hex HMAC-SHA-256 of `<hd-timestamp>.<raw body>`, keyed with the event secret. |

The add-on must check the signature over the raw body before parsing it, and should refuse a
timestamp more than five minutes away from its own clock. Any 2xx answer counts as delivered;
anything else, or no answer within 10 seconds, is retried after 30 seconds, then with a doubling
delay up to an hour, twelve times in all. Events may arrive more than once and, across retries, out
of order.

### `message.received`

A message arrived and the core routed it to a ticket or to the inbox.

| Field | Meaning |
|---|---|
| `message_id` | The message's id in the core. |
| `ticket_number` | The ticket it joined or opened, or `null` when it waits in the inbox. |
| `from_address`, `from_name` | The sender. |
| `subject`, `body_text` | The subject and the plain-text body. |

## Calls: add-on to core

The add-on calls the core's API with `Authorization: Bearer <token>`. Today only
`GET /api/addon/whoami` exists; it answers the add-on's name and scopes. What an add-on may do
(its scopes) grows with the operations add-ons need, such as proposals.
