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
These are for development and CI only. `podman compose up -d keycloak` starts it alone.

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
Production deployment: [`deploy.md`](deploy.md).

Real mail servers need TLS:

- `HD_IMAP_URL=imaps://user:password@host/INBOX` (port 993 unless given);
- `HD_SMTP_URL=smtps://user:password@host` (port 465) or `smtp://user:password@host?tls=required`
  (STARTTLS, port 587).

Percent-encode `@`, `/` and `:` inside the user or password. Certificates are checked against the
public web PKI; for servers with a private certificate authority, point `HD_MAIL_CA_FILE` at a PEM
file of that authority. Plain `imap://` and `smtp://` are accepted only for a server on this
machine, such as the test one.

To sign in through the test Keycloak, add (the redirect is registered for the core on 8080 and
for Vite on 5173):

```sh
HD_OIDC_ISSUER=http://127.0.0.1:55080/realms/helpdesk
HD_OIDC_CLIENT_ID=hd-core
HD_OIDC_CLIENT_SECRET=dev-only-client-secret
HD_OIDC_LABEL=Keycloak
HD_PUBLIC_URL=http://localhost:5173
```

and add an agent with a Keycloak user's address, such as `lena.marsh@example.com`. Use the same
host for the browser as in `HD_PUBLIC_URL`. [`deploy.md`](deploy.md#signing-in-through-an-identity-provider)
explains how the sign-in finds the agent. With these settings, `cargo run -p hd-core -- sign-in
local off` leaves only the Keycloak button on the sign-in page, and `sign-in local on` brings the
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

## Tests

```sh
cargo test                    # needs the database: each test gets a fresh one from sqlx
cargo test -- --ignored       # tests that also need the test mail server and Keycloak
```

## API specification

`cargo run -p hd-core -- openapi` prints the OpenAPI specification without touching the database; the running
core also serves it at `/api/openapi.json`.

`npm run api` (in `web/`) regenerates `web/openapi.json` and the TypeScript types in
`web/src/api/schema.d.ts` from the core; run it after changing the API. It runs openapi-typescript
through `npx` with its own TypeScript 5, because it does not support the TypeScript 7 the UI is
built with.
