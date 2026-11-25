# Secrets of the Compose install

The passwords, the mail servers' URLs and the tokens of the install are files in this directory,
one secret per file, and never in `.env`. `compose.yaml` mounts each into the containers that
read it, at `/run/secrets/<file>`, and gives the core and the Assistant its path in the matching
`_FILE` variable ([Secrets from files](../../../docs/external-database.md#secrets-from-files));
the bundled PostgreSQL reads its password through `POSTGRES_PASSWORD_FILE`. Compose shows the
files' paths, never their content: neither `docker compose config` nor `docker inspect` holds a
secret.

| File | Holds | Needed |
|---|---|---|
| `db_password` | The password of `HD_DATABASE_USER`. | Always. |
| `imap_url` | `HD_IMAP_URL`, the mailbox the core reads. | To read messages, with `HD_SECRET_IMAP_URL` in `.env`. |
| `smtp_url` | `HD_SMTP_URL`, the server the core sends through. | To send replies, with `HD_SECRET_SMTP_URL` in `.env`. |
| `oidc_client_secret` | `HD_OIDC_CLIENT_SECRET`. | To sign in through OpenID Connect, with `HD_SECRET_OIDC_CLIENT_SECRET` in `.env`. |
| `assistant_token` | The token `hd addon add` printed. | With the profile `assistant`. |
| `assistant_event_secret` | The event secret `hd addon add` printed. | With the profile `assistant`. |
| `assistant_api_key` | `HD_ASSISTANT_API_KEY`, the model service's key. | For a provider that takes one, with `HD_SECRET_ASSISTANT_API_KEY` in `.env`. |

A file holds the value alone; the surrounding whitespace, such as a final newline, is removed. The
optional files are used only when `.env` names them (the `HD_SECRET_*` lines of `.env.example`):
Compose cannot start a container whose secret's file is missing, so for a secret `.env` does
not name `compose.yaml` mounts `/dev/null` instead and gives the service no `_FILE` variable.

## Permissions

The directory belongs to the user that runs Compose, and only that user can open it; the files in
it are readable by everyone:

```sh
chmod 700 secrets
chmod 644 secrets/*
```

A clone or an unpacked archive gives the directory the usual mode (`755`): `chmod 700` it before
writing the first secret. The files must be readable by everyone because the containers read
them as users of their own, not as the user that runs Compose: the core and the Assistant as
10001, PostgreSQL as 999 (as root first), the dumps as root without any capability. Compose
mounts each file on its own, so a container never goes through the directory, and no other user
of the host can.

A file written with the usual `umask` (`022`) is `644` already:

```sh
printf '%s\n' 'imaps://helpdesk:password@mail.example.com/INBOX' > secrets/imap_url
```

A secret is read when its service starts. After changing one, recreate the service:
`docker compose up -d --force-recreate core` (or `assistant`).

## The database's password

With the profile `db`, run once before the first start, after closing the directory:

```sh
chmod 700 secrets
docker compose run --rm db-password
```

The second command writes a random password to `db_password`, belonging to the same user as the
directory, and the bundled PostgreSQL is created with it. Run again, it keeps the file there; it
warns when the directory is not `700`. Keep the file as long as the volume `db-data`: the
database keeps the password it was created with, and a new file does not change it.

Without the profile `db`, write the password of the existing database's user to `db_password`
([docs/external-database.md](../../../docs/external-database.md)).

## The Assistant's token and event secret

Registering the Assistant on the core prints both once; write them to their files, then start the
Assistant with the profile `assistant`:

```sh
docker compose exec -T core hd addon add assistant --url http://assistant:8090 \
  --event message.received --event ticket.opened \
  --scope directory:read --scope tickets:read --scope proposals:write > secrets/addon.txt
sed -n 's/^token: //p' secrets/addon.txt > secrets/assistant_token
sed -n 's/^event secret: //p' secrets/addon.txt > secrets/assistant_event_secret
rm secrets/addon.txt
```
