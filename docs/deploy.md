# Deploying to Kubernetes

The help desk ships as two container images and a Helm chart:

| | |
|---|---|
| `ghcr.io/manustars/hd-core` | The core: API, the agents' UI (served by the core itself), mail reading and sending, add-on events. Runs `hd serve`; any other `hd` command runs the same way. |
| `ghcr.io/manustars/hd-assistant` | The Assistant add-on (optional). |
| `oci://ghcr.io/manustars/charts/hd` | The Helm chart `hd` (its source is `deploy/chart`): both components, the database in one of three modes (a PostgreSQL the chart runs itself, a PostgreSQL cluster through CloudNativePG, or one of your own), backups, an optional test mail server for staging. |

The images are published by `.github/workflows/release.yml` when a version tag (`v1.2.3`) is
pushed: tagged with the version and, for a final version, `latest`. Both are built from
`deploy/Dockerfile` (`--target core` / `--target assistant`), run as the non-root user 10001 and
stop cleanly on SIGTERM. The same workflow then publishes the chart with the tag's version
(`1.2.3`, or `1.2.3-rc.1` for a pre-release), and publishes nothing when the tag disagrees with
the version in `deploy/chart/Chart.yaml`.

## Prerequisites

- Kubernetes 1.27 or later, Helm 3.
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

## Secrets to create

The chart never holds secrets; it refers to Secrets you create in the release's namespace. The
one Secret it makes itself is the generated password of its own PostgreSQL
(`postgresql.mode=standalone`).

