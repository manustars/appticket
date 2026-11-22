# Deploying to Kubernetes

The help desk ships as two container images and a Helm chart:

| | |
|---|---|
| `ghcr.io/manustars/hd-core` | The core: API, the agents' UI (served by the core itself), mail reading and sending, add-on events. Runs `hd serve`; any other `hd` command runs the same way. |
| `ghcr.io/manustars/hd-assistant` | The Assistant add-on (optional). |
| `deploy/chart` | The Helm chart `hd`: both components, an optional PostgreSQL cluster through CloudNativePG, optional backups, an optional test mail server for staging. |

The images are published by `.github/workflows/release.yml` when a version tag (`v1.2.3`) is
pushed: tagged with the version and, for a final version, `latest`. Both are built from
`deploy/Dockerfile` (`--target core` / `--target assistant`), run as the non-root user 10001 and
stop cleanly on SIGTERM.

## Prerequisites

- Kubernetes 1.27 or later, Helm 3.
- The CloudNativePG operator, unless you bring your own PostgreSQL (`postgresql.enabled=false`).
- An ingress controller and TLS for the agents' UI: the sign-in cookie is marked `Secure`, so the
  UI works only over HTTPS (or through a port-forward to `localhost`). A certificate of your own, or
  the ingress controller's default certificate (see [HTTPS](#https)).
