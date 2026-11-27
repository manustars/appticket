# Zabbix add-on

The Zabbix add-on brings the hosts a Zabbix monitors into the help desk's catalog: the hosts of
the host groups a customer has as external references become that customer's assets, with the
add-on as their source ([ADR 0002](../../docs/adr/0002-core-owns-the-asset-catalog.md)), and agents
see the live details of those hosts beside their assets. It talks to
Zabbix 7.0 LTS or later through `api_jsonrpc.php`, with an API token sent in
`Authorization: Bearer`.

Zabbix sends its alerts to the add-on's [webhook](#alerts), through the media type shipped here,
and the [rules for alerts](#alert-rules) decide which open a ticket
([ADR 0010](../../docs/adr/0010-monitoring-add-ons-open-tickets-by-explicit-rules.md)); the
[`check` command](#checking-an-alert-against-the-rules) tells which rule an alert meets. What
agents do to the tickets of problems goes [back to Zabbix](#back-to-zabbix). The
[reconciliation](#reconciliation) catches up with what the webhook missed, and the
[safety limit](#safety-limit) stops a flood of tickets.

## What it does

At startup and every few minutes (`sync.interval_minutes`) the add-on:

1. sends Zabbix's host groups to the core as the **name list** of its system (`zabbix.system`,
   `Zabbix` by default), nested as Zabbix nests them by name: `Brightfield/Offices` is nested in
   `Brightfield`. A group whose parent does not exist, such as `Brightfield/Docks/Crane` without
   `Brightfield/Docks`, is nested in the nearest one that does. Two groups whose names differ only
   in case are listed once, as the first in name order. Agents who manage customers then pick a
   customer's external references from that list, on the customer's page;
2. reads the hosts of the groups that customers cover. A group a customer has covers the groups
   nested in it, unless one of them is another customer's, which then covers it and the groups
   nested in it ([domain rules](../../docs/domain-rules.md#add-ons)). Each host covered by one
   customer becomes an asset of that customer:
   - name: the host's visible name;
   - kind: from the first mapping of `assets.kinds` that matches one of the host's tags or of
     the templates linked to the host itself (`selectParentTemplates`; a template that one of
     those templates links does not count), else `assets.default_kind`;
   - serial number: the inventory's `serialno_a`, when the inventory is on and has one;
   - no site, and no holder: agents give it one;
3. retires the assets of hosts that left Zabbix or that no group of their customer covers any
   more, and reports the hosts whose groups are covered by two customers, as conflicts with a
   sentence in English and Italian.

Every host of those groups counts, disabled ones included, and so do hosts that discovery
created: they come and go with discovery, and so do their assets. Renamed, given another serial
number, another tag or another template, a host updates its asset at the next round.

The asset's reference combines the host's id and the customer's (`10084@<customer id>`), since a
reference names one asset of one customer for good: a host that moves to another customer's
groups retires its asset at the old customer and becomes a new asset of the new one.

- A host no external reference covers is not imported, and is no conflict: its groups show among
  the names no customer covers, in the add-on's state.
- A host in a group an offboarded customer has, or in one nested in it, is imported for nobody,
  even inside an active customer's group; one also in an active customer's group is a conflict.
- A host in conflict is neither imported nor updated: an asset it already has stays as it is,
  holder included, and is updated again once the host is in the groups of one customer.
- When the customer already has an asset with the host's serial number or name, entered by hand
  or supplied by another add-on, the core records a link proposal instead of a second asset, and
  the add-on carries on. Once agents accept it, the add-on leaves that asset as it is, and unlinks
  itself when the host leaves Zabbix or its customer, or when the asset's source (an agent, for
  one entered by hand) retires it; the host then becomes an asset of the add-on's own at the next
  round.
- Nothing is retired in a round where Zabbix or the core did not answer every read; the next
  round tries again.

## Live details

When agents look at the asset of a host, one it supplied or one it is linked to, the core asks the
add-on for the asset's live details (`GET /assets/{reference}`, on `addon.listen`), and the add-on
asks Zabbix then. Their labels are in English and Italian; their values are as Zabbix's English
frontend words them, since the contract has values in one language only.

| Field | Value |
|---|---|
| Availability (*Disponibilità*) | `Available`, `Partly available`, `Not available`, `Unknown`, or `Disabled` for a host Zabbix does not monitor; see below. |
| Open problems (*Problemi aperti*) | How many problems of the host are open, leaving out suppressed ones and symptoms, as Zabbix's problem view does by default. |
| Highest severity (*Gravità massima*) | The highest severity of those problems, by Zabbix's default names (`Not classified`, `Information`, `Warning`, `Average`, `High`, `Disaster`); none without problems. |
| Main interface (*Interfaccia principale*) | The IP or DNS name Zabbix connects through, of the default interface of the agent, else SNMP, JMX or IPMI, as Zabbix's host list picks it; none without interfaces. |
| OS (*Sistema operativo*) | The inventory's OS; none when the inventory is off or has none. |

The state is **problem** when the host is not available or has an open High or Disaster problem,
**warning** when it has other open problems or its availability is anything but available, and
**ok** otherwise.

Zabbix 7.0 keeps availability per interface, and for the agent's active checks; its host list
shows a colour per interface type, counting the interfaces that an enabled item uses (all of them
when no item uses any), with the active checks counted with the agent's when the host has an
enabled one. The add-on applies the same rule to all of them together: available when all are
available, not available when all are not available, partly available when some are not available
and the others are available or unknown, and unknown otherwise or when there are none.

The links open the host's page in Zabbix (*Inventory → Hosts*, `hostinventories.php`, with links
to its latest data, graphs and configuration) and its open problems (*Monitoring → Problems* for
the host), on `zabbix.frontend_url`. Agents open them as their own Zabbix user, whose role must
allow those pages and who must be able to read the host; the configuration page also needs write
access, which is why the host link is not it.

The core waits 2 seconds: the add-on asks Zabbix its three questions at once (`host.get`,
`item.get` and `problem.get`) and gives up after 1.5 seconds, answering with an error, so that the
agents see the details as unavailable in time. Each read is answered on its own, beside the rounds
of the catalog: one waiting for Zabbix holds up nothing else. A host Zabbix does not have, or that
the add-on's user cannot read, has no live details.

The contract advises answering from what an add-on already knows; this add-on asks Zabbix on each
read instead, so that agents see a host as it is now rather than as it was at the last round, as
issue #117 asks, with a deadline short enough for the core. Each asset an agent's page shows costs
three calls to Zabbix, and the core reads 8 assets of an add-on at a time within its 2 seconds: on
a page with many assets, or with a slow Zabbix, the last ones may show their details as
unavailable.

## Registering it on the core

```sh
hd addon add zabbix --url http://127.0.0.1:8091 \
  --scope directory:read --scope catalog:write --scope names:write --scope conflicts:write \
  --scope tickets:read --scope tickets:write --event ticket.state_changed
```

`--url` is where the core reaches the add-on's `addon.listen`, for live details and for the
event `ticket.state_changed` (`POST /events`), which tells the add-on that an agent resolved or
closed the ticket of a problem, so that it goes [back to Zabbix](#back-to-zabbix). The core has no
command that adds an event to an add-on registered before: without it, nothing goes back to
Zabbix until the add-on is registered with it. `tickets:write` opens, notes and resolves the
tickets of alerts, and
`tickets:read` finds them by their problem and reads, before resolving one, whether an agent has
worked on it. The core has no command that changes the scopes of an add-on registered before
alerts came in: without the two, each alert fails with the core's sentence on the missing scope,
in the add-on's log and in Zabbix's (*Reports → Action log*), until the add-on is registered with
them. The core prints a token and an event secret once: put the token in
`HD_ZABBIX_TOKEN`, or in a file named by `HD_ZABBIX_TOKEN_FILE`, and the event secret, with which
the core signs its reads and its events, in `HD_ZABBIX_EVENT_SECRET` or a file named by
`HD_ZABBIX_EVENT_SECRET_FILE`.

Upgrading from a version without live details, the add-on stops at startup until
`HD_ZABBIX_EVENT_SECRET` is set (`check` does not need it): use the event secret `hd addon add`
printed with the token. The core has no command that shows it again or issues a new one (`hd addon` has only `add`, `list`,
`enable` and `disable`), and a second `hd addon add zabbix` is refused while the add-on exists.
When it was not kept, the only way out today is to register the add-on under another name, which
gets a new token and event secret but none of the old add-on's assets: the old one keeps them as
their source, and each host the new one sends matches its old asset by name or serial number, so
agents get a link proposal per host to accept. Disable the old add-on afterwards
(`hd addon disable zabbix`).

In Zabbix, give the add-on a user that can read the host groups of the customers, with an API
token (*Users → API tokens*), and put that token in `HD_ZABBIX_API_TOKEN` or in a file named by
`HD_ZABBIX_API_TOKEN_FILE`. The add-on sees only the groups and hosts that user can read. To go
[back to Zabbix](#back-to-zabbix), the user's role (*Users → User roles*) must allow, among its
actions, *Acknowledge problems* and *Add problem comments*, and *Close problems* when a rule
closes them, with access to the API and `event.acknowledge` among the methods it may call; to
close a problem the user also needs read-write permission on the host groups of the problem's
hosts, which Zabbix requires for closing an event.

## Configuration

A TOML file, named by `HD_ZABBIX_CONFIG` (`zabbix.toml` in the working directory by default);
[`zabbix.example.toml`](zabbix.example.toml) has every setting:

| Setting | Default | Meaning |
|---|---|---|
| `core.url` | required | The core's base URL, such as `https://helpdesk.example.com`. |
| `core.ui_url` | `core.url` | The base URL of the help desk's UI as agents' browsers reach it, for the address of a ticket's page that Zabbix's event menu opens, when the add-on reaches the core at another one; 200 characters at most, so that the address fits in a tag of Zabbix (255). |
| `zabbix.url` | required | The base URL of Zabbix's frontend, such as `https://zabbix.example.com`. |
| `zabbix.system` | `Zabbix` | The system's name as customers' external references name it. |
| `zabbix.timeout_seconds` | 30 | How long a call to Zabbix may take. |
| `sync.interval_minutes` | 5 | Minutes between two reads of the hosts. |
| `assets.default_kind` | required | The asset kind of a host no mapping matches, from the help desk's list. |
| `assets.kinds` | none | Mappings, in order: each a `template` (its visible name) or a `tag` (with an optional `value`), and the `kind` it gives; names and values compared ignoring case. |
| `alerts.rules` | the default list | The [rules for alerts](#alert-rules), in order. |
| `alerts.safety_limit_tickets` | 20 | The most tickets the alerts open in `alerts.safety_limit_minutes`, before the [safety limit](#safety-limit) stops them. |
| `alerts.safety_limit_minutes` | 5 | The minutes of the safety limit's window. |
| `zabbix.frontend_url` | `zabbix.url` | The base URL of Zabbix's frontend as agents' browsers reach it, for the links of the live details, when the add-on reaches Zabbix at another one. |
| `addon.listen` | `127.0.0.1:8091` | Where the add-on answers the core's reads of live details, receives its events (`/events`) and receives Zabbix's alerts on its [webhook](#alerts) (`/webhook`). |
| `webhook.secret_file` | none | The file holding the webhook's secret, of 16 characters or more, which Zabbix's media type sends; without a `[webhook]` section, the add-on receives no alerts and says so at startup. |
| `webhook.media_type` | `Help desk` | The name of the media type that sends Zabbix's events to the webhook, whose alerts the [reconciliation](#reconciliation) reads. |

A file that does not fit, such as an unknown setting, a mapping with both a template and a tag or
an invalid rule, stops the add-on with a sentence naming it. A kind the help desk does not have is refused by the
core for each host it would be given to, and logged.

The secrets come from the environment, also read from a `.env` file in the working directory or a
parent:

| Variable | Meaning |
|---|---|
| `HD_ZABBIX_TOKEN` | The token printed by `hd addon add`. |
| `HD_ZABBIX_TOKEN_FILE` | A file holding `HD_ZABBIX_TOKEN`, such as a mounted secret. |
| `HD_ZABBIX_API_TOKEN` | The Zabbix user's API token. |
| `HD_ZABBIX_API_TOKEN_FILE` | A file holding `HD_ZABBIX_API_TOKEN`. |
| `HD_ZABBIX_EVENT_SECRET` | The event secret printed by `hd addon add`. |
| `HD_ZABBIX_EVENT_SECRET_FILE` | A file holding `HD_ZABBIX_EVENT_SECRET`. |
| `HD_ZABBIX_CONFIG` | The configuration file. |
| `HD_ZABBIX_LOG` | A `tracing` filter (default `info`). |

A secret's file is used with the surrounding whitespace (such as a final newline) removed.
Setting a variable and its `_FILE` together, or naming a missing or empty file, stops the add-on
with a message saying which. The webhook's secret is read the same way from the file
`webhook.secret_file` names (a relative path from the working directory); a missing or empty file,
or a secret shorter than 16 characters, stops the add-on. Make one with, for example,
`openssl rand -hex 32 > webhook-secret`.

## Alerts

Zabbix sends the problems of triggers, their recoveries and their updates to the add-on's webhook
(`POST /webhook` on `addon.listen`), through the webhook media type
[`zabbix-media-type.yaml`](zabbix-media-type.yaml). The add-on acts on each within the request and
answers in less than 20 seconds, within the media type's timeout of 30, which the file sets
(Zabbix allows 1 to 60): a request without the webhook's
secret is refused (401), and one it could not act on, because Zabbix or the core did not answer or
the core refused, is answered 502 with the reason, which Zabbix shows in its action log
(*Reports → Action log*), trying three times in all, 10 seconds apart.

**A problem.** The add-on reads the problem's event from Zabbix (severity, name, tags, operational
data, start, hosts) and the host's groups, finds the customer whose asset the host is, as a round
does, and asks the [rules](#alert-rules). When they say `open`, it opens one ticket, with the
problem's event id as its ticket reference, and answers its number and the address of its page in
the help desk's UI (`core.ui_url`), which Zabbix adds to the problem as the tags `hd_ticket` and
`hd_ticket_url`; the media type's entry in the event's menu, *Help desk ticket 42*, opens that
page:

- for the customer of the host's asset, without a requester, so replies go to the customer's key
  requesters, and linked to that asset;
- with the rule's priority, and its service when the customer uses it; otherwise without, and the
  description says so;
- titled with the host's visible name and the event's name, the one the rules' `trigger_contains`
  is matched against: `SRV-01: Mail queue of SRV-01 is too long`;
- described, in its first note, with the problem's id, the rule that opened it, the severity, the
  start time (UTC), the tags and the operational data.

The same event sent again, by Zabbix trying again or by an escalation step, gets the same ticket.
A host that is not imported (no customer covers its groups, two do, or an offboarded customer does)
opens no ticket, nor does a problem the rules ignore: the add-on logs why. A host the catalog does
not have as an asset yet, such as one added to Zabbix since the last round, gets its ticket
without the asset, and the description says so. A trigger on the items of several hosts opens
the ticket of the first of them, in Zabbix's order, that is an asset of a customer and whose alert
a rule opens.

**A recovery.** The add-on reads the problem from Zabbix again and finds the host and the rule
that open its alert, as for the problem. It adds a note with the time the problem recovered, then
reads the ticket and resolves it as that rule says (`on_recovery`): by default only when no agent
has worked on it, always with `resolve`, never with `note`; and only an open or waiting ticket.
When no rule opens the alert any more, such as after the rules changed, the default decides. When
Zabbix does not answer, the add-on adds nothing and answers 502, so that Zabbix tries again,
rather than guess what the rule would say. The recovery of a problem the add-on itself closed,
[back in Zabbix](#back-to-zabbix) once an agent resolved or closed its ticket, adds no note and
resolves nothing: the agent's change is the ticket's, and a ticket they reopened since stays open.
Telling it apart takes the recovery's event, with its time and who closed the problem: when
Zabbix does not give it, the add-on adds nothing and answers 502 too.

`resolve` is the help desk's explicit choice, rule by rule, to override the default of
[ADR 0010](../../docs/adr/0010-monitoring-add-ons-open-tickets-by-explicit-rules.md), by which the
add-on resolves a ticket by itself only when no agent has worked on it: with `resolve`, the
recovery resolves the ticket even when an agent has.

A problem that reaches the add-on after it has already recovered in Zabbix, such as one Zabbix
sends again after a failure while the problem recovered meanwhile, opens its ticket and is handled
at once as its recovery would be: Zabbix's recovery message came when the add-on had no ticket for
it, and nothing else would note or resolve it.

**An update.** An update with a message, such as an acknowledgement with a comment, adds a note
with the Zabbix user, what they did and the message:
`Olga Operator (olga) updated the problem in Zabbix (acknowledged and commented): Looking at it`.
Updates without a message, and those of the add-on's own Zabbix user (the one of
`HD_ZABBIX_API_TOKEN`, which the add-on asks Zabbix for once, at the first update with a message),
add none.

A problem without a ticket, because none was opened for it, gets no note on recovery or update.

### Setting it up in Zabbix

1. Import the media type: *Alerts → Media types → Import*, the file `zabbix-media-type.yaml`.
   Zabbix 7.0 or later. It adds the entry *Help desk ticket* to the menu of each event the add-on
   gave a ticket's page to (`{EVENT.TAGS.hd_ticket_url}`); Zabbix shows it only for those. A media
   type imported from an earlier version of the file has no entry: import the file again with
   *Update existing*, then set its parameters again as below.
2. In the media type **Help desk**, set the parameter `url` to the add-on's webhook as the Zabbix
   server reaches it, such as `https://hd-zabbix.example.com/webhook`; `addon.listen` must then be
   an address the server reaches (`0.0.0.0:8091` behind a proxy with HTTPS, for one). Set
   `http_proxy` if the server goes through one.
3. Give the webhook's secret to the media type: create the global macro `{$HD_WEBHOOK_SECRET}`
   (*Administration → Macros*) of type *Secret text*, with the content of `webhook.secret_file`.
   The parameter `secret` sends it; a secret written in the parameter itself would work too, but
   it would show to those who can read media types.
4. Give a user the media type as a media (*Users → Users → Media*), with any *Send to*, such as
   `helpdesk`. That user must be able to read the hosts whose problems it receives. Make it the
   add-on's own user (the one of `HD_ZABBIX_API_TOKEN`), unless that user is a Super admin: since
   Zabbix 7.0.9 any other user reads only the alerts sent to itself, and the
   [reconciliation](#reconciliation) reads the media type's alerts to tell which recoveries the
   webhook received.
5. Create a trigger action (*Alerts → Actions → Trigger actions*) with the conditions that select
   the problems for the help desk, such as their host groups, and:
   - an operation that sends a message to that user through **Help desk**;
   - a recovery operation *Notify all involved*;
   - an update operation *Notify all involved*.

   The rules still decide which problems open a ticket; the action only decides which reach the
   add-on.

### Reconciliation

At startup and after each round of `sync.interval_minutes`, when it receives alerts (a `[webhook]`
section), the add-on catches up with what the webhook did not bring it, such as while it was down
or after Zabbix stopped trying:

- It reads the problems of triggers open now on the hosts it imports (`problem.get` on the host
  groups customers cover), leaving out suppressed problems (in a maintenance, or suppressed by
  hand) and symptoms of another problem, for which a trigger action pauses its operations by
  default. Each one that has no ticket and that the rules open gets its ticket, as from the
  webhook, with the problem's event id as its ticket reference, so doing it again opens no second
  one; a problem whose ticket was closed gets none. Zabbix gets no `hd_ticket` tag for those
  tickets: it takes tags only from the webhook's answer.
- It reads from Zabbix the problems of the open or waiting tickets it opened whose problem is not
  open now, and, for those that recovered, the media type's alerts of the recovery
  (`alert.get`): a recovery whose alert Zabbix sent reached the webhook, which handled it, and
  one Zabbix is still sending waits for the next reconciliation. Any other, whose alert failed or
  that has none, is handled now: a note with the time, then resolving as the rule says, as
  Zabbix's recovery message would, whatever changed on the ticket since. Within one run the
  add-on remembers the recoveries it handled, and a recovery the webhook and the reconciliation
  meet at once is handled by one of them, the webhook answering 502 to the other so that Zabbix
  tries again. A resolved ticket is left as it is, and so is one whose problem Zabbix no longer
  has, such as after its host was deleted.

  The add-on does not tell a recovery it handled itself, after a restart, from one nobody
  handled: Zabbix has no sent alert for either. After a restart, a ticket still open or waiting
  whose recovery the webhook never received gets the recovery's note again, and is resolved again
  if its rule says `resolve`. That happens only when Zabbix could not deliver the recovery, such
  as while the add-on was down, or when no action sends recoveries to the webhook. When the
  add-on's Zabbix user cannot read the media type (`webhook.media_type`) or its alerts (see step
  4 above), it logs so and handles every recovery it finds in this way.

A reconciliation in which Zabbix or the core does not answer its first reads does nothing; a
problem or ticket it cannot act on is logged and tried again at the next one.

### Safety limit

When the add-on would open more than `alerts.safety_limit_tickets` tickets (20 by default) in
`alerts.safety_limit_minutes` minutes (5), counting those of the webhook and of the reconciliation
together, it opens no more
([ADR 0010](../../docs/adr/0010-monitoring-add-ons-open-tickets-by-explicit-rules.md)):

- It opens one urgent ticket for the help desk's own organization (the customer made with
  `hd customer add --help-desk`), the safety ticket, saying how many problems it held back and
  since when. Its ticket reference is the time it stopped, such as
  `safety-limit:2026-11-27T09:00:00Z`, so that it is opened once; a note tells it how many were
  held back whenever a reconciliation finds more.
- Until an agent resolves or closes the safety ticket, the problems the rules open get no ticket:
  the webhook answers them without one, and the reconciliation counts them. A problem that
  already has a ticket still gets its number, and recoveries and updates still add their notes
  and resolve as the rules say.
- The add-on reads the safety ticket by its reference once per reconciliation and once per
  problem of the webhook it would open, and at startup finds the latest one among the tickets it
  opened: restarting it does not end the wait.
- Once the safety ticket is resolved or closed, the add-on counts afresh, and the reconciliation
  opens the tickets of the problems still open that the rules open, under the same limit, at once
  when a problem of the webhook found the wait over. Reaching the limit again opens another
  safety ticket.

At startup the window counts the add-on's tickets of problems, not closed, that changed within
it, as their last change is never before they opened: a restarting add-on opens no more than the
limit, though a ticket opened earlier that an agent changed meanwhile counts too. When the core
has no help desk's own organization, the safety ticket cannot open: the add-on logs it, holds
back the problems all the same, and tries again at each problem and reconciliation.

## Back to Zabbix

When an agent resolves or closes the ticket of a problem, the core tells the add-on
(`ticket.state_changed`, with the ticket's reference, the problem's event id), and the add-on
acknowledges the problem's event in Zabbix with a message naming the ticket and the agent, and
ending with the change's key, the id of the core's event that told it:

```text
Ticket 42 was resolved in the help desk by Marco Bellini; the problem stays open in Zabbix, since its rule does not close it. [help desk change 6f1c2c1e-8d2a-4c4f-9a43-0d5b8f6e2a10]
```

It also closes the problem when two things hold: the rule the problem's alert meets now says so
(`close_in_zabbix = true`), and the problem's trigger allows closing it by hand (*Allow manual
close*, `manual_close`). The message then says the problem is closed, and otherwise why it stays
open: its rule, no rule opening the alert any more (the default does not close), or its trigger.
When Zabbix refuses to close it all the same, such as for a user whose role does not allow
closing problems or who may only read its hosts, the add-on acknowledges it without closing, and
the message gives Zabbix's reason. A problem that has already recovered is acknowledged with the
ticket and the agent only.

Nothing goes back to Zabbix for the add-on's own resolutions, on recovery, for a ticket moving to
another state (open or waiting), nor for a ticket whose reference is no problem's event id, such
as one the add-on opens about itself. A problem Zabbix no longer has, such as one its housekeeper
removed, is left alone. Zabbix sends the acknowledgement back to the webhook as an update of the
add-on's own user, and the close as a recovery the add-on made: neither adds to the ticket.

The add-on acts on the event within the core's request, taking 8 seconds at most, less than the
10 the core waits. This departs from the contract's advice ([`sdk/README.md`](../../sdk/README.md))
to answer at once and do the work in the background, on purpose: an event the add-on could not
act on, because Zabbix or the core did not answer, is answered with a failure, and the core
delivers it again, from 30 seconds to an hour apart, twelve times in all, so the add-on keeps no
queue of its own and loses nothing when it restarts. The core delivers one event at a time, so a
slow Zabbix holds the next event up to those 8 seconds. A change is acknowledged once: before
acknowledging, the add-on looks for the change's key among its own user's acknowledgements of the
problem, so an event the core delivers again after the add-on acted on it, such as after an
answer that came too late, adds none, whatever the problem became since.

Problems opened before the add-on answered `hd_ticket_url` (before this version) do not have the
tag, so their events show no menu entry; Zabbix takes tags only from the webhook's answer.

## Alert rules

What an alert does is decided by the rules of `[[alerts.rules]]`
([ADR 0010](../../docs/adr/0010-monitoring-add-ons-open-tickets-by-explicit-rules.md)): an ordered
list where the **first rule that matches decides**. A rule matches when all its conditions hold; a
condition left out matches anything, so a rule without conditions matches every alert.

| Condition | Holds when |
|---|---|
| `min_severity` | The alert's severity is this one or higher: `not classified`, `information`, `warning`, `average`, `high` or `disaster`. |
| `customer` | The host is an asset of this customer, by name. A host no customer covers, or two do, has no customer. |
| `host_group` | One of the host's groups is this one or nested in it by name: `Brightfield` holds for `Brightfield/Offices/Floor 2`, not for `Brightfield East`. |
| `tags` | Each of these tags is among the alert's: `{ tag = "service", value = "mail" }`, or `{ tag = "scope" }` for any value. |
| `trigger_contains` | The trigger's name, with its macros resolved as Zabbix shows the problem, contains this text. It is the name of Zabbix's event: for a trigger with an event name of its own (*Event name* in the trigger's form), that one. |

A rule names a customer or a host group, not both. Names, values and texts are compared ignoring
case. The outcome of a rule is `outcome = "ignore"`, or `outcome = "open"` with:

| Setting | Default | Meaning |
|---|---|---|
| `priority` | required | The ticket's priority: `low`, `normal`, `high` or `urgent`. |
| `service` | none | A service, by name, which the ticket links when the customer uses it. |
| `on_recovery` | `resolve_if_not_worked_on` | When the alert's problem recovers in Zabbix: a note, and resolving the ticket only when no agent has worked on it; `resolve`, a note and resolving it in any case; or `note`, a note and never resolving it. |
| `close_in_zabbix` | `false` | Whether the add-on closes the problem in Zabbix when an agent resolves or closes the ticket, if the trigger allows closing it by hand ([back to Zabbix](#back-to-zabbix)). |

A rule may have a `name`, which the sentences about it and `check` show beside its position.

```toml
[[alerts.rules]]
name = "Backups"
trigger_contains = "backup"
outcome = "ignore"

[[alerts.rules]]
name = "Mail at Brightfield"
customer = "Brightfield"
min_severity = "average"
tags = [{ tag = "service", value = "mail" }]
outcome = "open"
priority = "high"
service = "Hosted mail"
```

An alert no rule matches is ignored. Without rules (or with `rules = []`), the **default list**
decides: rule 1, `Disaster`, opens a ticket with priority urgent for a disaster; rule 2, `High`,
one with priority high for a high; the rest is ignored. To ignore every alert, write a single rule
with `outcome = "ignore"` and no conditions.

A rule that does not fit stops the add-on at startup with a sentence naming it by its position and
name, such as `alerts.rules: rule 2 ("Mail at Brightfield"): priority is "soon": give low, normal,
high or urgent`. Among them: an unknown setting or severity, `open` without a priority, `ignore`
with a setting of `open`, a customer with a host group, a blank text and two rules of one name.
Customers, host groups and services are not looked up at startup: a name that does not exist
matches nothing, which `check` shows.

## Checking an alert against the rules

`check` tells which rule an alert meets and with what outcome, or that none does, with the
configuration file and the secrets the add-on runs with. It writes nothing, neither to Zabbix nor
to the core.

```sh
hd-zabbix check --event 4213
hd-zabbix check --host srv-01 --severity high --tag service=mail --tag scope \
  --trigger "Mail queue is too long"
```

- `--event` gives the alert by Zabbix's event id, that of a trigger's problem: its current
  severity, its tags (the trigger's, the items' and the host's) and its name come from Zabbix, and
  so do the hosts of its trigger, each checked in turn when there are several; the event of a
  recovery is refused.
- Otherwise `--host` (its technical or its visible name) and `--severity` are required, and
  `--tag` (`NAME` or `NAME=VALUE`, once for each tag) and `--trigger` are optional.

The host's groups come from Zabbix, and its customer from the customers' external references, read
from the core with the add-on's token as a round does. `check` prints the host with its groups and
whether it is an asset of a customer (a host that is not opens no ticket, whatever the rules say),
the alert as the rules see it, and the rule that decides with its outcome. For a host that is
not imported, it says which rule would match once the host is an asset of a customer:

```text
Host "SRV-01", in the host group "Brightfield/Offices": an asset of Brightfield.
Alert: Zabbix's event 4213, severity high, trigger "Mail queue of SRV-01 is too long", the tags "service=mail" and "site=north".
Rule 2 ("Mail at Brightfield") matches: a ticket opened with priority high, linking the service "Hosted mail" when the customer uses it; on recovery, a note, and the ticket resolved only when no agent has worked on it; once an agent resolves or closes the ticket, the problem left open in Zabbix.
```

## Development

```sh
podman compose --profile zabbix up -d    # Zabbix on http://127.0.0.1:55180, Admin / zabbix
cargo run -p hd-zabbix                   # with a configuration file and the secrets above
cargo test -p hd-zabbix                  # what needs no Zabbix
cargo build -p hd-core && cargo test -p hd-zabbix -- --ignored
```

The ignored tests run the add-on against that Zabbix and the real core: they start the `hd`
binary of the workspace (`cargo build -p hd-core` first, or name one in `HD_TEST_CORE_BIN`) on a
database of their own on the server of `DATABASE_URL`, which they drop afterwards. They sign in
to Zabbix as the default `Admin` of a fresh container, make an API token of their own, create
host groups and hosts under a prefix of their own and remove them at the end. A Zabbix elsewhere
is named by `HD_TEST_ZABBIX_URL` (default `http://127.0.0.1:55180`). CI runs them in the job
`zabbix`. The test of `check` raises a real alert: it pushes a value to a trapper item with
`history.push` until the server, once it has read the new item, opens the problem of its trigger.

The tests of live details register the add-on at a port of their own, where they serve its reads,
and raise problems the same way. One waits for Zabbix to
mark an interface nobody answers at as not available, which takes a minute or two.

The tests of alerts raise problems the same way and serve the add-on's webhook on a port of their
own, bound on `HD_TEST_ZABBIX_WEBHOOK_LISTEN` (default `0.0.0.0:0`, every address, so that Zabbix's
server reaches it from its container). Most post the media type's events to it themselves; one
imports `zabbix-media-type.yaml` under a name of its own, with a user and an action of its own,
gives the global macro `{$HD_WEBHOOK_SECRET}` a secret it generates (removing the macro at the
end, so two runs at once on one Zabbix would clash), and lets Zabbix send them. The server reaches the test at the host named by
`HD_TEST_ZABBIX_WEBHOOK_HOST`, by default `host.docker.internal`, which `compose.yaml` maps to the
host for the server. With Podman on Windows or macOS that name reaches the machine's virtual
machine, not the host where the test runs: give the host's address as the virtual machine sees
it, such as the address after `default via` in `podman machine ssh ip route` with WSL, and
allow the test's binary through the host's firewall if it asks.

The tests of what goes back to Zabbix register the add-on with `ticket.state_changed` at the
webhook's port, where they serve its events too, resolve and close tickets as the core's
administrator and read the acknowledgements and the problems in Zabbix; one imports the media
type, as the tests of alerts do, to read the event menu's entry and to let Zabbix send the
add-on's acknowledgement and close back to it. With `HD_TEST_ZABBIX_WEBHOOK_LISTEN` on a fixed
port, run them one at a time (`-- --ignored --test-threads=1`).

The tests of the reconciliation and the safety limit raise problems the same way and run the
reconciliation themselves, with a limit of 2 tickets in 5 minutes so that nothing waits for the
window. The one of the safety limit adds the help desk's own organization with
`hd customer add --help-desk` and resolves and closes the safety tickets as an agent does. One
imports the media type as the test of alerts does, and stops serving the webhook so that Zabbix
gives up sending a recovery. With `HD_TEST_ZABBIX_WEBHOOK_LISTEN` on a fixed port, run them one
at a time as well.
