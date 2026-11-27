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

An agent opened a ticket, by hand or from an inbox message, or an add-on opened one with
`POST /api/addon/tickets`. (A ticket opened by an incoming message is announced by that message's
`message.received`, with its `ticket_number`.)

| Field | Meaning |
|---|---|
| `ticket_number` | The new ticket. |

### `ticket.state_changed`

A ticket moved from one state to another (`open`, `waiting`, `resolved`, `closed`), recorded in
the same transaction as the change. A move that leaves the state as it was sends nothing, and a
closed ticket never moves again.

```json
{ "ticket_number": 42, "old_state": "waiting", "new_state": "open",
  "changed_by": { "kind": "agent", "agent": { "id": "…", "name": "Marco Bellini" } } }
```

| Field | Meaning |
|---|---|
| `ticket_number` | The ticket. |
| `old_state`, `new_state` | The state it left and the state it is in now; never the same. |
| `changed_by` | Who or what changed it, by `kind`. |

| `kind` | When | Other fields |
|---|---|---|
| `agent` | An agent moved the ticket by hand, with a reply (to the state they picked), by attaching an inbox message to it (which reopens a waiting or resolved ticket), or by resolving the children of a parent ticket (one event per child that changed). | `agent`: the agent's `id` and `name`. |
| `message` | A received message joined the ticket and reopened it. Its sender is not always the ticket's requester: a key requester's answer, for one, joins the ticket too. | `message_id`: the message, which `message.received` announces too. |

More kinds may come, such as an add-on once add-ons resolve the tickets they opened: skip a
`kind` you do not know. Receiving the event needs no scope; reading the ticket afterwards needs
`tickets:read`. A ticket being opened is no change of state: it is announced by `ticket.opened`
or `message.received`.

## Live details: core to add-on

Whenever agents look at an asset, on a customer's assets or on a ticket, the core asks the add-on
the asset comes from (its source) for the asset's live details, with the add-on's own reference
for it:

```
GET <url>/assets/{reference}
```

The reference is percent-encoded as one path segment, so `host 10/42` arrives as
`/assets/host%2010%2F42`. The request carries `hd-timestamp` and `hd-signature`, made as for
events but over `GET /assets/<reference>`, with the reference as the add-on gave it (not
percent-encoded), in place of a body: check them before answering. The add-on answers 200 with:

```json
{ "state": "warning",
  "fields": [{ "label": { "en": "Disk /var", "it": "Disco /var" }, "value": "91% full" },
             { "label": "Uptime", "value": "12 days" }],
  "links": [{ "label": { "en": "Open in Watchtower" },
              "url": "https://watchtower.example.com/hosts/1042" }] }
```

| Field | Meaning |
|---|---|
| `state` | `ok`, `warning` or `problem`: how the asset is doing in the add-on's system. |
| `fields` | Up to 20 values (at most 500 characters each), each with its `label`. Optional. |
| `links` | Up to 10 `http` or `https` links (at most 2000 characters each) into the add-on's system, each with its `label`. Optional. |

A `label` (at most 100 characters) is written in every language of the agents' UI, as a
rationale is (`GET /api/addon/whoami` lists them); a single string is taken as English, which is
also what agents see when their language is missing. Agents see the fields, state and links
beside the asset.

The core waits 2 seconds at most, for the whole answer, and asks every add-on at once, so one
slow add-on holds nothing else up. When the add-on does not answer in time, answers another
status, or answers anything that does not keep to the above as a whole (an unknown state, too
many fields, a link to another scheme, more than 64 KiB), agents see its details as unavailable,
and the core's log says why. A switched-off add-on is not asked: its details are unavailable.
Answer 404 when the add-on has no live details of that asset, as an add-on that serves no live
details at all does: agents then see none, and nothing unavailable. Answer from what the add-on
already knows rather than asking its own system on every read.

Retired assets and the assets of offboarded customers are not asked about. Being asked needs no
scope. Once assets can be linked to several add-ons, each linked add-on is asked the same way,
with its own reference.

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
| `GET /api/addon/tickets/{number}` | `tickets:read` | A ticket with its messages, links and open proposals; internal notes and announcements are never given to add-ons. |
| `POST /api/addon/proposals` | `proposals:write` | Proposes something to the agents. |
| `POST /api/addon/assets` | `catalog:write` | Creates an asset of a customer known by the add-on's reference, or updates the one the add-on supplied with that reference; proposes a link instead when the customer already has a matching asset. |
| `PUT /api/addon/assets/{id}` | `catalog:write` | Updates an asset the add-on is the source of. |
| `POST /api/addon/assets/{id}/retire` | `catalog:write` | Retires an asset the add-on is the source of. |
| `POST /api/addon/assets/{id}/unlink` | `catalog:write` | Unlinks the add-on from an asset it is linked to, which stays as it was. |
| `POST /api/addon/tickets` | `tickets:write` | Opens a ticket without a requester for a customer. |
| `POST /api/addon/tickets/{number}/notes` | `tickets:write` | Adds an internal note to a ticket. |

