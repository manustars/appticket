# Technology stack

The core is written in Rust (Tokio, axum, sqlx) and stores its data in PostgreSQL; the agents' UI is
a separate TypeScript application (React, Vite) that talks to the core only through its REST API,
described by an OpenAPI specification generated from the core's code. The core reads mail over IMAP.
Core, SDK, UI and official add-ons live in this one repository.

## Considered options

- **SQLite instead of PostgreSQL.** Easier to run locally, but weaker at enforcing the rules in
  `docs/domain-rules.md` as database constraints and at concurrent writes.
- **An ORM (Diesel, SeaORM) instead of sqlx.** The domain rules are easier to enforce and review in
  hand-written SQL that sqlx checks at compile time.
- **Provider webhooks instead of IMAP.** Faster delivery, but they tie the core to one mail
  provider and require exposing the core to the internet; other channels can come later as add-ons.

## Consequences

The generated OpenAPI specification is the contract between core and UI (TypeScript types are
generated from it) and the base of the SDK.
