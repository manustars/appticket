# Deploying the help desk

The help desk ships as three container images, a Helm chart and an archive for Compose:

| | |
|---|---|
| `ghcr.io/manustars/hd-core` | The core: API, the agents' UI (served by the core itself), mail reading and sending, add-on events. Runs `hd serve`; any other `hd` command runs the same way. |
| `ghcr.io/manustars/hd-assistant` | The Assistant add-on (optional). |
| `ghcr.io/manustars/hd-zabbix` | The Zabbix add-on (optional). |
| `oci://ghcr.io/manustars/charts/hd` | The Helm chart `hd` (its source is `deploy/chart`): the core and the add-ons, the database in one of three modes (a PostgreSQL the chart runs itself, a PostgreSQL cluster through CloudNativePG, or one of your own), backups, an optional test mail server for staging. |
| `hd-compose-1.2.3.tar.gz` | The Compose install for a single host (its source is `deploy/compose`), attached to the GitHub release of each version with its SHA-256 checksum (`hd-compose-1.2.3.tar.gz.sha256`): it unpacks into `hd-compose-1.2.3/`, whose `compose.yaml` runs the images of that version. |

The images are published by `.github/workflows/release.yml` when a version tag (`v1.2.3`) is
pushed: tagged with the version and, for a final version, `latest`. They are built from
`deploy/Dockerfile` (`--target core`, `--target assistant`, `--target zabbix`), run as the
non-root user 10001 and
stop cleanly on SIGTERM. The same workflow then publishes the chart with the tag's version
(`1.2.3`, or `1.2.3-rc.1` for a pre-release), and last creates the tag's GitHub release, a
pre-release for a pre-release version, with the Compose archive attached. It publishes nothing
when the tag disagrees with the version in `deploy/chart/Chart.yaml`, or when the archive cannot
be built.

## Where to install

- **On Kubernetes**, with the Helm chart: [Installing on Kubernetes](kubernetes.md) gives the
  prerequisites, the three database modes and how to choose one, the Secrets, the install itself,
  backups, upgrades, and moving from one database mode to another.
- **On a single host**, with Docker Compose or Podman: [Installing with Compose](compose.md) gives
  the install from the release's archive, with its own PostgreSQL or an existing one, HTTPS,
  backups, upgrades, a restore, and moving to Kubernetes.
- **On a PostgreSQL database of your own**, whatever the install:
  [Running on a PostgreSQL database of your own](external-database.md) says what the DBA creates,
  how the core connects and protects the connection, and how secrets are read from files.