A requester has one or more e-mail addresses, each belonging to exactly one requester. A ticket's
`requester` carries them all in `addresses`, the first registered first; `reply_to` is the address
the requester last wrote from on that ticket, where a reply goes, and is empty when they have not
written on it; `last_picked` is then the address an agent picked for the last reply there.
`email`, there and in `requester_email` of the ticket lists, is the address a reply goes to:
`reply_to` when there is one, else `last_picked`, else the first address.

An asset, in a customer's asset list, has its `kind` (a name from the help desk's list of asset
kinds) and `name`, its `serial_number` and its own `site` (`{"id", "name"}`, one of its customer's
sites) when it has them, `source_addon`, the add-on it came from, if any, and `linked_addons`, the
names of the other add-ons linked to it (see [Linked add-ons](#linked-add-ons)), with
`reference`, the add-on's own reference for it, when the add-on reading it is its source or linked
to it; another add-on's reference is not given. Add-ons are not told its holder, the requester it
has been given to, nor the site it is at through them.

A customer's `external_references` are the names other systems use for it, each a `system` and a
`name` in it, such as a group in a monitoring tool: `[{"system": "Watchtower", "name":
"Brightfield/Offices"}]`. A customer can have several in the same system, and a name names exactly
one customer within its system. The core compares systems and names ignoring case, so an add-on
looking up a name of its own system should too. Agents who manage customers set them; add-ons only
read them. Offboarded customers are not listed, and neither are their references.

The full request and response shapes are in the core's OpenAPI specification
(`hd openapi`, or `/api/openapi.json` on a running core), which also describes the body of
`ticket.state_changed` and the read of live details (`live_details`) among its `webhooks`.

### Supplying assets

An add-on such as an inventory or monitoring integration supplies the assets of its system to the
help desk's catalog. It is their source: only it changes what it supplied, and it changes nothing
else.

```json
{ "reference": "host-1042", "customer_id": "…", "kind": "Laptop", "name": "PC-0042",
  "serial_number": "SN-0042", "site": "Head office" }
```

- `reference` is the add-on's own identifier for the asset, such as a host's id in its system,
  compared exactly once trimmed. `POST /api/addon/assets` with a reference the add-on has not
  given before creates the asset (201); with one it has, it updates that asset instead (200), so
  the add-on can send its whole inventory again safely. A reference names one asset of one
  customer for good: an asset never changes customer, and the core refuses (409) a reference of
  another customer's asset.
- `kind` is the name of an asset kind of the help desk's list and `site` the name of one of the
  customer's active sites, both compared ignoring case; the add-on maps its own kinds and places
  to them in its configuration. An unknown kind, or a site that is not one of the customer's
  active sites, is refused (409) with a sentence saying which.
- Each call sends the whole asset: a `serial_number` or `site` left out is cleared. The answer is
  the asset as add-ons read it, with its `id` and `reference`.
- `PUT /api/addon/assets/{id}` updates an asset by its id, with the same fields but `reference`
  and `customer_id`; `POST /api/addon/assets/{id}/retire` retires it when it leaves the add-on's
  system (204, also when it is already retired). A retired asset stays on its tickets and loses
  its holder. Supplying it again with its reference (`POST`) brings it back; updating it by its
  id is refused (409), since what the add-on sends then may be older than the retirement.
- An add-on never gives an asset a holder: a `holder_id` or `holder` is refused (409). Agents
  choose the holder, and supplying the asset again leaves it. An asset entered by hand or
  supplied by another add-on, or one of an offboarded customer, is refused (409) by both calls.

Agents see which add-on an asset comes from; they still give it a holder, but do not change its
kind, name, serial number or site, nor retire it.

### Link proposals

When `POST /api/addon/assets` would create an asset (a reference the add-on has not given before)
and the customer already has an active asset with the same serial number or the same name, both
compared ignoring case, entered by hand or supplied by another add-on, the core does not create a
second one. It records a link proposal, that the add-on's asset is that existing one, and answers
202 with it:

```json
{ "id": "…", "addon": "watchtower", "reference": "host-1042", "customer_id": "…",
  "name": "PC-0042", "serial_number": "SN-0042", "matched_by": "serial_number",
  "asset_id": "…", "asset_name": "PC-0042", "created_at": "2026-11-27T09:00:00Z" }
```

- `asset_id` and `asset_name` name the asset that matched, and `matched_by` says by what:
  `serial_number` or `name`. When one asset has the serial number and another the name, the serial
  number decides; among several, the oldest asset is proposed.
- The add-on's own assets never match: two assets with the same name in the add-on's system are
  two assets. Retired assets and assets of other customers do not match either.
- Sending the asset again while the proposal is open records no second proposal: the answer is
  the same proposal, still 202, now with the `name` and `serial_number` just sent, so that agents
  compare what the add-on sends now. It stays open, proposing the same asset, even when that asset
  was renamed or retired meanwhile: an agent decides. The reference stays with its customer
  meanwhile: sending it for another customer is refused (409).
- Agents who manage the catalog see the proposal on the customer's assets and accept or discard
  it. Once discarded, the add-on's next attempt with that reference creates its own asset (201),
  and later ones update it as usual. Once accepted, the add-on is linked to the asset that matched,
  with that reference (see below). Updating an asset by its id (`PUT`) never proposes a link.
- An asset the add-on is linked to matches none of its other references, as its own assets do not.
  Agents cannot accept a proposal on an asset retired meanwhile, nor a second proposal of the same
  add-on on an asset it is already linked to; they discard it instead.

### Linked add-ons

An asset has at most one source and can be linked to any number of other add-ons, each with its
own reference to it, such as a server one integration monitors and another reaches remotely. A
linked add-on changes nothing of the asset: it only supplies live details and links.

- From then on the add-on's reference reaches the linked asset. `POST /api/addon/assets` with it
  answers 200 with the asset, changing nothing, when it sends what the asset has: its kind, name,
  serial number and site, names and serial numbers compared ignoring case as when they matched,
  so that sending again what produced the proposal is safe. Any other change is refused (409), and
  so is one sent with `PUT /api/addon/assets/{id}`. It never brings back a retired asset: only its
  source does. An add-on whose own name for the asset differs (when the serial number matched)
  should leave the assets it is linked to out of its inventory, recognising them in a customer's
  asset list by its `reference` when it is not their `source_addon`.
- `POST /api/addon/assets/{id}/retire` is refused (409) to a linked add-on. When the asset leaves
  its system it unlinks itself with `POST /api/addon/assets/{id}/unlink` (204, also when it has
  already unlinked itself): the asset stays as it was, and the reference no longer reaches it. An
  add-on that never was linked to the asset is refused (409), its source included, and so is an
  asset of an offboarded customer.
- After unlinking, the add-on's next attempt with that reference is treated as new: when the asset
  still matches, the core records a new link proposal for the agents.

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

### Tickets

An add-on with `tickets:write`, such as a monitoring integration, opens tickets by itself
([ADR 0010](../docs/adr/0010-monitoring-add-ons-open-tickets-by-explicit-rules.md)). A ticket is for
an active customer and has no requester, so the agents' replies on it go to the customer's key
requesters:

```json
{ "customer_id": "…", "title": "SRV-01: disk almost full",
  "description": "Disk /var is 97% full since 03:10.", "priority": "high",
  "asset_ids": ["…"], "service_ids": ["…"] }
```

`description` is optional and becomes the ticket's first internal note, by the add-on: agents read
it, the customer never does. `priority` is `low`, `normal` (when omitted), `high` or `urgent`.
`asset_ids` are active assets of the customer, `service_ids` active services it uses, both
optional. The core answers 201 with the ticket's `number`, or refuses the whole ticket (409) with a
sentence naming what does not fit: an offboarded customer, an asset of another customer, a retired
asset or service, a service the customer does not use. A customer, asset or service that does not
exist is not found (404), as when an agent links it.

`POST /api/addon/tickets/{number}/notes` with `{"body": "…"}` adds an internal note to any ticket,
closed ones too. Agents see the add-on as the author of the ticket and of its notes; a ticket read
with `tickets:read` names the add-on that opened it in `opened_by_addon`. Notes are still never
given to add-ons, theirs included.

An add-on never replies, assigns, or changes anything else on a ticket: its token opens none of
the agents' calls.