| Secret | Keys | When |
|---|---|---|
| database | `uri`: `postgres://user:password@host:5432/db` | only with `postgresql.mode=external`, for a database given as a URL; set `core.database.existingSecret` (and `secretKey`) |
| database password | `password` | only with `postgresql.mode=external`, for a database given as separate values; set `postgresql.external.password.existingSecret` (and `secretKey`) |
| standalone password | `password` | only with `postgresql.mode=standalone` under a tool that renders the chart without reading the cluster, such as Argo CD; set `postgresql.standalone.existingSecret` (and `secretKey`); otherwise the chart generates it (see [The chart's own PostgreSQL](#the-charts-own-postgresql)) |
| mail | `imap-url`, `smtp-url` | real mail: `core.mail.imap.existingSecret`, `core.mail.smtp.existingSecret` (may be the same Secret) |
| mail CA | `ca.crt` | mail servers with a private certificate authority: `core.mail.ca.existingSecret` |
| database CA | `ca.crt` | an external database whose certificate comes from a private authority: `core.database.ca.existingSecret` (see [TLS to the database](#tls-to-the-database)) |
| identity provider | `client-secret` | signing in through OpenID Connect: `core.oidc.existingSecret` (see [Signing in through an identity provider](#signing-in-through-an-identity-provider)) |
| Assistant | `token`, `event-secret`, and `api-key` for a hosted model | `assistant.secret.existingSecret`, after registering the add-on |
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
the chart without reading the cluster: the chart generates the password into the Secret `<name>-postgresql`
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
- a `dumps.enabled` that is not `true`, `false` or empty.

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
from the first release after 0.3.1 on: for 0.3.1 and earlier, or to try a change to the chart,
install from a clone of the repository instead, with `deploy/chart` in place of the `oci://…`
address and without `--version`.

The chart runs the images of its `appVersion`, which for a published chart is its version, unless
`core.image.tag` and `assistant.image.tag` say otherwise; from a clone, a release must have been
published (a `v…` tag pushed) for that tag to exist. A package published to GitHub's container
registry, an image or the chart, starts out private: make it public in the package settings once
(it cannot be made private again), or sign in with `helm registry login ghcr.io` for the chart,
and for the images create a registry Secret and list it in `imagePullSecrets`.

The examples on this page use the release name `hd` and the namespace `helpdesk`: the chart
names its objects after the release (`hd-core`, `hd-assistant`, `hd-db`, `hd-postgresql`), and a release whose name
does not contain `hd` gets `<release>-hd-…` instead.

`deploy/chart/values.yaml` documents every value (`helm show values
oci://ghcr.io/manustars/charts/hd --version <version>` prints it);
`deploy/chart/values-staging.example.yaml` is a small staging install with the test mail server.
The most common settings:

| Value | Meaning |
|---|---|
| `core.image.tag` | The version to run (default: the chart's `appVersion`). |
| `core.mail.from`, `core.mail.fromName` | The help desk's own address and display name, for replies. |
| `core.publicUrl`, `core.oidc.*` | Signing in through an OpenID Connect identity provider (see [Signing in through an identity provider](#signing-in-through-an-identity-provider)). |
| `core.mail.imap/smtp.existingSecret` | The mail Secrets above. |
| `ingress.*` | Host, class, annotations and TLS (see [HTTPS](#https)). |
| `core.signIn.*` | Failed sign-in limits and trusted proxies (see [Failed sign-ins](#failed-sign-ins)). |
| `dumps.*` | Scheduled database dumps to a volume (see [Backups](#backups)). |
| `postgresql.mode` | Where the database comes from: `standalone` (the default), `cloudnativepg` or `external`. The chart refuses a mode that is not the one of the database it finds (see [The upgrade guard](#the-upgrade-guard)). |
| `postgresql.standalone.*` | The chart's own PostgreSQL: storage size and class, its password's Secret (see [The chart's own PostgreSQL](#the-charts-own-postgresql)). |
| `postgresql.external.*` | An external database as separate values. |
| `postgresql.*` | The CloudNativePG cluster's instances, storage size and class, backups. |
| `assistant.*` | See [The Assistant](#the-assistant). |
| `greenmail.enabled` | The staging test mail server. |
| `networkPolicy.enabled` | Ingress NetworkPolicies; list the ingress controller's namespace in `networkPolicy.coreFrom`. With `standalone`, only the core and the dumps reach the database. |

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
  upgrading the chart brings PostgreSQL's fixes.
- **Volume.** `postgresql.standalone.storage.size` (5Gi) and `storageClass` describe the volume
  when it is first created; Kubernetes refuses an upgrade that changes them. The chart never deletes the
  volume: `helm uninstall` leaves its claim, `data-hd-postgresql-0`, in place.
- **Password.** On the first install the chart generates the password of the user `hd` into the
  Secret `hd-postgresql`; upgrades find that Secret and keep what it holds, and `helm uninstall`
  leaves it beside the volume. PostgreSQL takes the password when it first creates its data
  directory: changing the Secret afterwards does not change the password in the database.
- **GitOps tools.** A tool that renders the chart without reading the cluster, such as Argo CD
  and anything else built on `helm template`, cannot find the Secret the chart made, and would
  generate another password at every sync, which the database does not accept. There, create a
  Secret of your own before installing and name it in `postgresql.standalone.existingSecret`
  (its key in `secretKey`, `password` by default); the chart then generates nothing:

  ```sh
  kubectl -n helpdesk create secret generic hd-postgresql-password \
    --from-literal=password='PASSWORD'
  ```

  Set `postgresql.mode` explicitly with such a tool too: [the upgrade guard](#the-upgrade-guard)
  does not run there.

- **No TLS inside the cluster.** The core reaches the database by its Service without TLS, and
  the chart sets that explicitly (`HD_DATABASE_SSLMODE=disable`), so that the core's log names
  the choice in a warning at every start. `core.database.sslMode` and `core.database.ca` are for
  an external database and refused here. With `networkPolicy.enabled`, only the core and the
  dumps reach the database's port.
- **Dumps.** The [database dumps](#database-dumps) are on unless `dumps.enabled=false`: the
  chart's own PostgreSQL has no other backup.
- **`helm install --wait`.** Helm's `--wait` (and `--atomic`, which implies it) waits for every
  PersistentVolumeClaim of the release to be bound. With a StorageClass that binds a volume only
  when a pod first uses it (`volumeBindingMode: WaitForFirstConsumer`), the claim of the dumps
  stays pending until the first dump runs, and `--wait` runs into its timeout though the help
  desk is up. Install without `--wait` and follow the core with `kubectl -n helpdesk rollout
  status deployment/hd-core`, or give the dumps a claim that is already bound
  (`dumps.storage.existingClaim`). The same holds in the other modes with `dumps.enabled=true`.

### The upgrade guard

The chart refuses an install or an upgrade whose `postgresql.mode` is not the mode of the
database it finds in the release's namespace, so that the core never starts on another, empty
database while the help desk's data sits in the one it left. That holds for a mode set by
mistake and for one that comes from the chart's defaults, which changed in 0.4.0
([Upgrading from 0.3](#upgrading-from-03-standalone-is-the-default-database-mode)). It finds:

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
another one, which starts empty. Set postgresql.mode=cloudnativepg to stay on the cluster: …
```

Nothing has changed when it appears: the release, the database and the core are as they were.
Upgrade again with the mode it names.

To leave a database for good, move its data to the new one first, then delete what the guard
finds, and upgrade with the new mode: the chart refuses every other mode for as long as that
object exists.

- The chart's own PostgreSQL: `kubectl -n helpdesk delete statefulset hd-postgresql` stops it
  and leaves its volume and its password's Secret in place, to go back to or to delete later
  ([Uninstalling](#uninstalling)).
- The CloudNativePG cluster: `kubectl -n helpdesk delete cluster hd-db` deletes the cluster with
  its data. Keep a dump or a backup of it until the new database has proved good.

If it finds both, it cannot tell which one holds the help desk's data, and refuses every mode
until one of the two is deleted.

**What the guard needs.** Helm must read the cluster while it renders the chart, as `helm
install` and `helm upgrade` do, with the right to read StatefulSets in the release's namespace
and, where CloudNativePG is installed, its clusters there. Where Helm does not read the cluster
the guard finds nothing and refuses nothing:

- **GitOps tools** that render the chart themselves, such as Argo CD and anything else built on
  `helm template`. Set `postgresql.mode` explicitly in the values there, so that no new default
  ever changes it, and with `standalone` name the password's Secret too
  (`postgresql.standalone.existingSecret`, see
  [The chart's own PostgreSQL](#the-charts-own-postgresql)).
- **`helm template`**, and **`helm upgrade --dry-run`** unless it is `--dry-run=server`, which
  reads the cluster and shows the guard's answer without changing anything.

### HTTPS

The Ingress always lists its host for TLS when `ingress.tls.enabled` is true (the default):

- with `ingress.tls.secretName` empty, the ingress controller serves its default certificate,
  which suits a staging install where agents accept that certificate;
- with a TLS Secret named there, or one created by cert-manager through an annotation in
  `ingress.annotations`, the controller serves that certificate.

To send plain HTTP to HTTPS with ingress-nginx, add the annotation
`nginx.ingress.kubernetes.io/force-ssl-redirect: "true"`.

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

The core reads the header from the right and takes the first address that is not a trusted
proxy; what the browser itself wrote on the left is never believed. With a load balancer in front
of the ingress controller that adds to `X-Forwarded-For` too, list its addresses as well, and
make sure the ingress controller passes on what the load balancer added. List only proxies that
you run: anything that connects from a listed address can say it is anyone.

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
characters), or name a file holding it with `HD_AGENT_PASSWORD_FILE` or `--password-file`.
Customers, their domains, assets and services can then be managed in the UI, and so can agents and
roles, under Administration.

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
hd agent set-password bo@example.com     # generated and printed, or from HD_AGENT_PASSWORD(_FILE)
hd agent deactivate bo@example.com       # cannot sign in; their open tickets are unassigned
hd agent reactivate bo@example.com
```

The core always keeps one active agent with the Administrator role. If nobody who can still sign
in has it, `hd agent set-role <e-mail> Administrator` gives it back; the command line is not
limited by roles. Upgrading from a version without roles makes every existing agent an
Administrator.

### Signing in through an identity provider

Agents can sign in through the organization's OpenID Connect identity provider (such as
Keycloak, Authentik or Google) next to their local password
([ADR 0008](adr/0008-identity-providers-authenticate-the-product-authorizes.md)). The sign-in
page then shows a "Sign in with …" button. Without an [authorized group](#the-authorized-group)
only existing agents sign in this way: someone the provider knows who is not an agent of the help
desk is refused. With one, only the provider's users in that group sign in, and someone in it who
is not an agent yet becomes one at their first sign-in.

At the provider, register a confidential client for the help desk (authorization code flow; the
core uses PKCE with S256) with the redirect URI `<public URL>/api/session/identity-provider/callback`,
such as `https://helpdesk.example.com/api/session/identity-provider/callback`. Put its secret in a
Secret:

```sh
kubectl -n helpdesk create secret generic hd-oidc --from-literal=client-secret='…'
```

and set the values:

```yaml
core:
  publicUrl: https://helpdesk.example.com   # empty: https:// and ingress.host
  oidc:
    issuer: https://login.example.com/realms/staff
    clientId: helpdesk
    existingSecret: hd-oidc
    label: Northgate                        # the button reads "Sign in with Northgate"
    sessionHours: 12
    authorizedGroup: helpdesk-agents        # empty: only existing agents sign in
    groupClaim: groups
```

Outside the chart the same settings are environment variables: `HD_OIDC_ISSUER`,
`HD_OIDC_CLIENT_ID`, `HD_OIDC_CLIENT_SECRET` or `HD_OIDC_CLIENT_SECRET_FILE` (a file holding
the secret, such as a mounted Secret), `HD_PUBLIC_URL`, `HD_OIDC_LABEL` (default `OpenID
Connect`), `HD_OIDC_SESSION_HOURS` (default 12, at most 24), `HD_OIDC_AUTHORIZED_GROUP` (none by
default) and `HD_OIDC_GROUP_CLAIM` (default `groups`). The issuer and the public URL
must be `https://` addresses; `http://` is accepted only for a server on the core's own machine
(`localhost` or a loopback address), as for the mail servers. Without an issuer, client id and
secret nothing changes; with only some of them, or an authorized group without them, the core
does not start, and says which are missing (the chart already refuses an issuer without a public
URL). The core reads the provider's configuration and keys when an agent signs in, not at
startup, so a provider that is down does not keep the core or local passwords from working.

#### Checking the identity provider

`hd sign-in check` says whether the configured provider answers and accepts the client, before
any agent tries to sign in. It reads the same settings as the core, needs no database, and is run
where the core runs:

```sh
kubectl -n helpdesk exec deploy/hd-core -- hd sign-in check
```

It reads the provider's configuration and keys from `<issuer>/.well-known/openid-configuration`
as a sign-in does, then asks the provider's token endpoint, with the client id and secret, to
exchange a code the provider never issued. A provider that accepts the client refuses only the
code (`invalid_grant`), and the check passes; the provider may log that refusal as a failed
sign-in. This relies on the provider checking the client before the code, as Keycloak does; it
has been tried against Keycloak only, and a provider that checks the code first would refuse it
the same way whatever the secret, so with another provider a passed check is confirmed by
signing in once. The check fails, exits with 1 and says which of these it found:

- the provider cannot be reached at the issuer (the reason follows: connection refused, a name
  that does not resolve, a timeout, a certificate the core does not trust);
- no OpenID Connect configuration at the issuer, such as a realm that does not exist;
- a wrong issuer: the provider names its issuer otherwise, and `issuer` must match it exactly,
  trailing slash included;
- the client rejected (`invalid_client` or `unauthorized_client`): a wrong client id or secret, or
  a client not allowed to use the authorization code flow;
- an answer that says neither that the provider accepts the client nor that it rejects it, shown
  as the provider gave it.

It also exits with 1, saying so, when no identity provider is configured, or with only some of
its settings. The client secret is never printed. The check does not use the client credentials
grant, which a sign-in client usually may not use. It does not check that the provider accepts the
redirect URI, nor what its ID tokens hold (the e-mail, whether it is verified, the authorized
group's claim): signing in once shows those.

Anyone can open the sign-in through the provider, so at most 20 sign-ins may be under way from
one address at a time (the address is taken as for [failed sign-ins](#failed-sign-ins), so set
`core.signIn.trustedProxies` behind a proxy); beyond that the core answers 429 with
`Retry-After` until one ends. A sign-in ends when the agent comes back from the provider, or
after 10 minutes.

How the provider's sign-in finds the agent:

- With an authorized group, only someone in it goes further, at every sign-in: anyone else is
  refused, even an agent.
- The first time, by e-mail, and only when the provider says the address is verified (the
  `email_verified` claim of the ID token; the core asks for the `openid`, `email` and `profile`
  scopes). The identity is then linked to that agent. A provider that does not send
  `email_verified: true` in the ID token links no existing agent: check that yours does.
- The first time, when no agent has the address and the person is in the authorized group: they
  become an agent with the default role (never the Administrator role, not even on an
  installation without agents), linked to the identity and without a local password, with the
  name and e-mail the provider sends (the e-mail, verified or not, names them when there is no
  name). Without an e-mail there is nobody to link and nothing to make an agent with. Add the
  first agent with `hd agent add` before anyone signs in: once an agent exists, `hd agent add`
  gives the default role too, and `hd agent set-role <e-mail> Administrator` hands out the
  Administrator role.
- From then on, by the provider's stable identifier (`sub`), even after the address changed
  there. The agent's name and e-mail follow the provider at every sign-in, the e-mail whether or
  not the provider says it is verified (that matters only the first time). When the provider
  sends no name or no e-mail, the agent keeps the one they have. A linked agent's name and e-mail
  cannot be edited in the help desk.
- Refused, with the reason on the sign-in page: someone not in the authorized group, a missing
  e-mail or an unverified one with no link yet (an unverified address never picks an existing
  agent), someone who is not an agent while no authorized group is set, a deactivated agent
  found by their link or address (signing in never reactivates one, nor makes a new agent in
  their place; but see [the authorized group](#the-authorized-group)), and an e-mail, verified
  or not, that belongs to another agent, including the first time an agent already linked to
  another identity of the provider.

A session opened through the provider lasts `sessionHours`, so that an account disabled there
loses access within hours; one opened with a local password lasts 14 days. Signing out ends the
help desk's session only, not the one at the provider. Existing agents keep their local password
when they are linked. Agents made by a first sign-in have none: while local sign-in is on, a
password typed for them on the sign-in page is refused as a wrong one is (and counts as a
[failed sign-in](#failed-sign-ins)), and `hd agent set-password` or Administration can give them
one.

#### The authorized group

`authorizedGroup` (`HD_OIDC_AUTHORIZED_GROUP`) is a value the provider sends in the ID token
claim `groupClaim` (`HD_OIDC_GROUP_CLAIM`, default `groups`) for those who may sign in. The claim
must be in the ID token itself, at its top level, as a list of strings or a single string; the
value is compared exactly, case included. A token without the claim, or without the value in it,
is refused. Being in the group says who may enter, not what they may do: an agent's role is
always managed in the help desk ([ADR 0007](adr/0007-roles-and-permissions.md)), and no group of
the provider maps to a role.

The authorized group applies to signing in through the provider only. An agent with a local
password still signs in with it, in the group or not: deactivate an agent to keep them out, or
[switch local sign-in off](#switching-local-sign-in-off) (`hd sign-in local off`) so that
everyone signs in through the provider, where the group decides.

The group, not deactivation, is what keeps someone out of the provider's sign-in for good. A new
agent takes the address the provider sends, verified or not (with Entra ID always unverified,
since it sends no `email_verified`; guest users included). A deactivated agent is found by their
link, or, if they never signed in through the provider, by their address; if neither matches any
more (they were never linked and their address changed at the provider), someone in the group
comes back as a new agent. When someone leaves the help desk, remove them from the group at the
provider as well as deactivating their agent.

With Keycloak, add a "Group Membership" mapper to the client with the token claim name
`groups`, "Full group path" off and "Add to ID token" on; `authorizedGroup` is then the group's
name.

#### Microsoft Entra ID

The recommended setup sends an app role in the `roles` claim rather than the group in `groups`:
`groups` carries the groups' object IDs rather than their names, needs a groups claim configured
on the app, and is left out of the token for users in many groups, so the authorized group could
go missing for exactly those users. An app role is in the ID token whenever it is assigned.

1. In App registrations, register the help desk (accounts in this organizational directory only)
   with a Web redirect URI `<public URL>/api/session/identity-provider/callback`, and create a
   client secret under Certificates & secrets for the `hd-oidc` Secret.
2. Under App roles, create a role for users and groups, such as "Help desk agent" with the value
   `Helpdesk.Agent`.
3. In Enterprise applications, open the help desk and assign the role to the group of agents
   under Users and groups (assigning groups needs Entra ID P1 or P2; otherwise assign users).
   Only the group's direct members get the role: nested groups do not count. Setting
   "Assignment required" under Properties also keeps everyone else from signing in at Entra ID.
4. Set the values:

```yaml
core:
  oidc:
    issuer: https://login.microsoftonline.com/<tenant id>/v2.0
    clientId: <application (client) id>
    existingSecret: hd-oidc
    label: Microsoft
    authorizedGroup: Helpdesk.Agent
    groupClaim: roles
```

Entra ID sends the `email` claim for the `email` scope the core asks for, for users with an
address. Its ID tokens carry no `email_verified` claim, though, so no existing agent is linked by
e-mail: people in the group become new agents at their first sign-in, and one whose address an
existing agent already has is refused (`email_not_verified`) until that agent's address changes.
That includes the first Administrator: before
[switching local sign-in off](#switching-local-sign-in-off), give the Administrator role to an
agent made by a first sign-in (`hd agent set-role <e-mail> Administrator`).

#### Switching local sign-in off

Once agents sign in through the provider, local passwords can be switched off for the whole
installation, so that the provider decides who comes in. Whether local sign-in is on is kept in
the database, not in the deployment, and changed with `hd`:

```sh
kubectl -n helpdesk exec deploy/hd-core -- hd sign-in local off   # agents sign in through the provider only
kubectl -n helpdesk exec deploy/hd-core -- hd sign-in local       # says whether it is on or off
kubectl -n helpdesk exec deploy/hd-core -- hd sign-in local on    # local passwords work again
```

Switching it off ends every session opened with a local password, at once; sessions opened
through the provider go on. From then on the sign-in page shows only the "Sign in with …" button,
and the API refuses every local password, right or wrong, with 403 before checking it (nothing is
counted as a failed sign-in). It is refused while no identity provider is configured: `hd` reads
the provider's settings from its environment, so run it in the core's container as above, where
the chart sets them. Make sure an agent with the Administrator role can sign in through the provider
before switching it off; [`hd sign-in check`](#checking-the-identity-provider) first tells whether
the provider answers and accepts the client. Agents keep their local passwords, unused while it is off.

The command line can always switch local sign-in back on: it needs nothing but the database. If
the provider cannot be reached, or its settings were removed, while local sign-in is off:

```sh
kubectl -n helpdesk exec deploy/hd-core -- hd sign-in local on
kubectl -n helpdesk exec deploy/hd-core -- hd agent set-password agent@example.com   # generated and printed
```

and that agent signs in with the password. The core starts even with local sign-in off and no
identity provider configured, so that `hd` can be run in its container; it then logs an error, and
the sign-in page says that nobody can sign in until local sign-in is switched back on. A provider
configured in part, or a client secret file that cannot be read, still stops the core: fix or
remove those settings first, then switch local sign-in on. When the core starts with local
sign-in off, it also ends any session opened with a local password that is still there.

### Importing customers, sites, requesters and assets

Customers, their sites, their requesters and their assets can be imported from CSV files, in
the UI (Customers, then Import from CSV files) by an agent who manages customers, or with
`hd import`; an assets file also needs managing the catalog, and without it the whole import is
refused. Each kind has its own file, in UTF-8 (Excel: "CSV UTF-8"), separated by commas or
semicolons, with a header row of the column names below in any order. The files of one import
are taken customers first, then sites, then requesters, then assets, so that a row can refer to
something an earlier file creates. Every import first shows what it would create, update and
leave out; nothing changes until it is applied (the Import button, or `--apply`), and applying
checks every row again.

A filled cell sets a value and an empty one leaves it; an import never deletes, retires or
offboards anything, never marks a requester departed, never brings an offboarded customer, a
retired site, a departed requester or a retired asset back, and never changes an asset an
add-on is the source of. A row that breaks a rule is left out with its reason, as are the
sites, requesters and assets whose customer row is left out, the requesters and assets whose
site row is left out, and the assets whose holder's row is left out; the other rows are
imported. The rows left out come back as a file with
the same columns and an `error` column giving the reason, ready to correct and import again (an
`error` column is ignored on import). A column without a name is ignored when all its cells are
empty, as in the columns spreadsheets write after the last one. A file with an unknown or
missing column, a file that is not UTF-8, or more than 5,000 rows in a file stops the whole
import before anything changes. In the UI, the files of one import travel in one JSON request
of at most 10 MiB, so their CSV text together must be somewhat smaller: quotes, line ends and
non-ASCII characters take more room once encoded as JSON.

Customers file. A customer is recognised by its external reference, else by its name, ignoring
case; a row that could be two customers (two names that differ only in case, or a reference
naming one customer and a name naming another) is left out, and a customer is imported once
per file.

| Column | Meaning |
|---|---|
| `name` | The customer's name. The column is required; a new customer needs a value. |
| `external_system`, `external_name` | An external reference: a system, such as a monitoring tool, and the name it uses for the customer. Both or neither. It recognises the customer; one no customer has is added to the customer of the row. |
| `legal_name` | The legal name. |
| `tax_id`, `tax_country` | The tax identifier and its country (ISO 3166-1 alpha-2, such as `IT`); one always comes with the other. |
| `address` | The postal address; it can span several lines inside quotes. |
| `phone` | A phone number. |
| `website` | An `http` or `https` address; `https://` is added when there is none. |
| `notes` | Internal notes; a filled cell replaces the notes there. |

Sites file. It is taken after the customers file, so that a site can be at a customer that file
creates. A site is recognised by its name within its customer, ignoring case; a name that only a
retired site has is left out.

| Column | Meaning |
|---|---|
| `customer` | The site's customer, by name. |
| `customer_external_system`, `customer_external_name` | Or the customer by one of its external references. A sites file has the `customer` column, these two, or all three. |
| `name` | The site's name. Required. |
| `address` | The site's address. The column is required; a new site needs a value. |

Requesters file. A requester is recognised by any of their e-mail addresses, and is imported
once per file. A row is about one requester of its customer who has not departed, or about a
new one when none of its addresses belongs to anyone. It is left out when an address belongs to
a requester of another customer (departed or not), or to a requester who has departed, or when
its addresses belong to two requesters: an import moves, merges and brings back no one, and
takes no address from anyone. Those are done on the requester's page.

| Column | Meaning |
|---|---|
| `customer` | The requester's customer, by name. It must exist, or be created by the customers file, and not be offboarded. |
| `customer_external_system`, `customer_external_name` | Or the customer by one of its external references. A requesters file has the `customer` column, these two, or all three. |
| `email_addresses` | The requester's e-mail addresses, separated by commas, semicolons, spaces or line ends. Required. Those the requester does not have are added; none is taken away. |
| `name` | The requester's name. |
| `phone_numbers` | Phone numbers, separated by semicolons or line ends. Those the requester does not have are added; none is taken away. |
| `job_title` | The job title. |
| `site` | The requester's site, by name: one of their customer's sites, existing or created by the sites file, and not retired. |
| `notes` | Internal notes; a filled cell replaces the notes there. |

Assets file. An asset is recognised by its serial number within its customer, else by its name
within its customer, ignoring case, and is imported once per file. A row is about one asset
entered by hand and not retired, or about a new one when nothing matches. It is left out when it
matches an asset an add-on is the source of (even when it would change nothing: only the add-on
changes it), a retired asset (an active asset with its serial number or name comes first; a name
that only a retired asset has is not given to a new one), or two assets (two active ones with
its serial number, or, when its serial number matches none, two with its name); and when it is
recognised by its name while that asset has another serial number, since an import replaces no
serial number. Those are done on the asset's page.

| Column | Meaning |
|---|---|
| `customer` | The asset's customer, by name. It must exist, or be created by the customers file, and not be offboarded. |
| `customer_external_system`, `customer_external_name` | Or the customer by one of its external references. An assets file has the `customer` column, these two, or all three. |
| `name` | The asset's name. The column is required; a new asset needs a value. |
| `serial_number` | The serial number. It recognises the asset; one recognised by its name that has none gets it. |
| `kind` | The asset kind, by name, ignoring case: one of the list the help desk manages (Administration, Asset kinds). The column is required; a new asset needs a value. |
| `site` | The asset's own site, by name: one of its customer's sites, existing or created by the sites file, and not retired. |
| `holder` | The requester the asset is given to, by one of their e-mail addresses: a requester of its customer, existing or created by the requesters file, who has not departed. |

From the command line, inside the core's pod, whose file system is read-only, a file is read
from standard input:

```sh
kubectl -n helpdesk exec -i deploy/hd-core -- hd import --customers - < customers.csv          # preview
kubectl -n helpdesk exec -i deploy/hd-core -- hd import --customers - --apply < customers.csv  # import
```

The command lists every row with what it does, or why it is left out. Run on a machine with
access to the database, `hd import --customers customers.csv --sites sites.csv --requesters
requesters.csv --assets assets.csv` takes the four files in one import and writes the rows left
out to `customers-left-out.csv`, `sites-left-out.csv`, `requesters-left-out.csv` and
`assets-left-out.csv` beside them (or into `--left-out DIR`); it refuses to write one over a
file it imports. `hd import --help` describes the columns too.

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
(`dumps.image.tag`) at the database's major version or newer.

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
kubectl -n helpdesk logs job/hd-dump-now
```

To restore, stop the core, restore the chosen dump into the database with `pg_restore` from a pod
that mounts the dumps volume (for example `pg_restore --clean --if-exists --no-owner
--dbname="$DATABASE_URL" /dumps/hd-<date>.dump`; with the chart's own PostgreSQL or an external
database given as separate values the job has `PGHOST`, `PGUSER` and the others instead of a URL:
`--dbname="$PGDATABASE"`), then start the core again. With the chart's own PostgreSQL and
`networkPolicy.enabled`, only the pods of the core and of the dumps reach the database: give the
restore pod the labels of the dumps (`app.kubernetes.io/name: hd`, `app.kubernetes.io/instance:
<release>` and `app.kubernetes.io/component: dumps`), or it gets no answer. With
`dumps.enabled=false` the policy admits no pod of the dumps at all: turn the dumps on, or the
NetworkPolicies off, for the restore.

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
`bootstrap.recovery`), then pointing the core at it with `postgresql.mode=external`,
`core.database.existingSecret` (its `<cluster>-app` Secret) and, for its authority,
`core.database.ca.existingSecret` (`<cluster>-ca`). The chart refuses `external` while the
release's own cluster `<release>-db` exists ([The upgrade guard](#the-upgrade-guard)): once the
restored cluster holds what you need, delete that one (`kubectl -n helpdesk delete cluster
hd-db`), then upgrade.

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
  `postgresql.enabled`.
- **`postgresql.enabled: true` beside `core.database.existingSecret`**: the core used the
  Secret's database, and the chart made a CloudNativePG cluster beside it that nothing used. Set
  `postgresql.mode=external`, remove `postgresql.enabled`, and delete that cluster first
  (`kubectl -n helpdesk delete cluster hd-db`), since the chart refuses `external` while it
  exists.

An upgrade that leaves the mode out is refused, and changes nothing: the chart finds the
release's CloudNativePG cluster, and stops with a message that names `postgresql.mode=cloudnativepg`
([The upgrade guard](#the-upgrade-guard)); a `postgresql.enabled` left in the values stops it as
well, with the mode to set in its place.

A tool that renders the chart without reading the cluster, such as Argo CD, gets no such
refusal: there the new default would replace the CloudNativePG cluster in the release with an
empty PostgreSQL of the chart's own, and the core would start on it. Set `postgresql.mode` in
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
