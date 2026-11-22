# Working on appticket

## Settled decisions

- **Public repository.** Core and add-ons are AGPL-3.0 (`LICENSE`); the SDK is Apache-2.0
  (`sdk/LICENSE`) so that add-ons can carry any license.
- **English is the language of the project:** code, identifiers, database objects, API routes,
  docs, commit messages. English is also the source locale of the UI; Italian is a translation.
  UI strings live in `web/src/i18n/locales/<language>.json`; `en.json` is the source, and the
  build fails when another language misses a key or changes a placeholder (`web/src/i18n/README.md`).
- **The product name is provisional.** It lives in one configurable place and stays out of
  identifiers: environment variables, cookies, headers and package names use the neutral prefix
  `hd` (`HD_…`, `hd_session`, `hd-core`).
  The SDK is published to crates.io or npm only once the name is final.
- **CI runs on GitHub-hosted runners** (`ubuntu-latest`).
- **Domain language** is in `GLOSSARY.md`, the rules the schema must respect in
  `docs/domain-rules.md`, and decisions with their reasons in `docs/adr/`. Use the glossary's terms
  in code, docs and UI source strings.
- **A ticket refers to assets in a thin catalog the core owns;** add-ons can feed and enrich it
  ([ADR 0002](docs/adr/0002-core-owns-a-thin-asset-catalog.md)). What the help desk provides to
  several customers is a service, not an asset
  ([ADR 0003](docs/adr/0003-services-are-not-assets.md)).
- **Stack:** Rust core (Tokio, axum, sqlx) on PostgreSQL, TypeScript UI (React, Vite) over a REST
  API described by a generated OpenAPI specification, mail read over IMAP
  ([ADR 0004](docs/adr/0004-technology-stack.md)).
- **Core and add-ons talk JSON over HTTP:** events stored by the core and delivered with retries,
  actions through the core's API, reads with a short timeout; no message broker
  ([ADR 0005](docs/adr/0005-add-on-communication.md)).
- **Add-ons** live in `addons/` (the Assistant: [ADR 0006](docs/adr/0006-assistant.md)) and are
  built on the Rust SDK in `sdk/rust`; their contract with the core is `sdk/README.md`. The
  workspace has several binaries: run the core with `cargo run -p hd-core -- …`.
- **Agents have one role each;** the product defines the permissions, the help desk the roles,
  and every request that changes something checks its permission
  ([ADR 0007](docs/adr/0007-roles-and-permissions.md)).
- **Local development** runs PostgreSQL and a test mail server (GreenMail) in containers with
  Podman. Agents sign in with e-mail and password managed by the core; OIDC comes later.

## Clean repository

Every file, file name and commit message is original to this project. Test data uses the
documentation IP ranges (`192.0.2.0/24`, `198.51.100.0/24`, `203.0.113.0/24`), reserved domains
(`example.com`, `.test`, `.invalid`) and invented names. Commits use the maintainer's GitHub
noreply address.

`scripts/scan-clean.sh` enforces this; the hooks in `.githooks/` run it on every commit, commit
message and push (enable them with `git config core.hooksPath .githooks`). When it reports a hit,
rewrite the content. Widening its allowlists is the maintainer's call.
