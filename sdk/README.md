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
timestamp more than five minutes away from its own clock. The `hd-event-id` header is not signed:
to skip duplicates, use the `id` inside the verified body.

Answer quickly and do the work in the background: the core waits at most 10 seconds and delivers
to add-ons one event at a time. Once the core has a 2xx answer it does not send that event again,
so an add-on that fails later must retry the work itself. Any 2xx answer counts as delivered;
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
| `GET /api/addon/whoami` | none | The add-on's name, scopes and the languages of the agents' UI. |
| `GET /api/addon/customers` | `directory:read` | Customers that can get tickets, with their mail domains and external references. |
| `GET /api/addon/customers/{id}/assets` | `directory:read` | A customer's active assets. |
| `GET /api/addon/customers/{id}/services` | `directory:read` | The active services a customer uses. |
| `GET /api/addon/tickets?requester_email=…` or `?customer=…` | `tickets:read` | Tickets that are not closed, of the requester the address belongs to (any of their addresses) or of a customer. |
| `GET /api/addon/tickets/{number}` | `tickets:read` | A ticket with its messages, links and open proposals; internal notes are never given to add-ons. |
| `POST /api/addon/proposals` | `proposals:write` | Proposes something to the agents. |

A requester has one or more e-mail addresses, each belonging to exactly one requester. A ticket's
`requester` carries them all in `addresses`, the first registered first; `reply_to` is the address
the requester last wrote from on that ticket, where a reply goes, and is empty when they have not
written on it; `last_picked` is then the address an agent picked for the last reply there.
`email`, there and in `requester_email` of the ticket lists, is the address a reply goes to:
`reply_to` when there is one, else `last_picked`, else the first address.

A customer's `external_references` are the names other systems use for it, each a `system` and a
`name` in it, such as a group in a monitoring tool: `[{"system": "Watchtower", "name":
"Brightfield/Offices"}]`. A customer can have several in the same system, and a name names exactly
one customer within its system. The core compares systems and names ignoring case, so an add-on
looking up a name of its own system should too. Agents who manage customers set them; add-ons only
read them. Offboarded customers are not listed, and neither are their references.

The full request and response shapes are in the core's OpenAPI specification
(`hd openapi`, or `/api/openapi.json` on a running core).

### Proposals

A proposal is about an inbox message (`message_id`) or a ticket (`ticket_number`), never both, and
carries a `content` and a `rationale`: one sentence per language of the agents' UI, so that each
agent reads it in the language they chose. `GET /api/addon/whoami` lists those languages; a single
string is accepted too and taken as English, which is also what agents see when their language is
missing.

```json
{ "message_id": "…", "content": { "kind": "triage", "action": "open_ticket", "customer_id": "…" },
  "rationale": { "en": "The sender writes from Brightfield's domain.",
                 "it": "Il mittente scrive dal dominio di Brightfield." } }
```

A draft reply (`kind: reply`) is for the requester, so it is written in the requester's language,
not in the agents'.

| `kind` | On | Content |
|---|---|---|
| `triage` | message | `action`: `open_ticket` with `customer_id`, `attach` with `ticket_number`, or `discard` |
| `priority` | ticket | `priority`: `low`, `normal`, `high` or `urgent` |
| `assets` | ticket | `asset_ids`: active assets of the ticket's customer |
| `services` | ticket | `service_ids`: active services the ticket's customer uses |
| `reply` | ticket | `body`: a draft reply; an accepted draft becomes the agent's saved draft on the ticket, it is never sent by itself |

A new proposal replaces the open one of the same kind on the same message or ticket. The core
refuses (409) a proposal that does not apply any more: a message already out of the inbox, a closed
ticket, an asset of another customer. Nothing takes effect until an agent accepts it.