The sections below are the same wherever the help desk runs. Their commands are shown for the
Kubernetes install, with the release `hd` in the namespace `helpdesk`, where `hd` runs inside the
core's pod (`kubectl -n helpdesk exec deploy/hd-core -- hd …`), and their settings as values of
the chart beside the environment variables they set. How they read with Compose is in
[First steps](compose.md#first-steps) of the Compose guide.

## Behind a reverse proxy

The core serves the agents' UI and the API over plain HTTP. HTTPS is the job of a reverse proxy
in front of it: the ingress controller on Kubernetes ([HTTPS](kubernetes.md#https)), the proxy
the host has with Compose, or Caddy of the Compose install
([deploy/compose/reverse-proxy.md](../deploy/compose/reverse-proxy.md)). Whatever the proxy:

- It serves HTTPS at the address of `HD_PUBLIC_URL`, the `https://` address agents open, with no
  path, which the identity provider sends agents back to. It passes every request on, the UI and
  `/api/` alike. The session cookie is marked `Secure`, so browsers keep it only over HTTPS (or
  for `http://localhost`).
- It adds the browser's address on the right of `X-Forwarded-For`. That is the only header the
  core reads from a proxy: it reads neither `Host` nor `X-Forwarded-Proto`, `X-Forwarded-Host` or
  `Forwarded`, does not check `Origin`, and uses no WebSocket.
- It accepts requests of up to 10 MiB, the size of an
  [import](#importing-customers-sites-requesters-and-assets). nginx and ingress-nginx accept 1 MiB
  unless told otherwise (`client_max_body_size`, the annotation
  `nginx.ingress.kubernetes.io/proxy-body-size`).

### Trusted proxies

A failed sign-in counts under the address of the connection it comes on, which behind a proxy is
the proxy's: with no proxy trusted, a handful of failures by anyone (nine with the defaults)
refuse every agent for a while ([Failed sign-ins](kubernetes.md#failed-sign-ins)).
`HD_TRUSTED_PROXIES` lists the proxies whose `X-Forwarded-For` the core believes, as addresses or
networks separated by commas (`192.0.2.10, 198.51.100.0/24`); empty, it believes none.

The core reads the header from the right and takes the first address that is not a trusted
proxy; what the browser itself wrote on the left is never believed. With a load balancer in front
of the proxy that adds to `X-Forwarded-For` too, list its addresses as well, and make sure the
proxy passes on what the load balancer added. List only proxies that you run: anything that
connects from a listed address can write `X-Forwarded-For`, and so get round the limit per
address. The limit per account still holds.

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
one address at a time (the address is taken as for
[failed sign-ins](kubernetes.md#failed-sign-ins), so set `core.signIn.trustedProxies` behind a
proxy); beyond that the core answers 429 with `Retry-After` until one ends. A sign-in ends when the agent comes back from the provider, or
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
[failed sign-in](kubernetes.md#failed-sign-ins)), and `hd agent set-password` or Administration
can give them one.

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

When people in the group are refused as not in it (`not_in_authorized_group`), the core's log
line "the ID token does not put the person in the authorized group" says what the claim held:
`nothing: the ID token has no such claim` when the token lacks it, or its value, such as the
group's path when "Full group path" is on.

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

## The Zabbix add-on

The Zabbix add-on brings Zabbix's hosts into the catalog and opens tickets from its alerts by
rules; its README (`addons/zabbix/README.md`) describes its settings, its user in Zabbix and
how Zabbix is set up to send it the alerts. As for the Assistant, the order is: install (with
`zabbix.enabled=false`), register, create the Secret, enable.

```sh
kubectl -n helpdesk exec deploy/hd-core -- hd addon add zabbix \
  --url http://hd-zabbix:8091 \
  --scope directory:read --scope catalog:write --scope names:write --scope conflicts:write \
  --scope tickets:read --scope tickets:write --event ticket.state_changed
```

The command prints a token and an event secret once. Put them in a Secret, with the API token
of the add-on's user in Zabbix and a webhook secret of your own, of 16 characters or more, which
Zabbix's media type sends:

```sh
kubectl -n helpdesk create secret generic hd-zabbix \
  --from-literal=token='…' --from-literal=event-secret='…' --from-literal=api-token='…' \
  --from-literal=webhook-secret="$(openssl rand -hex 32)"
```

Then set the values and upgrade. `zabbix.config` is the add-on's configuration file without the
sections `[core]`, `[addon]` and `[webhook]`, which the chart writes from the other values:

```yaml
zabbix:
  enabled: true
  secret:
    existingSecret: hd-zabbix
  config: |
    [zabbix]
    url = "https://zabbix.example.com"

    [assets]
    default_kind = "Server"
  # The webhook at https://<ingress.host>/webhook, for a Zabbix outside the cluster.
  ingress:
    enabled: true
```

Zabbix's media type then sends to `https://helpdesk.example.com/webhook`, through the ingress
controller, which passes that path alone on to the add-on; a Zabbix server in the cluster uses
`http://hd-zabbix.helpdesk.svc:8091/webhook`. The add-on starts without a Zabbix that answers,
and tries again at each round. With `networkPolicy.enabled`, the add-on's NetworkPolicy lets in
the core and `networkPolicy.zabbix.webhookFrom` (and `networkPolicy.coreFrom` with the Ingress),
and lets it reach the name service, the core and Zabbix's API (`networkPolicy.zabbix.apiTo` on
`apiPorts`, any address on 443 and 80 by default, so a Zabbix on another port needs it in
`apiPorts`). It reaches the core at the core's pods only: `zabbix.coreUrl` must then be the
core's Service, as it is when left empty, and the chart refuses another address, such as the
Ingress's.

`zabbix.ingress` needs the core's Ingress (`ingress.enabled`), whose host, class, TLS and
annotations it takes, but for cert-manager's (`cert-manager.io/…`): the core's Ingress has the
host's certificate issued into `ingress.tls.secretName`, which the webhook's Ingress then uses as
it is, without a second certificate claiming the same Secret.

Unlike the Assistant, which takes its secrets as variables, the Zabbix add-on reads them from
the Secret mounted as files (the `_FILE` variables): its webhook's secret is read only from a
file (`webhook.secret_file`), so the Secret is mounted anyway, and the other three come from the
same mount.

