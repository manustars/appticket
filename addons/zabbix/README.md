# Zabbix add-on

The Zabbix add-on brings the hosts a Zabbix monitors into the help desk's catalog: the hosts of
the host groups a customer has as external references become that customer's assets, with the
add-on as their source ([ADR 0002](../../docs/adr/0002-core-owns-the-asset-catalog.md)). It talks to
Zabbix 7.0 LTS or later through `api_jsonrpc.php`, with an API token sent in
`Authorization: Bearer`.

The [rules for alerts](#alert-rules) are read and checked at startup, and the
[`check` command](#checking-a-problem-against-the-rules) tells which rule a problem meets; the
alerts themselves, the tickets they open and what goes back to Zabbix
([ADR 0010](../../docs/adr/0010-monitoring-add-ons-open-tickets-by-explicit-rules.md)) come in later
versions.

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

## Registering it on the core

```sh
hd addon add zabbix --url http://127.0.0.1:8091 \
  --scope directory:read --scope catalog:write --scope names:write --scope conflicts:write
```

The add-on receives no events and answers no reads yet, so `--url` is not called. The core prints
a token once: put it in `HD_ZABBIX_TOKEN`, or in a file named by `HD_ZABBIX_TOKEN_FILE`.

In Zabbix, give the add-on a user that can read the host groups of the customers, with an API
token (*Users → API tokens*), and put that token in `HD_ZABBIX_API_TOKEN` or in a file named by
`HD_ZABBIX_API_TOKEN_FILE`. The add-on sees only the groups and hosts that user can read.

## Configuration

A TOML file, named by `HD_ZABBIX_CONFIG` (`zabbix.toml` in the working directory by default);
[`zabbix.example.toml`](zabbix.example.toml) has every setting:

| Setting | Default | Meaning |
|---|---|---|
| `core.url` | required | The core's base URL, such as `https://helpdesk.example.com`. |
| `zabbix.url` | required | The base URL of Zabbix's frontend, such as `https://zabbix.example.com`. |
| `zabbix.system` | `Zabbix` | The system's name as customers' external references name it. |
| `zabbix.timeout_seconds` | 30 | How long a call to Zabbix may take. |
| `sync.interval_minutes` | 5 | Minutes between two reads of the hosts. |
| `assets.default_kind` | required | The asset kind of a host no mapping matches, from the help desk's list. |
| `assets.kinds` | none | Mappings, in order: each a `template` (its visible name) or a `tag` (with an optional `value`), and the `kind` it gives; names and values compared ignoring case. |
| `alerts.rules` | the default list | The [rules for alerts](#alert-rules), in order. |

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
| `HD_ZABBIX_CONFIG` | The configuration file. |
| `HD_ZABBIX_LOG` | A `tracing` filter (default `info`). |

A secret's file is used with the surrounding whitespace (such as a final newline) removed.
Setting a variable and its `_FILE` together, or naming a missing or empty file, stops the add-on
with a message saying which.

## Alert rules

What an alert does is decided by the rules of `[[alerts.rules]]`
([ADR 0010](../../docs/adr/0010-monitoring-add-ons-open-tickets-by-explicit-rules.md)): an ordered
list where the **first rule that matches decides**. A rule matches when all its conditions hold; a
condition left out matches anything, so a rule without conditions matches every alert.

| Condition | Holds when |
|---|---|
| `min_severity` | The problem's severity is this one or higher: `not classified`, `information`, `warning`, `average`, `high` or `disaster`. |
| `customer` | The host is an asset of this customer, by name. A host no customer covers, or two do, has no customer. |
| `host_group` | One of the host's groups is this one or nested in it by name: `Brightfield` holds for `Brightfield/Offices/Floor 2`, not for `Brightfield East`. |
| `tags` | Each of these event tags is among the problem's: `{ tag = "service", value = "mail" }`, or `{ tag = "scope" }` for any value. |
| `trigger_contains` | The trigger's name, with its macros resolved as the problem shows it, contains this text. It is the event's name: for a trigger with an event name of its own (*Event name* in the trigger's form), that one. |

A rule names a customer or a host group, not both. Names, values and texts are compared ignoring
case. The outcome of a rule is `outcome = "ignore"`, or `outcome = "open"` with:

| Setting | Default | Meaning |
|---|---|---|
| `priority` | required | The ticket's priority: `low`, `normal`, `high` or `urgent`. |
| `service` | none | A service, by name, which the ticket links when the customer uses it. |
| `on_recovery` | `resolve_if_not_worked_on` | When the problem recovers: a note, and resolving the ticket only when no agent has worked on it; `resolve`, a note and resolving it in any case; or `note`, a note and never resolving it. |
| `close_in_zabbix` | `false` | Whether the add-on closes the problem in Zabbix when an agent resolves the ticket, if the trigger allows closing it by hand. |

A rule may have a `name`, which logs, errors and `check` show beside its position.

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
Customers, host groups and services are not looked up at startup: a name nobody has never
matches, which `check` shows.

## Checking a problem against the rules

`check` tells which rule a problem meets and with what outcome, or that none does, with the
configuration file and the secrets the add-on runs with. It writes nothing, neither to Zabbix nor
to the core.

```sh
hd-zabbix check --event 4213
hd-zabbix check --host srv-01 --severity high --tag service=mail --tag scope \
  --trigger "Mail queue is too long"
```

- `--event` gives a problem event by its id in Zabbix: its current severity, its tags (the
  trigger's, the items' and the host's) and its name come from Zabbix, and so do the hosts of its
  trigger, each checked in turn when there are several; a recovery event is refused.
- Otherwise `--host` (its technical or its visible name) and `--severity` are required, and
  `--tag` (`NAME` or `NAME=VALUE`, once for each tag) and `--trigger` are optional.

The host's groups come from Zabbix, and its customer from the customers' external references, read
from the core with the add-on's token as a round does. `check` prints the host with its groups and
whether it is an asset of a customer (a host that is not opens no ticket, whatever the rules say),
the problem as the rules see it, and the rule that decides with its outcome:

```text
Host "SRV-01", in the host group "Brightfield/Offices": an asset of Brightfield.
Problem: event 4213, severity high, trigger "Mail queue of SRV-01 is too long", the tags "service=mail" and "site=north".
Rule 2 ("Mail at Brightfield") matches: a ticket opened with priority high, linking the service "Hosted mail" when the customer uses it; on recovery, a note, and the ticket resolved only when no agent has worked on it; once an agent resolves the ticket, the problem left open in Zabbix.
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
`zabbix`. The test of `check` raises a real problem: it pushes a value to a trapper item with
`history.push` until the server, once it has read the new item, opens the problem of its trigger.
