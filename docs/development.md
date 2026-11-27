# Local development

## Prerequisites

- Rust (stable) and Node.js 24 with npm.
- Podman (or Docker) with compose, for PostgreSQL, the test mail server and the test identity
  provider.

## Services

```sh
podman machine start          # once per boot, on Windows and macOS
podman compose up -d          # PostgreSQL on 127.0.0.1:55432, GreenMail on 55025 (SMTP) / 55143 (IMAP),
                              # Keycloak on 55080
```

The host ports sit above 55000 because Windows often reserves ranges in the 3000s.

GreenMail is a mail server for tests only: it accepts any mail on its SMTP port and keeps it in the
mailbox `helpdesk@helpdesk.test` (login `helpdesk`, password `helpdesk`). Nothing leaves it.

Keycloak, on 127.0.0.1:55080, is a test identity provider for signing in through OpenID Connect.
It imports `dev/keycloak/helpdesk-realm.json` when it starts and keeps nothing: realm `helpdesk`,
client `hd-core` with the secret `dev-only-client-secret`, and the users `lena`, `otto`, `nadia`
and `piet` (password `<user>-dev-password`, addresses `@example.com`; Otto's is not verified).
Lena, Otto and Piet are in the group `helpdesk-agents`, which the client sends in the ID token's
`groups` claim; Nadia is not.
These are for development and CI only. `podman compose up -d keycloak` starts it alone.

A test Zabbix 7.0, for the Zabbix add-on, starts only with its profile:

```sh
podman compose --profile zabbix up -d    # its frontend and API on 127.0.0.1:55180
```

Its server, frontend and database are three containers (`zabbix-server`, `zabbix-web`,
`zabbix-db`); at the first start the server sets up the database, which takes a minute or two
before the API answers. Sign in as `Admin` with the password `zabbix`. Nothing is kept once the
containers are removed with `podman compose --profile zabbix rm -sfv zabbix-web zabbix-server
zabbix-db` (not `down -v`, which would also remove the help desk's own database volume).

## Configuration

Create `.env` at the repository root (it is ignored by git):

```sh
HD_DATABASE_URL=postgres://hd:hd@127.0.0.1:55432/hd
# Used by the database tests (`#[sqlx::test]`); same server.
DATABASE_URL=postgres://hd:hd@127.0.0.1:55432/hd
HD_IMAP_URL=imap://helpdesk:helpdesk@127.0.0.1:55143/INBOX
HD_IMAP_POLL_SECONDS=5
# Replies: the address they come from, and the server that sends them.
HD_MAIL_FROM=helpdesk@helpdesk.test
HD_SMTP_URL=smtp://127.0.0.1:55025
```

Other variables: `HD_LISTEN` (default `127.0.0.1:8080`), `HD_MAIL_FROM_NAME` (default `Help desk`),
`HD_LOG` (a `tracing` filter, default `info,sqlx=warn`) and `HD_WEB_DIR` (the built UI, `web/dist`,
for the core to serve itself, as the container image does; in development Vite serves the UI).
`HD_SIGN_IN_FAILURES` (default 5), `HD_SIGN_IN_WINDOW_SECONDS` (default 900) and
`HD_TRUSTED_PROXIES` (none by default) set the limits on failed sign-ins and the reverse proxies
whose `X-Forwarded-For` is believed; Vite's development proxy is not one of them, so in
development every sign-in counts under `127.0.0.1`.
Production deployment: [`deploy.md`](deploy.md), on Kubernetes [`kubernetes.md`](kubernetes.md);
on a PostgreSQL database of your own:
[`external-database.md`](external-database.md).

Instead of `HD_DATABASE_URL`, the database can be given as separate parameters
([`external-database.md`](external-database.md#connecting)).

The database on `127.0.0.1` is reached as before, with TLS only if the server offers it; one on
another host is reached with verified TLS unless `HD_DATABASE_SSLMODE` says otherwise
([`external-database.md`](external-database.md#tls)).

Real mail servers need TLS:

- `HD_IMAP_URL=imaps://user:password@host/INBOX` (port 993 unless given);
- `HD_SMTP_URL=smtps://user:password@host` (port 465) or `smtp://user:password@host?tls=required`
  (STARTTLS, port 587).

Percent-encode `@`, `/` and `:` inside the user or password. Either URL can be read from a file
instead, with `HD_IMAP_URL_FILE` and `HD_SMTP_URL_FILE`, as every secret of the core. Certificates
are checked against the public web PKI; for servers with a private certificate authority, point
`HD_MAIL_CA_FILE` at a PEM file of that authority. Plain `imap://` and `smtp://` are accepted only
for a server on this machine, such as the test one.

To sign in through the test Keycloak, add (the redirect is registered for the core on 8080 and
for Vite on 5173):

```sh
HD_OIDC_ISSUER=http://127.0.0.1:55080/realms/helpdesk
HD_OIDC_CLIENT_ID=hd-core
HD_OIDC_CLIENT_SECRET=dev-only-client-secret
HD_OIDC_LABEL=Keycloak
HD_PUBLIC_URL=http://localhost:5173
```

and add an agent with a Keycloak user's address, such as `lena.marsh@example.com`; or add
`HD_OIDC_AUTHORIZED_GROUP=helpdesk-agents`, so that Lena, Otto and Piet become agents at their
first sign-in and Nadia is refused. Use the same
host for the browser as in `HD_PUBLIC_URL`. [`deploy.md`](deploy.md#signing-in-through-an-identity-provider)
explains how the sign-in finds the agent. With these settings, `cargo run -p hd-core -- sign-in
check` says whether Keycloak answers and accepts the client, `sign-in local off` leaves only the Keycloak button on the sign-in page, and `sign-in local on` brings the
password back.

The first time the core reads a mailbox it leaves the mail already there alone and reads only what
arrives afterwards; set `HD_IMAP_IMPORT_EXISTING=true` to turn the existing mail into tickets too.

Replies wait in an outbox in the database and are sent every few seconds; a failed attempt is
retried after 30 s, then twice as long each time (up to an hour), eight times in all; after that
the outbox gives up and the ticket shows the message as not sent. The test mail
server accepts every recipient, so replies sent locally never leave it.

## First run

```sh
cargo run -p hd-core -- migrate
cargo run -p hd-core -- customer add "Northgate Studio" --help-desk --domain northgate.test
cargo run -p hd-core -- customer add "Brightfield" --domain brightfield.test
HD_AGENT_PASSWORD=dev-password cargo run -p hd-core -- agent add marco@northgate.test --name "Marco Lenti"
cargo run -p hd-core -- serve
```

As the first agent, Marco gets the Administrator role (`hd role list` and `hd agent --help` show
the rest; passwords need at least 12 characters).

In another terminal, the UI:

```sh
cd web
npm install
npm run dev                   # http://localhost:5173, /api is proxied to the core
```

## Sending a test mail

```sh
printf 'From: Ada Ferrini <ada@brightfield.test>\r\nTo: helpdesk@helpdesk.test\r\nMessage-ID: <first@brightfield.test>\r\nSubject: PC will not start\r\n\r\nIt shows a black screen.\r\n' > first.eml
curl smtp://127.0.0.1:55025 --mail-from ada@brightfield.test --mail-rcpt helpdesk@helpdesk.test -T first.eml
```

Within `HD_IMAP_POLL_SECONDS` the core reads it: a sender whose domain belongs to a customer opens a
ticket, any other sender lands in the inbox.

## The Assistant

The Assistant is an add-on (`addons/assistant`, details in its README). To try it locally:

```sh
cargo run -p hd-core -- addon add assistant --url http://127.0.0.1:8090   --event message.received --event ticket.opened   --scope directory:read --scope tickets:read --scope proposals:write
# copy the printed token and event secret into .env, and choose the model:
#   HD_ASSISTANT_CORE_URL=http://127.0.0.1:8080
#   HD_ASSISTANT_TOKEN=…
#   HD_ASSISTANT_EVENT_SECRET=…
#   HD_ASSISTANT_MODEL_PROVIDER=…  # anthropic, openai, mistral, gemini, azure-openai, ollama or openai-compatible
#   HD_ASSISTANT_MODEL=…           # a model of that provider
#   HD_ASSISTANT_API_KEY=…         # for the hosted providers: customers' mail goes to that service
cargo run -p hd-assistant
```

There is no default provider or model; the Assistant does not start until both are set. Its
README lists what each provider needs. With a hosted provider every message costs a model call;
`HD_ASSISTANT_MODEL_PROVIDER=ollama` keeps the mail on this machine.

A self-hosted model (`ollama`, `openai-compatible`) gets one call at a time, since such servers are
often a single GPU shared with other work; a hosted provider gets four.
`HD_ASSISTANT_MODEL_CONCURRENCY` changes that. A model that thinks before answering can take tens
of seconds per message, so proposals appear accordingly later.

## The Zabbix add-on

The Zabbix add-on (`addons/zabbix`, details in its README) makes the hosts of the host groups
customers have into their assets, and the alerts its rules open into tickets. With the test
Zabbix above, make an API token for `Admin` in its frontend (*Users → API tokens*), then:

```sh
cargo run -p hd-core -- addon add zabbix --url http://127.0.0.1:8091 \
  --scope directory:read --scope catalog:write --scope names:write --scope conflicts:write \
  --scope tickets:read --scope tickets:write --event ticket.state_changed
cp addons/zabbix/zabbix.example.toml zabbix.toml   # core.url http://127.0.0.1:8080,
                                                   # zabbix.url http://127.0.0.1:55180
# in .env: HD_ZABBIX_TOKEN=… and HD_ZABBIX_EVENT_SECRET=… (printed above) and
# HD_ZABBIX_API_TOKEN=… (Zabbix's)
cargo run -p hd-zabbix
```

Its host groups then appear on a customer's page among the names to pick an external reference
in `Zabbix` from, and the assets of its hosts show their live details from Zabbix. For alerts, add
a `[webhook]` section with a secret file, listen on an address the Zabbix server's container
reaches, and import and set up the media type as the add-on's README says (*Alerts*).

## Tests

```sh
cargo test                    # needs the database: each test gets a fresh one from sqlx
cargo test -- --ignored       # tests that also need the test mail server, Keycloak, the TLS test
                              # database and the test Zabbix
```

The Zabbix add-on's ignored tests start the core's binary, so build it first
(`cargo build -p hd-core`), and reach the test Zabbix at `HD_TEST_ZABBIX_URL` (default
`http://127.0.0.1:55180`); CI runs them in a job of their own. The tests of alerts serve the
add-on's webhook to the Zabbix server, which reaches them at `HD_TEST_ZABBIX_WEBHOOK_HOST`
(default `host.docker.internal`); with Podman on Windows or macOS give the host's address as its
virtual machine sees it (the add-on's README says how).

The help desk supports PostgreSQL 16 and later: CI runs the core's tests on 16 and on 18, the
version of `compose.yaml`. To run them on 16 locally, start a server of that version on a port of
its own and point `DATABASE_URL` at it:

```sh
podman run -d --name hd-pg16 -p 127.0.0.1:55416:5432 \
  -e POSTGRES_USER=hd -e POSTGRES_PASSWORD=hd -e POSTGRES_DB=hd docker.io/library/postgres:16
DATABASE_URL=postgres://hd:hd@127.0.0.1:55416/hd cargo test
podman rm -f -v hd-pg16
```

The test of verified TLS to the database (`core/tests/database_tls.rs`) connects with
`verify-full` to a PostgreSQL serving TLS with a certificate from a test authority, with and
without that authority in `HD_DATABASE_CA_FILE`. `scripts/tls-test-database.sh` makes the
authority and the certificate with openssl, into the folder it is given (never the repository),
starts the server in the container `hd-dev-db-tls` on 127.0.0.1:55455, and prints the two
variables the test reads:

```sh
bash scripts/tls-test-database.sh target/tls-test-database
# HD_TEST_TLS_DATABASE_URL=postgres://hd:hd@127.0.0.1:55455/hd
# HD_TEST_TLS_DATABASE_CA_FILE=…/target/tls-test-database/ca.crt
podman rm -f -v hd-dev-db-tls  # when done
```

Add the two lines to `.env` once: the script writes the same files each time it starts the
server again. Without them the test says it is skipped, except in CI, where it fails;
`CONTAINER_ENGINE=docker` runs the script with Docker, as CI does.

The tests that run the `hd` binary (`core/tests/`) never give it the database of `DATABASE_URL`,
which is the one the core runs on during development: each creates a database of its own on that
server and drops it afterwards, also when the test fails, so no test migrates the development
database to the migrations of the branch it runs from. The tests of the separate database
parameters (`core/tests/database_parameters.rs`) also create a PostgreSQL user of their own that
owns it. The user of `DATABASE_URL` therefore needs to be allowed to create both: `hd`, the test
server's user, is a PostgreSQL superuser.

## API specification

`cargo run -p hd-core -- openapi` prints the OpenAPI specification without touching the database; the running
core also serves it at `/api/openapi.json`.

`npm run api` (in `web/`) regenerates `web/openapi.json` and the TypeScript types in
`web/src/api/schema.d.ts` from the core; run it after changing the API. It runs openapi-typescript
through `npx` with its own TypeScript 5, because it does not support the TypeScript 7 the UI is
built with.
