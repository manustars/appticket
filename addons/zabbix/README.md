# Zabbix add-on

The Zabbix add-on brings the hosts a Zabbix monitors into the help desk's catalog: the hosts of
the host groups a customer has as external references become that customer's assets, with the
add-on as their source ([ADR 0002](../../docs/adr/0002-core-owns-the-asset-catalog.md)). It talks to
Zabbix 7.0 LTS or later through `api_jsonrpc.php`, with an API token sent in
`Authorization: Bearer`.

Alerts, the tickets they open and what goes back to Zabbix
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
   - kind: from the first mapping of `assets.kinds` that matches one of the host's templates or
     tags, else `assets.default_kind`;
   - serial number: the inventory's `serialno_a`, when the inventory is on and has one;
   - no site, and no holder: agents give it one;
3. retires the assets of hosts that left Zabbix or that no group of their customer covers any
   more, and reports the hosts it does not import because their groups are covered by two
   customers, as conflicts with a sentence in English and Italian.

The asset's reference combines the host's id and the customer's (`10084@<customer id>`), since a
reference names one asset of one customer for good: a host that moves to another customer's
groups retires its asset at the old customer and becomes a new asset of the new one.

- A host no external reference covers is not imported, and is no conflict: its groups show among
  the names no customer covers, in the add-on's state.
- A host in a group an offboarded customer has, or in one nested in it, is imported for nobody,
  even inside an active customer's group; one also in an active customer's group is a conflict.
- A host that becomes a conflict retires its asset, which comes back once the host is in the
  groups of one customer again.
- When the customer already has an asset with the host's serial number or name, entered by hand
  or supplied by another add-on, the core records a link proposal instead of a second asset, and
  the add-on carries on. Once agents accept it, the add-on leaves that asset as it is, and unlinks
  itself when the host leaves Zabbix or its customer.
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

A file that does not fit, such as an unknown setting or a mapping with both a template and a tag,
stops the add-on with a sentence naming it. A kind the help desk does not have is refused by the
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
`zabbix`.