- For backups, a StorageClass for the database dumps (see [Backups](#backups)); for CloudNativePG's
  own backups, a CSI driver with a `VolumeSnapshotClass` or S3-compatible storage with the Barman
  Cloud plugin.
- For real mail: an IMAP mailbox and an SMTP server reachable over TLS (`imaps://`, `smtps://` or
  `smtp://…?tls=required`). Plain `imap://`/`smtp://` are accepted only for a server on the core's
  own host, which is how the staging test mail server works.

## Secrets to create

The chart never holds secrets; it refers to Secrets you create in the release's namespace.

| Secret | Keys | When |
|---|---|---|
| database | `uri`: `postgres://user:password@host:5432/db` | only with `postgresql.enabled=false`; set `core.database.existingSecret` (and `secretKey`) |
| mail | `imap-url`, `smtp-url` | real mail: `core.mail.imap.existingSecret`, `core.mail.smtp.existingSecret` (may be the same Secret) |
| mail CA | `ca.crt` | mail servers with a private certificate authority: `core.mail.ca.existingSecret` |
| Assistant | `token`, `event-secret`, and `api-key` for a hosted model | `assistant.secret.existingSecret`, after registering the add-on |
| backup storage | `ACCESS_KEY_ID`, `ACCESS_SECRET_KEY` | object-store backups: `postgresql.backup.objectStore.credentialsSecret` |

For example, the mail Secret:

```sh
kubectl -n helpdesk create secret generic hd-mail \
  --from-literal=imap-url='imaps://helpdesk%40example.com:PASSWORD@imap.example.com/INBOX' \
  --from-literal=smtp-url='smtps://helpdesk%40example.com:PASSWORD@smtp.example.com'
```

Percent-encode `@`, `/` and `:` inside the user name or password.

With `postgresql.enabled=true` (the default) nothing is needed for the database: CloudNativePG
creates the Secret `<name>-db-app` (`hd-db-app` for a release named `hd`) and the core reads its
`uri`.

## Install

```sh
helm install hd deploy/chart -n helpdesk --create-namespace -f my-values.yaml
```

The chart runs the images of its `appVersion` unless `core.image.tag` and
`assistant.image.tag` say otherwise: a release must have been published (a `v…` tag pushed) for
that tag to exist. A package published to GitHub's container registry starts out private: make
it public in the package settings once (it cannot be made private again), or create a registry
Secret and list it in `imagePullSecrets`.

The examples on this page use the release name `hd` and the namespace `helpdesk`: the chart
names its objects after the release (`hd-core`, `hd-assistant`, `hd-db`), and a release whose name
does not contain `hd` gets `<release>-hd-…` instead.

`deploy/chart/values.yaml` documents every value; `deploy/chart/values-staging.example.yaml` is a
small staging install with the test mail server. The most common settings:

| Value | Meaning |
|---|---|
| `core.image.tag` | The version to run (default: the chart's `appVersion`). |
| `core.mail.from`, `core.mail.fromName` | The help desk's own address and display name, for replies. |
| `core.mail.imap/smtp.existingSecret` | The mail Secrets above. |
| `ingress.*` | Host, class, annotations and TLS (see [HTTPS](#https)). |
| `dumps.*` | Scheduled database dumps to a volume (see [Backups](#backups)). |
| `postgresql.*` | Instances, storage size and class, backups. |
| `assistant.*` | See [The Assistant](#the-assistant). |
| `greenmail.enabled` | The staging test mail server. |
| `networkPolicy.enabled` | Ingress NetworkPolicies; list the ingress controller's namespace in `networkPolicy.coreFrom`. |

On a first install the core waits and restarts until CloudNativePG has created the database and
its Secret, usually within a minute or two. The core applies database migrations when it starts. It runs as one replica and is replaced, not
rolled, on upgrade: it reads the mailbox and sends the outbox, and two cores at once would do both
twice. Its readiness follows `/api/health` (unready while the database is unreachable); liveness
only checks that the port answers.

### HTTPS

The Ingress always lists its host for TLS when `ingress.tls.enabled` is true (the default):

- with `ingress.tls.secretName` empty, the ingress controller serves its default certificate,
  which suits a staging install where agents accept that certificate;
- with a TLS Secret named there, or one created by cert-manager through an annotation in
  `ingress.annotations`, the controller serves that certificate.

To send plain HTTP to HTTPS with ingress-nginx, add the annotation
`nginx.ingress.kubernetes.io/force-ssl-redirect: "true"`.

## First steps

The `hd` command runs inside the core's pod, with the pod's database settings:

```sh
# The help desk's own organization, with its mail domain.
kubectl -n helpdesk exec deploy/hd-core -- hd customer add "Northgate Studio" --help-desk --domain example.com

# The first agent, who gets the Administrator role; a password is generated and printed once.
kubectl -n helpdesk exec deploy/hd-core -- hd agent add agent@example.com --name "Ada Agent"
```

To choose the password yourself instead, pass it through the environment of the command:
`kubectl -n helpdesk exec deploy/hd-core -- env HD_AGENT_PASSWORD='…' hd agent add …` (at least 12
characters). Customers,
their domains, assets and services can then be managed in the UI, and so can agents and roles,
under Administration.

### Agents and roles

Every agent has one role ([ADR 0007](adr/0007-roles-and-permissions.md)). The first agent gets
the built-in Administrator role, which allows everything; later agents get the default role
(`Agent`, which works tickets and sorts the inbox) unless `--role` names another. Roles and
agents are managed in the UI by agents whose role allows it, or with the `hd` command:

```sh
hd role permissions                      # the permissions a role can hold
hd role list
hd role add "Front desk" --permission inbox.triage --permission tickets.work
hd role default "Front desk"             # the role new agents get
hd agent add bo@example.com --name "Bo" --role "Front desk"
hd agent set-role bo@example.com Administrator
hd agent set-password bo@example.com     # generated and printed, or from HD_AGENT_PASSWORD
hd agent deactivate bo@example.com       # cannot sign in; their open tickets are unassigned
hd agent reactivate bo@example.com
```

The core always keeps one active agent with the Administrator role. If nobody who can still sign
in has it, `hd agent set-role <e-mail> Administrator` gives it back; the command line is not
limited by roles. Upgrading from a version without roles makes every existing agent an
Administrator.

## The Assistant

The Assistant needs credentials that only exist once it is registered on the core, so the order
is: install (with `assistant.enabled=false`), register, create the Secret, enable.

```sh
kubectl -n helpdesk exec deploy/hd-core -- hd addon add assistant \
  --url http://hd-assistant:8090 \
  --event message.received --event ticket.opened \
  --scope directory:read --scope tickets:read --scope proposals:write
```

The command prints a token and an event secret once. Put them in a Secret, with the model
service's key when it needs one:

```sh
kubectl -n helpdesk create secret generic hd-assistant \
  --from-literal=token='…' --from-literal=event-secret='…' --from-literal=api-key='…'
```

Then set the values and upgrade:

```yaml
assistant:
  enabled: true
  secret:
    existingSecret: hd-assistant
  model:
    provider: anthropic          # or openai, mistral, gemini, azure-openai, ollama, openai-compatible
    name: claude-opus-5-5        # the provider's model id
```

There is no default provider or model. The Assistant logs a warning at startup when customers'
mail goes to a model service outside its pod's host; with a self-hosted model use
`openai-compatible` (or `ollama`) and `model.url`. Self-hosted servers get one model call at a
time by default (`model.concurrency`). Its README (`addons/assistant/README.md`) describes every
setting.

## Staging with the test mail server

With `greenmail.enabled=true`, a GreenMail test server runs next to the core in the same pod, and
the core reads and sends through it unless mail Secrets are set. It accepts any mail and keeps it
in one mailbox (`helpdesk@helpdesk.test` by default); nothing leaves it. To send a test mail:

```sh
kubectl -n helpdesk port-forward svc/hd-greenmail 3025:3025
curl smtp://127.0.0.1:3025 --mail-from ada@customer.example.com \
  --mail-rcpt helpdesk@helpdesk.test -T message.eml
```

A sender whose domain belongs to a customer opens a ticket; any other sender lands in the inbox.

GreenMail keeps mail in memory only: a restart of the core's pod empties the test mailbox, and the
core's next read starts over with the mail that arrives afterwards. It is for trying the help desk
out, not for keeping anything.

## Backups

Two independent kinds, both off by default.

### Database dumps

With `dumps.enabled=true`, a CronJob runs `pg_dump` on `dumps.schedule` (five-field cron, the
cluster's time zone; default every day at 02:30). It writes a compressed dump in PostgreSQL's custom
format to a volume, and keeps the latest `dumps.keep` (default 7). It needs no object storage and
works with the CloudNativePG cluster or an external database; the volume (`dumps.storage.size`,
`storageClass`, or `existingClaim`) is kept on `helm uninstall`. Keep the job's image
(`dumps.image.tag`) at the database's major version or newer.

To take one now and to list the dumps:

```sh
kubectl -n helpdesk create job --from=cronjob/hd-dumps hd-dump-now
kubectl -n helpdesk logs job/hd-dump-now
```

To restore, stop the core, restore the chosen dump into the database with `pg_restore` from a pod
that mounts the dumps volume (for example `pg_restore --clean --if-exists --no-owner
--dbname="$DATABASE_URL" /dumps/hd-<date>.dump`), then start the core again:

```sh
kubectl -n helpdesk scale deploy/hd-core --replicas=0
# … run pg_restore …
kubectl -n helpdesk scale deploy/hd-core --replicas=1
```

A dump is as old as its schedule: anything after the last dump is lost on restore. Copy the
dumps off the cluster too if the cluster's storage is the only place they live.

### CloudNativePG backups

For clusters with object storage or volume snapshots, CloudNativePG's own backups give
point-in-time recovery. Turn them on with `postgresql.backup.enabled=true` and one method:

- `method: volumeSnapshot` — CSI volume snapshots of the database volume, on the schedule in
  `postgresql.backup.schedule`. Set `volumeSnapshot.className` to your `VolumeSnapshotClass`
  (required). Snapshots stay in the cluster's storage: good for quick restores, not a copy
  elsewhere.
- `method: objectStore` — base backups and continuous WAL archiving to S3-compatible storage
  through the Barman Cloud plugin, which must be installed next to the CloudNativePG operator (the
  operator's own `barmanObjectStore` support is deprecated and removed in its 1.32). Set
  `objectStore.destinationPath` (`s3://bucket/path/`), `endpointURL` for services other than AWS,
  `credentialsSecret` and `retentionPolicy` (for example `30d`).

The schedule has six fields, seconds first: `"0 0 2 * * *"` is every day at 02:00. An on-demand
backup can be taken with a CloudNativePG `Backup` resource for the cluster `<release>-db`.
Restoring means creating a new cluster that bootstraps from the backup (CloudNativePG's
`bootstrap.recovery`), then pointing the core at it with `core.database.existingSecret`.

## Uninstalling

`helm uninstall` removes the core, the Assistant and the rest, but leaves the database cluster
(`postgresql.keepOnUninstall`, true by default) so that its data survives; installing again with
the same release name takes it back. To delete the data too, delete the cluster by hand
(`kubectl -n helpdesk delete cluster hd-db`). Turning `postgresql.enabled` off in an upgrade also
leaves the kept cluster in place.

## Upgrades

```sh
helm upgrade hd deploy/chart -n helpdesk -f my-values.yaml --set core.image.tag=0.2.0
```

The core is stopped, then started with the new version, which applies its migrations before
serving: expect a short pause for agents and mail (mail waits in the mailbox and is read on
start). Take a backup before upgrading across versions with migrations. The Assistant can be
upgraded on its own; events wait in the core while it restarts.
