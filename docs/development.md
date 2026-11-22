# Local development

## Prerequisites

- Rust (stable) and Node.js 24 with npm.
- Podman (or Docker) with compose, for PostgreSQL and the test mail server.

## Services

```sh
podman machine start          # once per boot, on Windows and macOS
podman compose up -d          # PostgreSQL on 127.0.0.1:55432, GreenMail on 55025 (SMTP) / 55143 (IMAP)
```

The host ports sit above 55000 because Windows often reserves ranges in the 3000s.

GreenMail is a mail server for tests only: it accepts any mail on its SMTP port and keeps it in the
mailbox `helpdesk@helpdesk.test` (login `helpdesk`, password `helpdesk`). Nothing leaves it.

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

Other variables: `HD_LISTEN` (default `127.0.0.1:8080`), `HD_MAIL_FROM_NAME` (default `Help desk`)
and `HD_LOG` (a `tracing` filter, default `info,sqlx=warn`).

Real mail servers need TLS:

- `HD_IMAP_URL=imaps://user:password@host/INBOX` (port 993 unless given);
- `HD_SMTP_URL=smtps://user:password@host` (port 465) or `smtp://user:password@host?tls=required`
  (STARTTLS, port 587).

Percent-encode `@`, `/` and `:` inside the user or password. Certificates are checked against the
public web PKI; for servers with a private certificate authority, point `HD_MAIL_CA_FILE` at a PEM
file of that authority. Plain `imap://` and `smtp://` are accepted only for a server on this
machine, such as the test one.

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
cargo test -- --ignored       # tests that also need the test mail server (TLS reading)
```

## API specification

`cargo run -p hd-core -- openapi` prints the OpenAPI specification without touching the database; the running
core also serves it at `/api/openapi.json`.

`npm run api` (in `web/`) regenerates `web/openapi.json` and the TypeScript types in
`web/src/api/schema.d.ts` from the core; run it after changing the API. It runs openapi-typescript
through `npx` with its own TypeScript 5, because it does not support the TypeScript 7 the UI is
built with.
