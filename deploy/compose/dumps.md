# Dumps of the Compose install

The service `dumps-schedule` dumps the help desk's database with `pg_dump` every day at
`HD_DUMPS_TIME` (`.env`, a time in UTC, `02:30` when not set), in PostgreSQL's custom format,
compressed, to the volume `dumps`, and keeps the latest `HD_DUMPS_KEEP` (7 when not set). It is on
with the profile `db`, whose database has no other backup, and turned on beside an existing
database by adding the profile `dumps` to `COMPOSE_PROFILES`. The chart's dumps work the same way
([docs/kubernetes.md](../../docs/kubernetes.md#database-dumps)).

Each dump is named after the time it was taken, in UTC, such as `hd-20261125T023000Z.dump`. It is
written under a temporary name (`.partial`) and renamed when complete, so an interrupted dump
never counts as one and never takes the place of an older dump; the next dump removes what it
left. One dump runs at a time: one asked for while another runs waits for it. A dump that fails
leaves the older ones and is in the log of `dumps-schedule` (`docker compose logs
dumps-schedule`), and the schedule goes on.

The volume `dumps` stays on `docker compose down`, and goes with `docker compose down -v`, which
removes the database's volume too. It is on the same disk as the database: copy the dumps off the
host as well.

## Taking one now

```sh
docker compose run --rm dumps
```

It says `dumped hd-20261125T093012Z.dump; kept:`, then lists the volume. With the profile `db` it
starts the bundled PostgreSQL first when it is not running.

## Listing them and copying them off the host

```sh
docker compose run --rm dumps ls -l /dumps
docker compose run --rm -T dumps cat /dumps/hd-20261125T023000Z.dump > hd-20261125T023000Z.dump
```

`-T` gives the dump through as it is, without a terminal in between.

## How the dumps reach the database

As the core does: the host, port, name and user of `HD_DATABASE_*`, and the password of
`secrets/db_password`, read when a dump starts and given to `pg_dump` alone, never in the
container's environment. With the profile `db`, that is the bundled PostgreSQL, without TLS
(`HD_DATABASE_SSLMODE=disable`).

An existing database is reached with `HD_DATABASE_SSLMODE`, `verify-full` when not set
([TLS](../../docs/external-database.md#tls)). The dumps' image holds no certificate authority,
though, while the core trusts the public ones: for `verify-full` and `verify-ca`, the dumps
require TLS without verifying the server's certificate (`sslmode=require`), and say so in a
warning at each dump. A weaker mode is used as it is. The Compose install gives no
`HD_DATABASE_CA_FILE` yet; once it does, the dumps verify the certificate against that authority
as the core does.

`pg_dump` must be at least as new as the server: the image is PostgreSQL 18, that of the profile
`db`. For an existing database of a later major version, set `HD_DUMPS_IMAGE_TAG` in `.env` to a
tag of `docker.io/library/postgres` of that version or newer.

## Restoring into a fresh install

A fresh install, with the profile `db`, takes a dump before its core first starts. When it starts,
the core applies its migrations to an empty database; restored first, the database has the
dump's tables and migrations, and the core applies only those of a newer version. Use the version
of the help desk that took the dump, or a newer one.

In the new install's directory, with its `.env` set up for the profile `db` and the dump copied
there:

```sh
chmod 700 secrets
docker compose run --rm db-password
docker compose run --rm -T dumps restore < hd-20261125T023000Z.dump
docker compose up -d
```

The third command starts the bundled PostgreSQL alone, which creates its database with the new
password, and restores the dump into it; it ends with `restored`. The agents sign in with the
passwords they had.

`pg_restore` replaces what the dump holds (`--clean --if-exists`), in one transaction: a restore
that fails changes nothing, and says why. The objects restored belong to `HD_DATABASE_USER`
(`--no-owner --no-privileges`), so the old install's user may have had another name. If the core
of the new install has already started, start over with its database removed before restoring:
`docker compose down -v`, then the last two commands.

Without the profile `db`, the same command restores into the existing database of `HD_DATABASE_*`:
a new, empty one ([What the DBA creates](../../docs/external-database.md#what-the-dba-creates)),
before the core first starts on it.
