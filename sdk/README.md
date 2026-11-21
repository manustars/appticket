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

### `ticket.opened`

An agent opened a ticket, by hand or from an inbox message. (A ticket opened by an incoming message
is announced by that message's `message.received`, with its `ticket_number`.)

| Field | Meaning |
|---|---|
| `ticket_number` | The new ticket. |

## Calls: add-on to core

The add-on calls the core's API with `Authorization: Bearer <token>`. Each operation needs a scope
given at registration (`hd addon add … --scope <scope>`); without it the core answers 409 with a
sentence saying which scope is missing.

| Call | Scope | What it does |
|---|---|---|
| `GET /api/addon/whoami` | none | The add-on's name and scopes. |
| `GET /api/addon/customers` | `directory:read` | Customers that can get tickets, with their mail domains. |
| `GET /api/addon/customers/{id}/assets` | `directory:read` | A customer's active assets. |
| `GET /api/addon/tickets?requester_email=…` or `?customer=…` | `tickets:read` | Tickets that are not closed. |
| `GET /api/addon/tickets/{number}` | `tickets:read` | A ticket with its messages, notes, links and open proposals. |
| `POST /api/addon/proposals` | `proposals:write` | Proposes something to the agents. |

The full request and response shapes are in the core's OpenAPI specification
(`hd openapi`, or `/api/openapi.json` on a running core).

### Proposals

A proposal is about an inbox message (`message_id`) or a ticket (`ticket_number`), never both, and
carries a `content` and a one-sentence `rationale` shown to the agent:

```json
{ "message_id": "…", "content": { "kind": "triage", "action": "open_ticket", "customer_id": "…" },
  "rationale": "The sender writes from Brightfield's domain." }
```

| `kind` | On | Content |
|---|---|---|
| `triage` | message | `action`: `open_ticket` with `customer_id`, `attach` with `ticket_number`, or `discard` |
| `priority` | ticket | `priority`: `low`, `normal`, `high` or `urgent` |
| `assets` | ticket | `asset_ids`: active assets of the ticket's customer |
| `reply` | ticket | `body`: a draft reply; an accepted draft goes into the agent's composer, it is never sent by itself |

A new proposal replaces the open one of the same kind on the same message or ticket. The core
refuses (409) a proposal that does not apply any more: a message already out of the inbox, a closed
ticket, an asset of another customer. Nothing takes effect until an agent accepts it.
