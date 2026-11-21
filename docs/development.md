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
```

Other variables: `HD_LISTEN` (default `127.0.0.1:8080`) and `HD_LOG` (a `tracing` filter, default
`info,sqlx=warn`).

A real mailbox needs TLS: `HD_IMAP_URL=imaps://user:password@host/INBOX` (port 993 unless given;
percent-encode `@`, `/` and `:` inside the user or password). The server's certificate is checked
against the public web PKI; for a server with a private certificate authority, point
`HD_IMAP_CA_FILE` at a PEM file of that authority. Plain `imap://` is accepted only for a server on
this machine, such as the test one.

## First run

```sh
cargo run -- migrate
cargo run -- customer add "Northgate Studio" --help-desk --domain northgate.test
cargo run -- customer add "Brightfield" --domain brightfield.test
HD_AGENT_PASSWORD=dev-password cargo run -- agent add marco@northgate.test --name "Marco Lenti"
cargo run -- serve
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

## Tests

```sh
cargo test                    # needs the database: each test gets a fresh one from sqlx
cargo test -- --ignored       # tests that also need the test mail server (TLS reading)
```

## API specification

`cargo run -- openapi` prints the OpenAPI specification without touching the database; the running
core also serves it at `/api/openapi.json`.

`npm run api` (in `web/`) regenerates `web/openapi.json` and the TypeScript types in
`web/src/api/schema.d.ts` from the core; run it after changing the API. It runs openapi-typescript
through `npx` with its own TypeScript 5, because it does not support the TypeScript 7 the UI is
built with.
