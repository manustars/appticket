# Installing on Kubernetes

The help desk installs on Kubernetes with its Helm chart, `oci://ghcr.io/manustars/charts/hd`.
[Deploying the help desk](deploy.md) describes the images and the chart, and what to do once the
help desk runs: the first agent, roles, signing in through an identity provider, importing
customers, the Assistant. What is the same for every install, a PostgreSQL database of your own,
TLS to it and secrets read from files, is in
[Running on a PostgreSQL database of your own](external-database.md). A help desk installed with
Compose moves here with a dump ([Moving to Kubernetes](compose.md#moving-to-kubernetes)).

The examples on this page use the release name `hd` and the namespace `helpdesk`: the chart
names its objects after the release (`hd-core`, `hd-assistant`, `hd-db`, `hd-postgresql`), and a
release whose name does not contain `hd` gets `<release>-hd-…` instead.

## Prerequisites

- Kubernetes 1.27 or later, and Helm 3.8 or later, which reads the chart from the registry.
  CI installs and upgrades the chart with Helm 4.
- A StorageClass for the database's volume, when the chart runs PostgreSQL itself
  (`postgresql.mode=standalone`, the default; see
  [The chart's own PostgreSQL](#the-charts-own-postgresql)): nothing else is needed for it.
- The CloudNativePG operator, only for `postgresql.mode=cloudnativepg`.
- For your own PostgreSQL, a database prepared as
  [Running on a PostgreSQL database of your own](external-database.md#what-the-dba-creates) says:
  PostgreSQL 16 or later, a database dedicated to the help desk and owned by the core's user.
- An ingress controller and TLS for the agents' UI: the sign-in cookie is marked `Secure`, so the
  UI works only over HTTPS (or through a port-forward to `localhost`). A certificate of your own, or
  the ingress controller's default certificate (see [HTTPS](#https)).
- For backups, a StorageClass for the database dumps (see [Backups](#backups)); for CloudNativePG's
  own backups, a CSI driver with a `VolumeSnapshotClass` or S3-compatible storage with the Barman
  Cloud plugin.
- For real mail: an IMAP mailbox and an SMTP server reachable over TLS (`imaps://`, `smtps://` or
  `smtp://…?tls=required`). Plain `imap://`/`smtp://` are accepted only for a server on the core's
  own host, which is how the staging test mail server works.

## Choosing the database mode

`postgresql.mode` says where the database comes from
([ADR 0009](adr/0009-two-ways-to-install-three-ways-to-get-the-database.md)):

| Mode | The database | Choose it when |
|---|---|---|
| `standalone`, the default | PostgreSQL 18 that the chart runs itself from the official image: one instance on a volume that outlives the release, its password generated at install, daily [database dumps](#database-dumps) on by default ([The chart's own PostgreSQL](#the-charts-own-postgresql)). | Nothing else is to run PostgreSQL for the help desk, and a restore to the last dump, a short stop when the database restarts and a dump and a restore for a [new major version](#a-new-major-version-of-postgresql) are acceptable. It needs only a StorageClass. |
| `cloudnativepg` | A cluster that the CloudNativePG operator runs for the chart, `hd-db`. | The cluster already runs the operator, or the help desk needs what it gives: more than one instance with failover (`postgresql.instances`), and point-in-time recovery from [its own backups](#cloudnativepg-backups). The operator needs rights over the whole cluster to install. |
| `external` | A PostgreSQL 16 or later database of your own, dedicated to the help desk, given as a URL or as separate values ([Running on a PostgreSQL database of your own](external-database.md)). | Your organization runs PostgreSQL already, such as a managed service or a server its DBAs back up, and the help desk is to be one more database there. |

The mode is chosen at install, and the chart keeps it: it refuses an upgrade to another mode for
as long as it finds the database of the one before ([The upgrade guard](#the-upgrade-guard)).
Changing it later moves the data with a dump and a restore
([Moving to another database mode](#moving-to-another-database-mode)).

## Secrets to create

The chart never holds secrets; it refers to Secrets you create in the release's namespace. The
one Secret it makes itself is the generated password of its own PostgreSQL
(`postgresql.mode=standalone`).

| Secret | Keys | When |
|---|---|---|
| database | `uri`: `postgres://user:password@host:5432/db` | only with `postgresql.mode=external`, for a database given as a URL; set `core.database.existingSecret` (and `secretKey`) |
| database password | `password` | only with `postgresql.mode=external`, for a database given as separate values; set `postgresql.external.password.existingSecret` (and `secretKey`) |
| standalone password | `password` | only with `postgresql.mode=standalone` under a tool that renders the chart without reading the cluster, such as Argo CD; set `postgresql.standalone.existingSecret` (and `secretKey`; see [GitOps tools](#gitops-tools-argo-cd)); otherwise the chart generates it (see [The chart's own PostgreSQL](#the-charts-own-postgresql)) |
| mail | `imap-url`, `smtp-url` | real mail: `core.mail.imap.existingSecret`, `core.mail.smtp.existingSecret` (may be the same Secret) |
| mail CA | `ca.crt` | mail servers with a private certificate authority: `core.mail.ca.existingSecret` |
| database CA | `ca.crt` | an external database whose certificate comes from a private authority: `core.database.ca.existingSecret` (see [TLS to the database](#tls-to-the-database)) |
| identity provider | `client-secret` | signing in through OpenID Connect: `core.oidc.existingSecret` (see [Signing in through an identity provider](deploy.md#signing-in-through-an-identity-provider)) |
| Assistant | `token`, `event-secret`, and `api-key` for a hosted model | `assistant.secret.existingSecret`, after registering the add-on |
| Zabbix add-on | `token`, `event-secret`, `api-token` (its Zabbix user's) and `webhook-secret` | `zabbix.secret.existingSecret`, after registering the add-on |
| backup storage | `ACCESS_KEY_ID`, `ACCESS_SECRET_KEY` | object-store backups: `postgresql.backup.objectStore.credentialsSecret` |

For example, the mail Secret:

```sh
kubectl -n helpdesk create secret generic hd-mail \
  --from-literal=imap-url='imaps://helpdesk%40example.com:PASSWORD@imap.example.com/INBOX' \
  --from-literal=smtp-url='smtps://helpdesk%40example.com:PASSWORD@smtp.example.com'
```

Percent-encode `@`, `/` and `:` inside the user name or password.

With `postgresql.mode=cloudnativepg` nothing is needed for the database:
CloudNativePG creates the Secret `<name>-db-app` (`hd-db-app` for a release named `hd`) and the
core reads its `uri`, and verifies the cluster's certificate against the cluster's own authority
(see [TLS to the database](#tls-to-the-database)).

With `postgresql.mode=external` the chart takes the database in one of two forms, not both, as
[Connecting](external-database.md#connecting) describes them: the `uri` of the database Secret,
a connection string, or the separate parameters, from `postgresql.external.host`, `port`
(default 5432), `name` and `user`, with the password as it is in its Secret. Outside the chart,
every secret of the core and of the Assistant can also be read from a file instead of a variable
([Secrets from files](external-database.md#secrets-from-files)).

With `postgresql.mode=standalone` (the default) nothing is needed either, unless a tool renders
the chart without reading the cluster ([GitOps tools](#gitops-tools-argo-cd)): the chart generates
the password into the Secret `<name>-postgresql`
([The chart's own PostgreSQL](#the-charts-own-postgresql)).

The chart stops with a message for:

- a `postgresql.mode` that is not the mode of the database the release already has
  ([The upgrade guard](#the-upgrade-guard));
- `postgresql.enabled`, which `postgresql.mode` replaces: `false` was `external`, and so was `true`
  beside `core.database.existingSecret`; `true` alone was `cloudnativepg`;
- with `cloudnativepg` and with `standalone`, what names an external database:
  `core.database.existingSecret`, `sslMode` and `ca.existingSecret`, and
  `postgresql.external.host`, `name`, `user` and `password.existingSecret`;
- with `standalone`, a `postgresql.standalone.image.tag` of another major version than 18, or
  one that is a number rather than a string (quote it in a values file, or use `--set-string`:
  YAML reads `18.10` as 18.1); and, on an upgrade, a `postgresql.standalone.secretKey` that the
  Secret with the generated password does not have, since the chart would generate another
  password under it;
- with `external`, both forms together, the separate values in part, and neither form;
- a `dumps.enabled` that is not `true`, `false` or empty;
- with the dumps on, a `dumps.image.tag` that is a number, as for the standalone tag.

It ignores, without a message: the values of the modes not chosen (with `external` and
`standalone` the cluster's `postgresql.instances`, `storage`, `backup` and the others; without
`standalone`, `postgresql.standalone`); beside a URL Secret and without `external`,
`postgresql.external.port` and `password.secretKey`; with the separate values,
`core.database.secretKey`.

## Install

```sh
helm install hd oci://ghcr.io/manustars/charts/hd --version <version> \
  -n helpdesk --create-namespace -f my-values.yaml
```

`<version>` is a release of the help desk, without the `v` of its tag. Helm 3.8 or later reads
the chart from the registry, so that no clone of the repository is needed. The chart is published
from 0.4.0 on: for 0.3.1 and earlier, or to try a change to the chart, install from a clone of
the repository instead, with `deploy/chart` in place of the `oci://…` address and without
`--version`.

The chart runs the images of its `appVersion`, which for a published chart is its version, unless
`core.image.tag`, `assistant.image.tag` and `zabbix.image.tag` say otherwise; from a clone, a release must have been
published (a `v…` tag pushed) for that tag to exist. A package published to GitHub's container
registry, an image or the chart, starts out private: make it public in the package settings once
(it cannot be made private again), or sign in with `helm registry login ghcr.io` for the chart,
and for the images create a registry Secret and list it in `imagePullSecrets`.

`deploy/chart/values.yaml` documents every value (`helm show values
oci://ghcr.io/manustars/charts/hd --version <version>` prints it);
`deploy/chart/values-staging.example.yaml` is a small staging install with the test mail server.
The most common settings:

| Value | Meaning |
|---|---|
| `core.image.tag` | The version to run (default: the chart's `appVersion`). |
| `core.mail.from`, `core.mail.fromName` | The help desk's own address and display name, for replies. |
| `core.publicUrl`, `core.oidc.*` | Signing in through an OpenID Connect identity provider (see [Signing in through an identity provider](deploy.md#signing-in-through-an-identity-provider)). |
| `core.mail.imap/smtp.existingSecret` | The mail Secrets above. |
| `ingress.*` | Host, class, annotations and TLS (see [HTTPS](#https)). |
| `core.signIn.*` | Failed sign-in limits and trusted proxies (see [Failed sign-ins](#failed-sign-ins)). |
| `dumps.*` | Scheduled database dumps to a volume (see [Backups](#backups)). |
| `postgresql.mode` | Where the database comes from: `standalone` (the default), `cloudnativepg` or `external`. The chart refuses a mode that is not the one of the database it finds (see [The upgrade guard](#the-upgrade-guard)). |
| `postgresql.standalone.*` | The chart's own PostgreSQL: storage size and class, its password's Secret (see [The chart's own PostgreSQL](#the-charts-own-postgresql)). |
| `postgresql.external.*` | An external database as separate values. |
| `postgresql.*` | The CloudNativePG cluster's instances, storage size and class, backups. |
| `assistant.*` | See [The Assistant](deploy.md#the-assistant). |
| `zabbix.*` | See [The Zabbix add-on](deploy.md#the-zabbix-add-on). |
| `greenmail.enabled` | The staging test mail server. |
| `networkPolicy.enabled` | Ingress NetworkPolicies; list the ingress controller's namespace in `networkPolicy.coreFrom`. With `standalone`, only the core and the dumps reach the database. The Zabbix add-on's limits its egress too (`networkPolicy.zabbix.*`, see [The Zabbix add-on](deploy.md#the-zabbix-add-on)). |

On a first install the core waits and restarts until the database is there (until CloudNativePG
has created it and its Secret, or the chart's own PostgreSQL has started), usually within a
minute or two. The core applies database migrations when it starts. It runs as one replica and is replaced, not
rolled, on upgrade: it reads the mailbox and sends the outbox, and two cores at once would do both
twice. Its readiness follows `/api/health` (unready while the database is unreachable); liveness
only checks that the port answers.

### The chart's own PostgreSQL

With `postgresql.mode=standalone`, the default, the chart runs PostgreSQL itself, with no
operator: a StatefulSet of the official `postgres` image, one instance, with the database `hd`
owned by the user `hd`.

- **Version.** The chart fixes the major version, 18, and refuses a
  `postgresql.standalone.image.tag` of another one; the minor version follows the chart, so that
  upgrading the chart brings PostgreSQL's fixes. A new major version comes with a dump and a
  restore ([A new major version of PostgreSQL](#a-new-major-version-of-postgresql)).
- **Volume.** `postgresql.standalone.storage.size` (5Gi) and `storageClass` describe the volume
  when it is first created; Kubernetes refuses an upgrade that changes them. The chart never deletes the
  volume: `helm uninstall` leaves its claim, `data-hd-postgresql-0`, in place.
- **Password.** On the first install the chart generates the password of the user `hd` into the
  Secret `hd-postgresql`; upgrades find that Secret and keep what it holds, and `helm uninstall`
  leaves it beside the volume. PostgreSQL takes the password when it first creates its data
  directory: changing the Secret afterwards does not change the password in the database.
- **GitOps tools.** A tool that renders the chart without reading the cluster, such as Argo CD,
  cannot find the Secret the chart made, and would generate another password at every sync: name
  a Secret of your own in `postgresql.standalone.existingSecret` there
  ([GitOps tools](#gitops-tools-argo-cd)).
- **No TLS inside the cluster.** The core reaches the database by its Service without TLS, and
  the chart sets that explicitly (`HD_DATABASE_SSLMODE=disable`), so that the core's log names
  the choice in a warning at every start. `core.database.sslMode` and `core.database.ca` are for
  an external database and refused here. With `networkPolicy.enabled`, only the core and the
  dumps reach the database's port.
- **Dumps.** The [database dumps](#database-dumps) are on unless `dumps.enabled=false`: the
  chart's own PostgreSQL has no other backup. The database's pod mounts the dumps' claim too,
  unused, so that the claim is bound when the database starts and `helm install --wait` ends
  even with a StorageClass that binds a volume only when a pod first uses it
  (`volumeBindingMode: WaitForFirstConsumer`). The claim is ReadWriteOnce, used on one node at
  a time, so the dumps run on the database's node, with its tolerations, and its group on the
  volume. Turning the dumps on or off restarts the database. A claim of your own
  (`dumps.storage.existingClaim`) is left to the dumps alone.

### The upgrade guard

The chart refuses an install or an upgrade whose `postgresql.mode` is not the mode of the
database it finds in the release's namespace, so that the core never starts on another, empty
database while the help desk's data sits in the one it left. That holds for a mode set by
mistake and for one that comes from the chart's defaults, which changed in 0.4.0
([Upgrading from 0.3](#upgrading-from-03-standalone-is-the-default-database-mode)). Whatever
its name, it runs on `helm install` as on `helm upgrade`: `helm uninstall` leaves a CloudNativePG
cluster in place, and an install with other values beside it would start the core on an empty
database just as an upgrade would. It finds:

- **the CloudNativePG cluster** of the release (`hd-db`), and then refuses every mode but
  `cloudnativepg`. `helm uninstall` leaves that cluster in place, so an install under the same
  release name finds it as an upgrade does;
- **the chart's own PostgreSQL**, by its StatefulSet (`hd-postgresql`), and then refuses every
  mode but `standalone`. The volume and the password's Secret that `helm uninstall` leaves do
  not count: installing again in another mode is not refused, and leaves them where they are.

The message names what it found, the mode asked for and the mode to set:

```
the upgrade guard found the CloudNativePG cluster hd-db of this release in the namespace
helpdesk, and postgresql.mode is standalone: the core would leave the database it has for
another one, the chart's own PostgreSQL, which starts empty. Set postgresql.mode=cloudnativepg
to stay on the cluster: …
```

Nothing has changed when it appears: the release, the database and the core are as they were.
Upgrade again with the mode it names.

The guard finds the cluster by its name, and cannot tell one the core runs on from one that
was left behind: a release that moved to a database of its own under 0.3 still has the cluster
the chart had made, and so has one that set `postgresql.enabled: true` beside
`core.database.existingSecret`. There the mode in the message is not the one to set: the
leftover cluster is to be deleted, as
[Moving to another database mode](#moving-to-another-database-mode) says.

To leave a database for good, the chart has to stop finding it: the chart refuses every other
mode for as long as that object exists.
[Moving to another database mode](#moving-to-another-database-mode) gives the order: a dump, the
object deleted, the upgrade, the restore.

If it finds both, it cannot tell which one holds the help desk's data, and refuses every mode
until the one the core does not run on is deleted.

**What the guard needs.** Helm must read the cluster while it renders the chart, as `helm
install` and `helm upgrade` do, with the right to read StatefulSets in the release's namespace
and, where CloudNativePG is installed, its clusters there. Where Helm does not read the cluster
the guard finds nothing and refuses nothing:

- **GitOps tools** that render the chart themselves, such as Argo CD and anything else built on
  `helm template`: set the mode there yourself ([GitOps tools](#gitops-tools-argo-cd)).
- **`helm template`**, and **`helm upgrade --dry-run`** unless it is `--dry-run=server`, which
  reads the cluster and shows the guard's answer without changing anything. `--dry-run=server`
  needs Helm 3.13 or later: earlier versions have only the dry run that does not read the
  cluster.

### HTTPS

The Ingress always lists its host for TLS when `ingress.tls.enabled` is true (the default):

- with `ingress.tls.secretName` empty, the ingress controller serves its default certificate,
  which suits a staging install where agents accept that certificate;
- with a TLS Secret named there, or one created by cert-manager through an annotation in
  `ingress.annotations`, the controller serves that certificate.

To send plain HTTP to HTTPS with ingress-nginx, add the annotation
`nginx.ingress.kubernetes.io/force-ssl-redirect: "true"`. What the controller must pass on, and
the size of the requests it must accept, are in
[Behind a reverse proxy](deploy.md#behind-a-reverse-proxy).

### TLS to the database

The core reaches a database on another host over TLS and verifies its certificate
(`verify-full`) unless told otherwise. [TLS](external-database.md#tls) in the guide to running on
your own database gives the modes, the defaults, the warning a weaker mode logs and what stops the
core. The chart sets its two variables from two values, which are for an external database
(`postgresql.mode=external`, in either form) only and refused with the CloudNativePG cluster and
with the chart's own PostgreSQL, which is reached without TLS
([The chart's own PostgreSQL](#the-charts-own-postgresql)):

- `core.database.sslMode`: `HD_DATABASE_SSLMODE`, for the separate values and for a URL without
  its own `sslmode`;
- `core.database.ca.existingSecret` and `secretKey`: a Secret holding the PEM file of a private
  authority, mounted as `HD_DATABASE_CA_FILE`.

With the CloudNativePG cluster the chart does it: `verify-full`, with the authority's certificate
mounted from the Secret `<name>-db-ca` that CloudNativePG creates for the cluster (only `ca.crt`:
the Secret also holds the authority's key). The host in the cluster's `uri`,
`<name>-db-rw.<namespace>`, is one of the names the cluster's certificate lists.

### Failed sign-ins

Repeated failed sign-ins are slowed, then refused for a while, per account and per address, so
that guessing at the sign-in page cannot lock a person out of their organization's
directory once agents sign in through it. After `core.signIn.failures` failures
(`HD_SIGN_IN_FAILURES`, default 5) within `core.signIn.windowSeconds`
(`HD_SIGN_IN_WINDOW_SECONDS`, default 900, that is 15 minutes), the next four attempts wait 1, 2,
4 and 8 seconds before the password is checked, and attempts after those are refused without
checking it until a window has passed since the last failure. The sign-in page then says to try
again later. At most `failures` + 4 passwords are checked per account within a window: keep that
below the directory's own lockout threshold.

A sign-in counts under the address of the connection it comes on. Behind the chart's Ingress
every connection comes from the ingress controller, which passes the browser's address on in
`X-Forwarded-For`; the core believes that header only from the proxies listed in
`core.signIn.trustedProxies` (`HD_TRUSTED_PROXIES`, addresses or networks separated by commas).
With the list empty, the header is ignored and every agent is counted under the controller's
address, so that a handful of failures by anyone (nine with the defaults) refuse everyone for a
while. List the addresses the ingress controller's pods connect from, usually the cluster's pod
network:

```yaml
core:
  signIn:
    trustedProxies:
      - 10.0.0.0/8          # your cluster's pod network
```

How the core reads the header, a load balancer in front of the controller included, and what
trusting a proxy allows it: [Trusted proxies](deploy.md#trusted-proxies).

## GitOps tools (Argo CD)

Argo CD, and anything else that renders the chart with `helm template` and applies what it
rendered, never lets the chart read the cluster. Two things the chart does by reading it then do
not happen:

- **The upgrade guard does not run** ([The upgrade guard](#the-upgrade-guard)): a mode that is
  not the one of the database, set by mistake or come from a new default of the chart, would
  start the core on another, empty database. Set `postgresql.mode` in the values, so that no new
  default ever changes it; to change it, follow
  [Moving to another database mode](#moving-to-another-database-mode) yourself, since nothing
  stops a step left out.
- **The generated password is not found**
  ([The chart's own PostgreSQL](#the-charts-own-postgresql)): with `standalone` the chart would
  generate another password at every sync, which the database does not accept. Create a Secret
  of your own before the first sync and name it in `postgresql.standalone.existingSecret` (its
  key in `secretKey`, `password` by default); the chart then generates nothing. For a release
  that Helm installed before, put in it the password of the Secret `hd-postgresql`: the database
  keeps the password it was created with.

  ```sh
  kubectl -n helpdesk create secret generic hd-postgresql-password \
    --from-literal=password='PASSWORD'
  ```

The values for such a tool, with the chart's own PostgreSQL:

```yaml
postgresql:
  mode: standalone
  standalone:
    existingSecret: hd-postgresql-password
```

With `cloudnativepg` and `external` the mode is all there is to set. A release that moves to 0.4.0
under such a tool sets the mode **before** it moves
([Upgrading from 0.3](#upgrading-from-03-standalone-is-the-default-database-mode)).

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

Two independent kinds: database dumps, in every database mode, and CloudNativePG's own backups,
off by default.

### Database dumps

The dumps are on by default with the chart's own PostgreSQL (`postgresql.mode=standalone`),
which has no other backup, and off by default in the other modes: `dumps.enabled`, left empty,
means that, and `true` or `false` say it explicitly.

When they are on, a CronJob runs `pg_dump` on `dumps.schedule` (five-field cron, the
cluster's time zone; default every day at 02:30). It writes a compressed dump in PostgreSQL's custom
format to a volume, and keeps the latest `dumps.keep` (default 7). It needs no object storage and
works in every database mode; the volume (`dumps.storage.size`,
`storageClass`, or `existingClaim`) is kept on `helm uninstall`. Keep the job's image
(`dumps.image.tag`) at the database's major version or newer. A dump that has not ended after
`dumps.activeDeadlineSeconds` (6 hours), waiting for its pod included, fails, so that the next one
runs.

With `cloudnativepg` or `external`, the claim the chart makes for the dumps is used only by the
dumps. Helm's `--wait` (and `--atomic`, which implies it) waits for every PersistentVolumeClaim of
the release to be bound: with a StorageClass that binds a volume only when a pod first uses it
(`volumeBindingMode: WaitForFirstConsumer`), the claim stays pending until the first dump runs,
and `--wait` runs into its timeout though the help desk is up. Install without `--wait` and follow
the core with `kubectl -n helpdesk rollout status deployment/hd-core`, or give the dumps a claim
that is already bound (`dumps.storage.existingClaim`). The chart's own PostgreSQL mounts the claim
itself ([The chart's own PostgreSQL](#the-charts-own-postgresql)).

`pg_dump` reaches the chart's own PostgreSQL as the core does: inside the cluster, without TLS,
with the password from its Secret.

`pg_dump` reaches an external database, in either form, over TLS, but the job's image holds no
public certificate authorities: it verifies the server's certificate only against
`core.database.ca`.

- With `core.database.ca`, it uses `core.database.sslMode`, `verify-full` when empty, against
  that authority, mounted as for the core.
- Without it, TLS is required but the certificate is not verified (`sslmode=require`) where the
  core verifies it (`sslMode` empty, `verify-full` or `verify-ca`); a weaker `sslMode` is used as
  it is. To have `pg_dump` verify a certificate from a public authority, give that authority in
  `core.database.ca`: the core trusts it besides the public ones.
- A URL that carries its own `sslmode` keeps it, and with `verify-full` or `verify-ca` there
  `pg_dump` fails without `core.database.ca`.

The dumps of the CloudNativePG cluster use the cluster's URL as it is.

To take one now and to list the dumps:

```sh
kubectl -n helpdesk create job --from=cronjob/hd-dumps hd-dump-now
kubectl -n helpdesk wait --for=condition=complete job/hd-dump-now --timeout=600s
kubectl -n helpdesk logs job/hd-dump-now
```

Each dump is named after the time it was taken, in UTC, as the job's log says
(`dumped hd-20261125T023000Z.dump; kept:`, then the dumps on the volume).

#### Restoring a dump

A Job that mounts the dumps' volume restores a dump with `pg_restore`, while the core is stopped.
For the chart's own PostgreSQL, save this as `hd-restore.yaml`, with the dump's name in `DUMP`:

```yaml
apiVersion: batch/v1
kind: Job
metadata:
  name: hd-restore
spec:
  backoffLimit: 0
  template:
    metadata:
      labels:
        # The labels of the dumps: with networkPolicy.enabled, only the core and the dumps
        # reach the chart's own PostgreSQL.
        app.kubernetes.io/name: hd
        app.kubernetes.io/instance: hd
        app.kubernetes.io/component: dumps
    spec:
      restartPolicy: Never
      # On the node of the chart's own PostgreSQL, which mounts the dumps' claim too: a
      # ReadWriteOnce volume is used on one node at a time.
      affinity:
        podAffinity:
          requiredDuringSchedulingIgnoredDuringExecution:
            - topologyKey: kubernetes.io/hostname
              labelSelector:
                matchLabels:
                  app.kubernetes.io/name: hd
                  app.kubernetes.io/instance: hd
                  app.kubernetes.io/component: postgresql
      securityContext:
        runAsNonRoot: true
        runAsUser: 10001
        runAsGroup: 10001
        seccompProfile:
          type: RuntimeDefault
      containers:
        - name: pg-restore
          # The image of the dumps (dumps.image), at the database's major version or newer.
          image: docker.io/library/postgres:18.7-trixie
          env:
            - name: DUMP
              value: hd-20261125T023000Z.dump
            - name: PGHOST
              value: hd-postgresql
            - name: PGDATABASE
              value: hd
            - name: PGUSER
              value: hd
            - name: PGPASSWORD
              valueFrom:
                secretKeyRef:
                  name: hd-postgresql
                  key: password
            - name: PGSSLMODE
              value: disable
          command: ["/bin/sh", "-c"]
          args:
            - >-
              pg_restore --clean --if-exists --no-owner --no-privileges --single-transaction
              --dbname="$PGDATABASE" "/dumps/$DUMP" && echo "restored $DUMP"
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            capabilities:
              drop: ["ALL"]
          volumeMounts:
            - name: dumps
              mountPath: /dumps
              readOnly: true
      volumes:
        - name: dumps
          persistentVolumeClaim:
            claimName: hd-dumps
```

```sh
kubectl -n helpdesk scale deploy/hd-core --replicas=0
kubectl -n helpdesk apply -f hd-restore.yaml
kubectl -n helpdesk wait --for=condition=complete job/hd-restore --timeout=600s
kubectl -n helpdesk logs job/hd-restore
kubectl -n helpdesk delete job hd-restore
kubectl -n helpdesk scale deploy/hd-core --replicas=1
```

The log ends with `restored hd-….dump`. The restore replaces what the dump holds, in one
transaction: one that fails changes nothing, the Job fails without trying again, `kubectl wait`
runs into its timeout, and the log says why. The objects restored belong to the user that
restores them (`--no-owner --no-privileges`), so that a dump goes into a database whose user has
another name. `pg_restore`, and the server it restores into, are to be of the dump's major
version or newer: the image is the one of `dumps.image.tag`.

With `networkPolicy.enabled`, only the pods of the core and of the dumps reach the chart's own
PostgreSQL, which is why the Job's pod has the labels of the dumps (with another release name,
`app.kubernetes.io/instance` is that name, and with `nameOverride`, `app.kubernetes.io/name` is
that, in the affinity too). The affinity is for the chart's own PostgreSQL, which mounts the dumps'
claim: leave it out with a claim of your own (`dumps.storage.existingClaim`) and where that
PostgreSQL is not running, or the Job's pod waits for it; with `postgresql.standalone.tolerations`,
give the Job those too. With `postgresql.standalone.existingSecret` the password is in that Secret,
and with `dumps.storage.existingClaim` the dumps are on that claim. With `dumps.enabled=false` the
policy admits no pod of the dumps at all: turn the dumps on, or the NetworkPolicies off, for the
restore. For another database, the Job reaches it as the dumps do (above), with the variables of
`pg_dump`; moving to a database of your own shows one ([Moving to another database
mode](#moving-to-another-database-mode)).

A dump is as old as its schedule: anything after the last dump is lost on restore. Copy the
dumps off the cluster too if the cluster's storage is the only place they live.

#### Restoring a dump from elsewhere

A dump that is not on the dumps' volume, such as one of a
[Compose install that moves to Kubernetes](compose.md#moving-to-kubernetes) or one copied off the
cluster, is given to `pg_restore` on its standard input, through a pod that reaches the database
as the Job above does, while the core is stopped. For the chart's own PostgreSQL, save this as
`hd-restore-pod.yaml`:

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: hd-restore
  labels:
    # The labels of the dumps: with networkPolicy.enabled, only the core and the dumps reach the
    # chart's own PostgreSQL.
    app.kubernetes.io/name: hd
    app.kubernetes.io/instance: hd
    app.kubernetes.io/component: dumps
spec:
  restartPolicy: Never
  securityContext:
    runAsNonRoot: true
    runAsUser: 10001
    runAsGroup: 10001
    seccompProfile:
      type: RuntimeDefault
  containers:
    - name: pg-restore
      # The image of the dumps (dumps.image), at the dump's major version or newer.
      image: docker.io/library/postgres:18.7-trixie
      env:
        - name: PGHOST
          value: hd-postgresql
        - name: PGDATABASE
          value: hd
        - name: PGUSER
          value: hd
        - name: PGPASSWORD
          valueFrom:
            secretKeyRef:
              name: hd-postgresql
              key: password
        - name: PGSSLMODE
          value: disable
      # Waits for the dump, which kubectl exec gives pg_restore below.
      command: ["sleep", "infinity"]
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        capabilities:
          drop: ["ALL"]
```

Then, on the machine that holds the dump, `hd-20261125T023000Z.dump` here:

```sh
kubectl -n helpdesk apply -f hd-restore-pod.yaml
kubectl -n helpdesk wait --for=condition=Ready pod/hd-restore --timeout=300s
kubectl -n helpdesk exec -i hd-restore -- sh -c 'pg_restore --clean --if-exists --no-owner \
  --no-privileges --single-transaction --dbname="$PGDATABASE" && echo restored' \
  < hd-20261125T023000Z.dump
kubectl -n helpdesk delete pod hd-restore
```

It ends with `restored`, and replaces what the dump holds as the Job does, in one transaction. The
pod mounts no volume, so it needs no affinity. For another database, give the pod the variables
the Job gets for it: those of [To a database of your own](#to-a-database-of-your-own), with the
authority's volume, or the cluster's URL of
[To a CloudNativePG cluster](#to-a-cloudnativepg-cluster) in `DATABASE_URL`, with
`--dbname="$DATABASE_URL"`. Only the chart's own PostgreSQL has been restored into this way.

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
backup can be taken with a CloudNativePG `Backup` resource for the cluster `<name>-db`.
Restoring means creating a new cluster that bootstraps from the backup (CloudNativePG's
`bootstrap.recovery`), then pointing the core at it with `postgresql.mode=external`,
`core.database.existingSecret` (its `<cluster>-app` Secret) and, for its authority,
`core.database.ca.existingSecret` (`<cluster>-ca`). The chart refuses `external` while the
release's own cluster `<name>-db` exists ([The upgrade guard](#the-upgrade-guard)): once the
restored cluster holds what you need, delete that one (`kubectl -n helpdesk delete cluster
hd-db`), then upgrade.

## Moving to another database mode

The mode chosen at install ([Choosing the database mode](#choosing-the-database-mode)) can change
later, with the data moved by a [database dump](#database-dumps) and a
[restore](#restoring-a-dump). The chart refuses the new mode for as long as it finds the database
it leaves ([The upgrade guard](#the-upgrade-guard)). In this order:

1. Stop the core (`kubectl -n helpdesk scale deploy/hd-core --replicas=0`) and take a
   [database dump](#database-dumps), or for the CloudNativePG cluster a backup, that outlives the
   database. With `postgresql.mode=cloudnativepg` the dumps run only with `dumps.enabled=true`.
   A database of your own can be filled from the dump now; a database the chart makes, its own
   PostgreSQL or a new CloudNativePG cluster, exists only after the upgrade.
2. Delete what the guard finds:
   - the chart's own PostgreSQL: `kubectl -n helpdesk delete statefulset hd-postgresql` stops it
     and leaves its volume and its password's Secret in place, to go back to or to delete later
     ([Uninstalling](#uninstalling));
   - the CloudNativePG cluster: `kubectl -n helpdesk delete cluster hd-db` deletes the cluster
     **with its data**: the dump or the backup of step 1 is then the only copy.
3. Upgrade with the new mode, and restore the dump into the new database unless it holds the
   data already. Into a database the chart makes, restore with the core still stopped: upgrade
   with `--set core.replicas=0`, restore, then upgrade again without it.

### To a database of your own

From the chart's own PostgreSQL to a database of your own, prepared as
[What the DBA creates](external-database.md#what-the-dba-creates) says, reached over TLS as
[TLS](external-database.md#tls) says, and of PostgreSQL 18 or later: a dump of the chart's
PostgreSQL 18 is not sure to restore into an older version. Here the database is given as
separate values: `helpdesk` on `db.example.com`, owned by the user `helpdesk`, with a certificate
from a private authority whose certificate is in `database-ca.crt`.

1. Create the Secrets of the database's password and of its authority:

   ```sh
   kubectl -n helpdesk create secret generic hd-database \
     --from-literal=password='Lantern@river/72:quiet'
   kubectl -n helpdesk create secret generic hd-database-ca --from-file=ca.crt=database-ca.crt
   ```

2. Stop the core and take a dump:

   ```sh
   kubectl -n helpdesk scale deploy/hd-core --replicas=0
   kubectl -n helpdesk create job --from=cronjob/hd-dumps hd-dump-before-move
   kubectl -n helpdesk wait --for=condition=complete job/hd-dump-before-move --timeout=600s
   kubectl -n helpdesk logs job/hd-dump-before-move
   ```

3. Restore the dump into the new database. Save this as `hd-restore.yaml`, with the dump's name
   from the log in `DUMP`: the Job of [Restoring a dump](#restoring-a-dump), reaching the
   database as the core will, with its certificate verified against the authority:

   ```yaml
   apiVersion: batch/v1
   kind: Job
   metadata:
     name: hd-restore
   spec:
     backoffLimit: 0
     template:
       spec:
         restartPolicy: Never
         # Beside the chart's own PostgreSQL, still running, which mounts the dumps' claim too.
         affinity:
           podAffinity:
             requiredDuringSchedulingIgnoredDuringExecution:
               - topologyKey: kubernetes.io/hostname
                 labelSelector:
                   matchLabels:
                     app.kubernetes.io/name: hd
                     app.kubernetes.io/instance: hd
                     app.kubernetes.io/component: postgresql
         securityContext:
           runAsNonRoot: true
           runAsUser: 10001
           runAsGroup: 10001
           seccompProfile:
             type: RuntimeDefault
         containers:
           - name: pg-restore
             # The image of the dumps (dumps.image), at the database's major version or newer.
             image: docker.io/library/postgres:18.7-trixie
             env:
               - name: DUMP
                 value: hd-20261125T023000Z.dump
               - name: PGHOST
                 value: db.example.com
               - name: PGDATABASE
                 value: helpdesk
               - name: PGUSER
                 value: helpdesk
               - name: PGPASSWORD
                 valueFrom:
                   secretKeyRef:
                     name: hd-database
                     key: password
               - name: PGSSLMODE
                 value: verify-full
               - name: PGSSLROOTCERT
                 value: /etc/hd/database-ca/ca.crt
             command: ["/bin/sh", "-c"]
             args:
               - >-
                 pg_restore --clean --if-exists --no-owner --no-privileges --single-transaction
                 --dbname="$PGDATABASE" "/dumps/$DUMP" && echo "restored $DUMP"
             securityContext:
               allowPrivilegeEscalation: false
               readOnlyRootFilesystem: true
               capabilities:
                 drop: ["ALL"]
             volumeMounts:
               - name: dumps
                 mountPath: /dumps
                 readOnly: true
               - name: database-ca
                 mountPath: /etc/hd/database-ca
                 readOnly: true
         volumes:
           - name: dumps
             persistentVolumeClaim:
               claimName: hd-dumps
           - name: database-ca
             secret:
               secretName: hd-database-ca
   ```

   ```sh
   kubectl -n helpdesk apply -f hd-restore.yaml
   kubectl -n helpdesk wait --for=condition=complete job/hd-restore --timeout=600s
   kubectl -n helpdesk logs job/hd-restore
   kubectl -n helpdesk delete job hd-restore
   ```

4. Stop the chart's own PostgreSQL, which leaves its volume and its password's Secret in place:

   ```sh
   kubectl -n helpdesk delete statefulset hd-postgresql
   ```

5. In `my-values.yaml`, give the database in place of `mode: standalone`; `core.database.sslMode`
   left out is `verify-full` ([TLS to the database](#tls-to-the-database)):

   ```yaml
   postgresql:
     mode: external
     external:
       host: db.example.com
       name: helpdesk
       user: helpdesk
       password:
         existingSecret: hd-database
   core:
     database:
       ca:
         existingSecret: hd-database-ca
   ```

   and upgrade:

   ```sh
   helm upgrade hd oci://ghcr.io/manustars/charts/hd --version <version> -n helpdesk -f my-values.yaml
   kubectl -n helpdesk rollout status deployment/hd-core --timeout=300s
   ```

   The core starts on the new database, with what the dump held. The dumps stop, since they are
   off by default with `external` (`dumps.enabled: true` turns them on against the new
   database); their volume `hd-dumps` stays, with the dumps on it.

6. Once the help desk has proved good on the new database, delete the old one's volume and
   password, which are kept until then to go back to:

   ```sh
   kubectl -n helpdesk delete persistentvolumeclaim data-hd-postgresql-0
   kubectl -n helpdesk delete secret hd-postgresql
   ```

### To a CloudNativePG cluster

From the chart's own PostgreSQL to a cluster of the CloudNativePG operator, which must be
installed: the same order, with a database that exists only after the upgrade.

```sh
kubectl -n helpdesk scale deploy/hd-core --replicas=0
kubectl -n helpdesk create job --from=cronjob/hd-dumps hd-dump-before-move
kubectl -n helpdesk wait --for=condition=complete job/hd-dump-before-move --timeout=600s
kubectl -n helpdesk logs job/hd-dump-before-move
kubectl -n helpdesk delete statefulset hd-postgresql
```

Set `postgresql.mode: cloudnativepg` in `my-values.yaml`, with the cluster's values
(`postgresql.instances`, `storage`, `backup`) and `dumps.enabled: true` to keep the dumps, then
upgrade with the core stopped and wait for the cluster:

```sh
helm upgrade hd oci://ghcr.io/manustars/charts/hd --version <version> -n helpdesk \
  -f my-values.yaml --set core.replicas=0
kubectl -n helpdesk wait --for=condition=Ready cluster/hd-db --timeout=600s
```

Restore with the Job of [Restoring a dump](#restoring-a-dump), without its `affinity` (the chart's
own PostgreSQL is stopped) and with the cluster's URL from its Secret in place of the variables
`PGHOST`, `PGDATABASE`, `PGUSER`, `PGPASSWORD` and `PGSSLMODE`:

```yaml
            - name: DATABASE_URL
              valueFrom:
                secretKeyRef:
                  name: hd-db-app
                  key: uri
```

and `--dbname="$DATABASE_URL"` in place of `--dbname="$PGDATABASE"`. Then start the core:

```sh
helm upgrade hd oci://ghcr.io/manustars/charts/hd --version <version> -n helpdesk -f my-values.yaml
```

Leaving the CloudNativePG cluster goes the same way, with its dumps (`dumps.enabled: true`) or a
backup taken first and `kubectl -n helpdesk delete cluster hd-db`, which deletes the cluster with
its data.

## Uninstalling

`helm uninstall` removes the core, the Assistant and the rest, but leaves the database.

With the chart's own PostgreSQL (`postgresql.mode=standalone`) it leaves the database's volume
claim (`data-hd-postgresql-0`) and the Secret with the generated password
(`hd-postgresql`): the data directory accepts no other password, and installing again with the
same release name, in the same namespace, takes both back. To delete the data too, delete both by
hand:

```sh
kubectl -n helpdesk delete persistentvolumeclaim data-hd-postgresql-0
kubectl -n helpdesk delete secret hd-postgresql
```

Whether the volume itself goes with its claim is up to the StorageClass's reclaim policy. Do not
delete the Secret alone: a new install would generate another password, which the data kept on
the volume does not accept. The volume of the dumps (`hd-dumps`) is kept in every mode.

With `postgresql.mode=cloudnativepg` it leaves the database cluster
(`postgresql.keepOnUninstall`, true by default) so that its data survives; installing again with
the same release name and `postgresql.mode=cloudnativepg` takes it back, and with any other mode
is refused for as long as the cluster exists ([The upgrade guard](#the-upgrade-guard)). To delete
the data too, delete the cluster by hand (`kubectl -n helpdesk delete cluster hd-db`).

## Upgrades

```sh
helm upgrade hd oci://ghcr.io/manustars/charts/hd --version <version> -n helpdesk -f my-values.yaml
```

The chart of a version runs the core and the Assistant of that version; `core.image.tag` and
`assistant.image.tag` set the version of either one on its own. The core is stopped, then started
with the new version, which applies its migrations before serving: expect a short pause for agents
and mail (mail waits in the mailbox and is read on start). Take a backup before upgrading across
versions with migrations. The Assistant can be upgraded on its own (`assistant.image.tag`); events
wait in the core while it restarts.

### A new major version of PostgreSQL

The chart's own PostgreSQL (`postgresql.mode=standalone`) runs the major version the chart fixes,
18. A data directory belongs to one major version: a version of the chart that moves to another
one says so in its upgrade notes, and is upgraded to with a dump and a restore. An upgrade that
leaves them out loses nothing, but the new version's image finds the old data directory on the
volume and does not start. The image of PostgreSQL 18, on the volume of a PostgreSQL 17 laid out
as the chart lays out its own, names the old version's data directory:

```
Error: in 18+, these Docker images are configured to store database data in a
       format which is compatible with "pg_ctlcluster" (specifically, using
       major-version-specific directory names).  …

       Counter to that, there appears to be PostgreSQL data in:
         /var/lib/postgresql/17/docker
```

The core then waits, unready, for its database. `helm -n helpdesk rollback hd` brings the old
version back; then delete the database's pod (`kubectl -n helpdesk delete pod hd-postgresql-0`),
since its StatefulSet does not replace a pod that never got ready.

The upgrade, with the dumps on (the default), `<version>` being the version whose notes announce
the new major version:

1. Stop the core and take a dump with the release as it is, whose `pg_dump` is of the old
   version:

   ```sh
   kubectl -n helpdesk scale deploy/hd-core --replicas=0
   kubectl -n helpdesk create job --from=cronjob/hd-dumps hd-dump-before-upgrade
   kubectl -n helpdesk wait --for=condition=complete job/hd-dump-before-upgrade --timeout=600s
   kubectl -n helpdesk logs job/hd-dump-before-upgrade
   ```

2. Keep the old volume, and free the database's claim for a new one. A PersistentVolume belongs
   to the cluster, not to the namespace: setting its reclaim policy to `Retain` needs the right to
   change it, and keeps the old data directory until you delete the volume. Without it, deleting
   the claim may delete the volume, as its StorageClass says, and the dump is the only copy.

   ```sh
   volume=$(kubectl -n helpdesk get persistentvolumeclaim data-hd-postgresql-0 -o jsonpath='{.spec.volumeName}')
   kubectl patch persistentvolume "$volume" -p '{"spec":{"persistentVolumeReclaimPolicy":"Retain"}}'
   kubectl -n helpdesk delete statefulset hd-postgresql
   kubectl -n helpdesk delete persistentvolumeclaim data-hd-postgresql-0
   ```

3. Upgrade with the core still stopped. The new version starts on a new, empty volume, with the
   password that its Secret keeps:

   ```sh
   helm upgrade hd oci://ghcr.io/manustars/charts/hd --version <version> -n helpdesk \
     -f my-values.yaml --set core.replicas=0
   kubectl -n helpdesk rollout status statefulset/hd-postgresql --timeout=300s
   ```

4. Restore the dump with the Job of [Restoring a dump](#restoring-a-dump), the dump's name from
   the log of step 1 in `DUMP`, and the new version's `dumps.image.tag` in its image
   (`helm show values oci://ghcr.io/manustars/charts/hd --version <version>` prints it):

   ```sh
   kubectl -n helpdesk apply -f hd-restore.yaml
   kubectl -n helpdesk wait --for=condition=complete job/hd-restore --timeout=600s
   kubectl -n helpdesk logs job/hd-restore
   kubectl -n helpdesk delete job hd-restore
   ```

5. Start the core:

   ```sh
   helm upgrade hd oci://ghcr.io/manustars/charts/hd --version <version> -n helpdesk -f my-values.yaml
   kubectl -n helpdesk rollout status deployment/hd-core --timeout=300s
   ```

6. Once the help desk has proved good on the new version, delete the old volume, the one that
   step 2 put in `volume` (`kubectl get persistentvolume` lists it as `Released`, for the claim
   `helpdesk/data-hd-postgresql-0`), and what its storage keeps behind it, as the
   StorageClass's provisioner documents: a volume kept with `Retain` is not cleaned up by
   Kubernetes.

   ```sh
   kubectl delete persistentvolume "$volume"
   ```

### Upgrading from 0.3: standalone is the default database mode

Until 0.3 the chart's default database was a CloudNativePG cluster (`postgresql.enabled`, true
by default). Since 0.4.0 the value is `postgresql.mode`, and its default is `standalone`: a
PostgreSQL the chart runs itself, which needs no operator
([The chart's own PostgreSQL](#the-charts-own-postgresql)). The same values therefore mean
another database, and an upgrade has to say which one the release has:

- **A release on the CloudNativePG cluster the chart made** (0.3 with its default values, or
  with `postgresql.enabled: true`): set `postgresql.mode=cloudnativepg`, and remove
  `postgresql.enabled` from the values if it is there.

  ```sh
  helm upgrade hd oci://ghcr.io/manustars/charts/hd --version <version> -n helpdesk \
    -f my-values.yaml --set postgresql.mode=cloudnativepg
  ```

  Put `postgresql.mode: cloudnativepg` in the values file to keep: every later upgrade needs
  it. The cluster, its data and its Secrets stay as they are.
- **A release on a database of your own** (`postgresql.enabled: false` with
  `core.database.existingSecret`): set `postgresql.mode=external` and remove
  `postgresql.enabled`. If the release ran on the chart's CloudNativePG cluster before that,
  0.3 left the cluster in place (`kubectl -n helpdesk get cluster hd-db` shows it), and the
  chart refuses `external` while it exists, with a message that names `cloudnativepg`: do not
  set that, since the core has not run on that cluster since. Delete the cluster first
  (`kubectl -n helpdesk delete cluster hd-db`), once nothing in it is needed.
- **`postgresql.enabled: true` beside `core.database.existingSecret`**: the core used the
  Secret's database, and the chart made a CloudNativePG cluster beside it that nothing used. Set
  `postgresql.mode=external`, remove `postgresql.enabled`, and delete that cluster first
  (`kubectl -n helpdesk delete cluster hd-db`), since the chart refuses `external` while it
  exists.

An upgrade that leaves the mode out is refused, and changes nothing: the chart finds the
release's CloudNativePG cluster, and stops with a message that names `postgresql.mode=cloudnativepg`
([The upgrade guard](#the-upgrade-guard)); a `postgresql.enabled` left in the values stops it as
well, with the mode to set in its place.

A tool that renders the chart without reading the cluster, such as Argo CD
([GitOps tools](#gitops-tools-argo-cd)), gets no such refusal: there the new default would
replace the CloudNativePG cluster in the release with an empty PostgreSQL of the chart's own, and the core would start on it. Set `postgresql.mode` in
the values **before** moving such a release to 0.4.0. Whether the cluster and its data would
survive that is up to the tool: Helm leaves in place a cluster that leaves the release
(`postgresql.keepOnUninstall`), and a tool that prunes what the chart no longer renders may not.

### Upgrading from 0.3: verified TLS to the database

From this version the core verifies the certificate of a database on another host
([TLS to the database](#tls-to-the-database)); 0.3 used TLS if the server offered it, without
verifying it. Upgrade the chart together with the core: the new core under the 0.3 chart (only
`core.image.tag` changed) reaches the CloudNativePG cluster with `verify-full` but without its
authority, and does not start.

- **The CloudNativePG cluster** (`postgresql.mode=cloudnativepg`):
  nothing to do. The chart sets `verify-full` and mounts the cluster's authority from
  `<name>-db-ca`, which CloudNativePG has created for every cluster the chart made.
- **An external database** (`postgresql.mode=external`) on another host: from now on its
  certificate must be from a trusted authority, for the host in the URL. Before upgrading, for a
  private authority put its certificate in a Secret and set `core.database.ca.existingSecret`; a
  certificate from a public authority needs nothing; a CloudNativePG cluster named in
  `existingSecret`, such as a restored one, needs `core.database.ca.existingSecret:
  <cluster>-ca`. A database that does not serve TLS, or one to keep as before for now, needs
  `core.database.sslMode` (`prefer` is 0.3's behaviour), or `sslmode` in its URL; the core then
  logs a warning at every start. A core that cannot verify the certificate does not start and
  logs why: the database is not touched, and setting one of the above and upgrading again brings
  the core back.
- **Outside the chart**, the same holds for `HD_DATABASE_URL` and the separate parameters, with
  `HD_DATABASE_SSLMODE` and `HD_DATABASE_CA_FILE` ([TLS](external-database.md#tls)).

The database dumps (`dumps.*`) of an external database follow the same values as far as their
image can verify ([Database dumps](#database-dumps)).
