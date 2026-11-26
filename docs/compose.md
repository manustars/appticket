# Installing with Compose

The help desk installs on a single host with Docker Compose, or with Podman (`podman compose`),
from the archive attached to each GitHub release, `hd-compose-<version>.tar.gz`
([ADR 0009](adr/0009-two-ways-to-install-three-ways-to-get-the-database.md)).
[Deploying the help desk](deploy.md) describes the images and the archive, and what to do once
the help desk runs: the first agent, roles, signing in through an identity provider, importing
customers, the Assistant. What is the same for every install, a PostgreSQL database of your own,
TLS to it and secrets read from files, is in
[Running on a PostgreSQL database of your own](external-database.md). On Kubernetes, the help
desk installs with its Helm chart instead ([Installing on Kubernetes](kubernetes.md)).

The commands on this page are Docker's, run in the install's directory; with Podman, `podman
compose` takes the place of `docker compose` ([Podman](#podman)).

## Prerequisites

- Docker Engine with Compose 2.20 or later (`docker compose version`), or Podman with Docker
  Compose as its compose provider ([Podman](#podman)).
- `curl`, `sha256sum` and `tar`, to download and unpack the archive (on macOS, `shasum -a 256`
  in place of `sha256sum`).
- For HTTPS, which the agents' UI needs (the sign-in cookie is marked `Secure`): a reverse proxy
  on the host, or the ports 80 and 443 for the Caddy that the install can start ([HTTPS](#https)).
- For your own PostgreSQL, a database prepared as
  [What the DBA creates](external-database.md#what-the-dba-creates) says: PostgreSQL 16 or later,
  dedicated to the help desk and owned by the core's user. Without one, the install runs
  PostgreSQL itself.
- For real mail: an IMAP mailbox and an SMTP server reachable over TLS (`imaps://`, `smtps://` or
  `smtp://…?tls=required`).

## Install

Download the archive of a version and its checksum, check it, and unpack it. `1.2.3` stands for
the version, without the `v` of its tag:

```sh
version=1.2.3
base=https://github.com/manustars/appticket/releases/download/v$version
curl -fLO "$base/hd-compose-$version.tar.gz"
curl -fLO "$base/hd-compose-$version.tar.gz.sha256"
sha256sum -c "hd-compose-$version.tar.gz.sha256"
tar -xzf "hd-compose-$version.tar.gz"
cd "hd-compose-$version"
cp .env.example .env
```

The archive unpacks into `hd-compose-<version>/`, whose `compose.yaml` runs the images of that
version. Keep the directory where it is: the next version unpacks beside it
([Upgrades](#upgrades)).

### The settings

`.env` holds the install's settings, and `.env.example`, which it was copied from, describes each
of them. Set at least:

- `COMPOSE_PROFILES`: what runs beside the core. `db`, as it ships, is the install's own
  PostgreSQL; leave it out for [an existing database](#with-an-existing-database). Add
  `https` for [Caddy](#https), `assistant` for [the Assistant](#first-steps), `greenmail` for a
  test mail server, not for production.
- `HD_PUBLIC_URL`: the `https://` address agents open the help desk at, with no path.
- `HD_HTTP_PORT` (and `HD_HTTP_ADDRESS`): where the core is published on the host for the proxy
  in front of it, `127.0.0.1:8080` unless set.
- The mail servers: `HD_SECRET_IMAP_URL`, `HD_SECRET_SMTP_URL` and `HD_MAIL_FROM`, with the URLs
  in files of `secrets/` ([Secrets of the Compose install](../deploy/compose/secrets/README.md)).
  Without them the core reads and sends no mail.

Nothing secret goes in `.env`: the passwords, the mail servers' URLs and the tokens are files in
`secrets/`, which [its README](../deploy/compose/secrets/README.md) lists with the permissions
they need. A setting changed later takes effect with `docker compose up -d`, which recreates the
services whose settings changed; a secret changed later, as the README says.

`compose.yaml` names the Compose project `hd`, whatever the directory: the containers, the
network `hd_default` and the volumes `hd_db-data` and `hd_dumps` are named after it, so that the
directory of the next version finds them. A second install on the same host needs another name,
`COMPOSE_PROJECT_NAME` in its `.env`, besides a port (`HD_HTTP_PORT`) and a subnet (`HD_SUBNET`,
[reverse-proxy.md](../deploy/compose/reverse-proxy.md#the-proxys-address-and-hd_subnet)) of its
own; its network and volumes are then named after that name where these pages say `hd_`.

### With the install's own PostgreSQL

With the profile `db`, keep the database's settings of `.env.example` as they are. Close
`secrets/` to the other users of the host, write the database's password once, and start:

```sh
chmod 700 secrets
docker compose run --rm db-password
docker compose up -d
docker compose ps
curl -sS http://127.0.0.1:8080/api/health
```

The second command writes a random password to `secrets/db_password`, which the database is
created with on its first start
([The database's password](../deploy/compose/secrets/README.md#the-databases-password)). The
core waits for the database, applies its migrations and serves: `/api/health`, on the port of
`HD_HTTP_PORT`, answers `{"status":"ok"}` once it reaches the database (and 503 while it does
not); run `curl` again if the core has not started yet. The database's dumps run every day at
02:30 UTC ([Backups](#backups)).

### With an existing database

Without the profile `db`, the core uses the database that `.env` names: `HD_DATABASE_HOST`,
`HD_DATABASE_PORT`, `HD_DATABASE_NAME` and `HD_DATABASE_USER`, as
[Connecting](external-database.md#connecting) describes them. Remove the line
`HD_DATABASE_SSLMODE=disable`, so that the core verifies the database's certificate
(`verify-full`, [TLS](external-database.md#tls)), and add the profile `dumps` to
`COMPOSE_PROFILES` for the dumps of [Backups](#backups), unless the database is backed up
otherwise. The password of the database's user is the file `secrets/db_password`:

```sh
chmod 700 secrets
printf '%s\n' 'Lantern@river/72:quiet' > secrets/db_password
docker compose up -d
```

The Compose install gives no `HD_DATABASE_CA_FILE` yet: the certificate of a database on another
host must come from a public authority, or `HD_DATABASE_SSLMODE` be set to a weaker mode, which
the core logs at every start.

## HTTPS

The core serves plain HTTP; HTTPS is the job of a reverse proxy in front of it. What any proxy
must do (serve `HD_PUBLIC_URL`, add `X-Forwarded-For`, accept requests of 10 MiB) is in
[Behind a reverse proxy](deploy.md#behind-a-reverse-proxy). With Compose, it is one of:

- **The proxy the host already has**, such as nginx, Traefik or Caddy, passing requests on to
  `http://127.0.0.1:8080` (`HD_HTTP_PORT`).
  [deploy/compose/reverse-proxy.md](../deploy/compose/reverse-proxy.md) says which address the
  core sees it connect from, the gateway of the install's network `HD_SUBNET`, which the core
  trusts as its proxy unless `HD_TRUSTED_PROXIES` says otherwise, and has an example for each:
  [nginx](../deploy/compose/examples/nginx.conf), the one the install's test runs,
  [Traefik](../deploy/compose/examples/traefik.yaml), not tested, and
  [Caddy](../deploy/compose/examples/Caddyfile).
- **Caddy, started by the install**, for a host with no proxy of its own: add `https` to
  `COMPOSE_PROFILES`, and choose its certificate with `HD_HTTPS_CERTIFICATE`: `acme` (Let's
  Encrypt, the default, for a host name that leads to this host from the Internet), `files` (your
  own, in `secrets/tls_certificate` and `secrets/tls_key`) or `internal` (Caddy's own authority,
  for a LAN or a trial).
  [The profile `https`](../deploy/compose/reverse-proxy.md#caddy-the-profile-https) says what
  each needs.

## First steps

The `hd` command runs inside the core's container, with the core's database settings. The help
desk's own organization and the first agent, whose password is generated and printed once:

```sh
docker compose exec core hd customer add "Northgate Studio" --help-desk --domain example.com
docker compose exec core hd agent add agent@example.com --name "Ada Agent"
```

The agent then signs in at `HD_PUBLIC_URL`. [Deploying the help desk](deploy.md#first-steps)
goes on from there, the same for every install, with the commands shown for Kubernetes: where it
has `kubectl -n helpdesk exec deploy/hd-core -- hd …`, run `docker compose exec core hd …`, and
`docker compose exec -T core hd …` where it has `exec -i` to give a file on standard input. Where
it sets values of the chart, set the environment variables it names in `.env`; `.env.example`
lists those the install passes on. The identity provider's client secret is the file
`secrets/oidc_client_secret`, and the Assistant is registered as
[secrets/README.md](../deploy/compose/secrets/README.md#the-assistants-token-and-event-secret)
says, its token and event secret written to files of `secrets/`, then started with the profile
`assistant`.

## Backups

With the profile `db`, and with `dumps` beside an existing database, the service `dumps-schedule`
dumps the database every day at `HD_DUMPS_TIME` (02:30 UTC) to the volume `dumps`, which keeps
the latest `HD_DUMPS_KEEP` (7). `docker compose run --rm dumps` takes one now.
[Dumps of the Compose install](../deploy/compose/dumps.md) says how they reach the database, how
to list them and how to copy them off the host, which they share a disk with.

## Upgrades

Each version is a directory of its own: the new one is unpacked beside the old, takes its `.env`
and its secrets, and finds the same volumes, named after the project. Read the release's notes
first. From the directory that holds the old version, `1.2.3` here, with the new one, `1.2.4`:

1. Download and check the new version's archive, copy the old version's `.env` and `secrets/`
   into its directory, and unpack the archive over them:

   ```sh
   old=1.2.3
   version=1.2.4
   base=https://github.com/manustars/appticket/releases/download/v$version
   curl -fLO "$base/hd-compose-$version.tar.gz"
   curl -fLO "$base/hd-compose-$version.tar.gz.sha256"
   sha256sum -c "hd-compose-$version.tar.gz.sha256"
   mkdir "hd-compose-$version"
   cp -pR "hd-compose-$old/.env" "hd-compose-$old/secrets" "hd-compose-$version/"
   tar -xzf "hd-compose-$version.tar.gz"
   diff "hd-compose-$old/.env.example" "hd-compose-$version/.env.example"
   ```

   The archive holds no `.env` and no secret, so it replaces only the files it ships. `secrets/`
   holds the certificate and key of `files` as well; a file that `.env` names elsewhere in the
   old directory is to be copied too. `diff` shows the settings that came or changed, to set in
   `.env` if need be, beside the addresses of the repository's pages, which name the version.

2. Download the new version's images, while the old version still runs:

   ```sh
   cd "hd-compose-$version"
   docker compose pull
   ```

3. Stop the old version's core, take a dump, which is the one to go back to, copy it into the
   old directory, with the name the dump says it took in place of `hd-20261125T023000Z.dump`,
   and stop the old version:

   ```sh
   cd "../hd-compose-$old"
   docker compose stop core
   docker compose run --rm dumps
   docker compose run --rm -T dumps cat /dumps/hd-20261125T023000Z.dump > hd-20261125T023000Z.dump
   docker compose down
   ```

   `down` keeps the volumes: the database and the dumps. The copy outlives the dumps' volume,
   which keeps only the latest `HD_DUMPS_KEEP`.

4. Start the new version:

   ```sh
   cd "../hd-compose-$version"
   docker compose up -d
   docker compose ps
   ```

   The core applies the new version's migrations before it serves. Mail waits in the mailbox
   meanwhile, and is read when the core starts.

Keep the old directory, and the dump copied into it, until the new version has proved good. To
go back to the old version, restore that dump in the old directory rather than start the old
version on the database as the new one left it, with the new version's migrations applied:
whatever the help desk did after step 3 is lost. Stop the new version, remove the database's
volume, `hd_db-data` (named after the project, the `COMPOSE_PROJECT_NAME` of `.env` when it sets
one), restore and start, from the directory that holds both versions:

```sh
cd "hd-compose-$version"
docker compose down
cd "../hd-compose-$old"
docker volume rm hd_db-data
docker compose run --rm -T dumps restore < hd-20261125T023000Z.dump
docker compose up -d
```

The restore starts the database alone, which creates it anew with the password of
`secrets/db_password`, and ends with `restored`
([Restoring into a fresh install](../deploy/compose/dumps.md#restoring-into-a-fresh-install)).
A version that moves the install's PostgreSQL to a new major version says so in its notes, with
the dump and the restore it takes.

## Restore

A dump restores into a database before the core first starts on it, with the version of the help
desk that took the dump or a newer one.

- **Into a fresh install**, on another host or on this one: copy the dump out of the dumps'
  volume ([dumps.md](../deploy/compose/dumps.md#listing-them-and-copying-them-off-the-host)),
  then [install](#install) with the profile `db` as far as the settings in `.env`, copy the dump
  into the install's directory, and restore it with the commands of
  [Restoring into a fresh install](../deploy/compose/dumps.md#restoring-into-a-fresh-install),
  which write the database's password, restore and start. The agents sign in with the passwords
  they had. On the same host, the fresh install would find the old one's volumes, named after
  the project, with a database that the new password does not open: give it a
  `COMPOSE_PROJECT_NAME` of its own, with its own `HD_HTTP_PORT` and `HD_SUBNET`
  ([The settings](#the-settings)), or remove the old install first, once its dumps are copied
  off, with `docker compose down -v` in its directory.
- **Into the install it came from**, such as to go back to the dump of an upgrade: stop it, remove
  the database's volume, and restore the dump copied into its directory, as
  [Upgrades](#upgrades) does to go back.

## Podman

`podman compose` runs an external compose provider: Docker Compose (`docker-compose`) when it is
installed, else the Python `podman-compose`. The install needs Docker Compose 2.20 or later
(`podman compose version` prints the one it runs); `podman-compose` has not been tried. The
install, the first steps, an upgrade, a restore into a fresh install and the move to Kubernetes
have been carried out as this page gives them with Podman 5.8 on Windows, in its rootful WSL
machine, with Docker Compose 5.1.4: every command is the same with `podman compose` in place of
`docker compose`. What to know with Podman:

- **Warnings about the optional secrets.** `up` warns `secret file hd_imap_url does not exist`,
  and so for each optional secret that `.env` does not name, which is mounted from `/dev/null`
  ([Secrets of the Compose install](../deploy/compose/secrets/README.md)): nothing is missing.
- **An empty volume left by each `down`.** The image of the dumps, PostgreSQL's, declares a
  volume, so `dumps-schedule` gets an anonymous one, which it never writes, and `podman compose
  down` leaves it behind when it removes the container (`down -v` removes it with the rest;
  `run --rm dumps` removes its own). Whether Docker does the same has not been checked.
  `podman volume ls --filter dangling=true` lists it, with any other volume that no container
  uses; remove it by its name with `podman volume rm`. `podman volume prune` would remove the
  install's own volumes too while the install is down, and those of anything else on the host.
- **After a restart of the host**, Podman, which has no daemon, starts the containers again
  (`restart: unless-stopped`) only with its `podman-restart` service enabled: `systemctl enable
  podman-restart.service`, or `systemctl --user enable podman-restart.service` for rootless
  Podman.
- **Rootless Podman** opens no port below 1024: give Caddy others (`HD_CADDY_HTTP_PORT`,
  `HD_CADDY_HTTPS_PORT`), and read in
  [reverse-proxy.md](../deploy/compose/reverse-proxy.md#the-proxys-address-and-hd_subnet) what
  has not been tried with it.
- **On Windows**, `chmod 700 secrets` changes nothing on the Windows file system, and
  `db-password` warns that `secrets/` is not `0700`: keep the install in a directory that only
  you can open.

## Moving to Kubernetes

A Compose install moves to a Kubernetes install of the same version or a newer one with a dump,
restored into the database of the chart, in any of its modes
([Choosing the database mode](kubernetes.md#choosing-the-database-mode)), and with the same
secrets. The agents then sign in with the passwords they had, and everything the help desk holds
comes along: it is all in the database. In this order:

1. Prepare the Kubernetes install as [Installing on Kubernetes](kubernetes.md) says, with the
   Compose install's secrets in the Secrets of [Secrets to create](kubernetes.md#secrets-to-create).
   In the Compose install's directory, for the files that its `secrets/` holds:

   ```sh
   kubectl create namespace helpdesk
   kubectl -n helpdesk create secret generic hd-mail \
     --from-file=imap-url=secrets/imap_url --from-file=smtp-url=secrets/smtp_url
   kubectl -n helpdesk create secret generic hd-oidc \
     --from-file=client-secret=secrets/oidc_client_secret
   kubectl -n helpdesk create secret tls hd-tls \
     --cert=secrets/tls_certificate --key=secrets/tls_key
   ```

   The core removes the whitespace around a secret, such as a file's final newline, from a
   Secret as from a file. Name the Secrets in `my-values.yaml`, with the Ingress at the host of
   `HD_PUBLIC_URL` serving the certificate of `files` ([HTTPS](kubernetes.md#https) has the
   other ways):

   ```yaml
   core:
     mail:
       imap:
         existingSecret: hd-mail
       smtp:
         existingSecret: hd-mail
     oidc:
       existingSecret: hd-oidc
   ingress:
     enabled: true
     host: helpdesk.example.com
     tls:
       secretName: hd-tls
   ```

   `secrets/db_password` is the password of the Compose install's database. The chart generates
   that of its own PostgreSQL, and CloudNativePG that of its cluster; with `external`, the
   password of the database the chart is given goes in the database password Secret of
   [Secrets to create](kubernetes.md#secrets-to-create), which is that file when the Compose
   install already ran on that database. The other settings of `.env` become these values
   ([`values.yaml`](../deploy/chart/values.yaml) documents each):

   | `.env` | `my-values.yaml` |
   |---|---|
   | `HD_PUBLIC_URL` | `core.publicUrl`, or nothing when it is `https://` and `ingress.host` |
   | `HD_MAIL_FROM`, `HD_MAIL_FROM_NAME` | `core.mail.from`, `core.mail.fromName` |
   | `HD_IMAP_POLL_SECONDS`, `HD_IMAP_IMPORT_EXISTING` | `core.mail.pollSeconds`, `core.mail.importExisting` |
   | `HD_SIGN_IN_FAILURES`, `HD_SIGN_IN_WINDOW_SECONDS` | `core.signIn.failures`, `core.signIn.windowSeconds` |
   | `HD_TRUSTED_PROXIES` | `core.signIn.trustedProxies`, with the addresses of the ingress controller's pods rather than `HD_SUBNET` ([Failed sign-ins](kubernetes.md#failed-sign-ins)) |
   | `HD_OIDC_ISSUER`, `HD_OIDC_CLIENT_ID`, `HD_OIDC_LABEL`, `HD_OIDC_SESSION_HOURS`, `HD_OIDC_AUTHORIZED_GROUP`, `HD_OIDC_GROUP_CLAIM` | `core.oidc.issuer`, `clientId`, `label`, `sessionHours`, `authorizedGroup`, `groupClaim` |
   | `HD_LOG` | `core.log` |
   | `HD_DUMPS_TIME`, `HD_DUMPS_KEEP`, `HD_DUMPS_IMAGE_TAG` | `dumps.schedule`, a cron expression in the cluster's time zone (`"30 2 * * *"` for 02:30), `dumps.keep`, `dumps.image.tag` |
   | `HD_DATABASE_HOST`, `HD_DATABASE_PORT`, `HD_DATABASE_NAME`, `HD_DATABASE_USER`, `HD_DATABASE_SSLMODE` | with `external` only: `postgresql.external.host`, `port`, `name`, `user`, and `core.database.sslMode` |

   The others are the Compose install's own and have no value in the chart: `HD_VERSION`, whose
   place `--version` takes, `COMPOSE_*`,
   `HD_HTTP_ADDRESS`, `HD_HTTP_PORT`, `HD_SUBNET`, the Caddy settings (`HD_HTTPS_CERTIFICATE`,
   `HD_CADDY_*`), whose place the Ingress takes, `HD_GREENMAIL_*` (the chart's test mail server is
   `greenmail.enabled`), the `HD_SECRET_*` lines, whose files the Secrets above hold, and the
   Assistant's, which does not move (below).

2. On the Compose host, stop the core, take a dump and copy it off the volume, with the name the
   dump says it took in place of `hd-20261125T023000Z.dump`:

   ```sh
   docker compose stop core
   docker compose run --rm dumps
   docker compose run --rm -T dumps cat /dumps/hd-20261125T023000Z.dump > hd-20261125T023000Z.dump
   ```

   Anything the help desk does after this is not in the dump: keep the core stopped.

3. Install the chart with the core stopped, and wait for its database. With the chart's own
   PostgreSQL (`standalone`, the default):

   ```sh
   helm install hd oci://ghcr.io/manustars/charts/hd --version <version> \
     -n helpdesk --create-namespace -f my-values.yaml --set core.replicas=0
   kubectl -n helpdesk rollout status statefulset/hd-postgresql --timeout=300s
   ```

   With `cloudnativepg`, wait for the cluster instead
   (`kubectl -n helpdesk wait --for=condition=Ready cluster/hd-db --timeout=600s`); with
   `external`, the database is to be empty, as the DBA created it.

4. Restore the dump, from the machine that holds it, as
   [Restoring a dump from elsewhere](kubernetes.md#restoring-a-dump-from-elsewhere) says.

5. Start the core:

   ```sh
   helm upgrade hd oci://ghcr.io/manustars/charts/hd --version <version> -n helpdesk \
     -f my-values.yaml
   kubectl -n helpdesk rollout status deployment/hd-core --timeout=300s
   ```

Once the agents reach the help desk at its address in the cluster, stop the Compose install for
good with `docker compose down`, which keeps its volumes until you remove them (`docker compose
down -v`). A Compose install on an existing database needs no dump: stop it with `docker compose
down`, and install the chart with `postgresql.mode=external` on the same database
([To a database of your own](kubernetes.md#to-a-database-of-your-own) shows the values).

The Assistant does not move yet. The core keeps it registered with its address under Compose,
`http://assistant:8090`, which the cluster does not have, and has no command to change an
add-on's address: in the cluster, its events would wait undelivered.
