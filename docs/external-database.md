# Running on a PostgreSQL database of your own

The help desk can run on a PostgreSQL database that you provide, instead of one its installation
brings (the external database of
[ADR 0009](adr/0009-two-ways-to-install-three-ways-to-get-the-database.md)). This page holds
whatever the installation: what the DBA creates, how the core connects, how the connection is
protected, and how secrets are read from files. The installation guides say where each setting
goes: [Deploying to Kubernetes](deploy.md) for the Helm chart.

## What the DBA creates

- **PostgreSQL 16 or later.** At startup, before applying its migrations, the core logs the
  server's version and stops on an older one with a message naming the version it found, such as
  `the database server is PostgreSQL 15.8, and the help desk needs PostgreSQL 16 or later`.
- **A database for the help desk alone, owned by the user the core connects as.** Not a database
  shared with other applications, not even in a schema of its own: restoring the help desk would
  then touch their data too.
- **Nothing else.** The core applies its migrations itself, as that user, every time it starts:
  `hd serve`, `hd migrate` (which applies them and exits) and every other `hd` command that uses
  the database. They need nothing beyond owning the database: no superuser and no extension.

Run this as a superuser (such as `postgres`) or as a role with `CREATEROLE` and `CREATEDB`, with
your own names and password:

```sql
CREATE ROLE helpdesk LOGIN PASSWORD 'Lantern@river/72:quiet';
GRANT helpdesk TO CURRENT_USER;  -- so that a DBA who is not a superuser can give it the database
CREATE DATABASE helpdesk OWNER helpdesk ENCODING 'UTF8' TEMPLATE template0;
REVOKE ALL ON DATABASE helpdesk FROM PUBLIC;
REVOKE helpdesk FROM CURRENT_USER;
```

| Statement | Why |
|---|---|
| `CREATE ROLE … LOGIN` | The core's user: not a superuser, and it cannot create databases or roles. |
| `GRANT helpdesk TO CURRENT_USER` | A role that is not a superuser may give a database only to a role it can act as; without this, `CREATE DATABASE` fails with `must be able to SET ROLE "helpdesk"`. The last line takes it back. |
| `ENCODING 'UTF8' TEMPLATE template0` | Mail arrives in every language. `template0` makes the encoding work whatever the server's `template1` uses. |
| `REVOKE ALL … FROM PUBLIC` | PostgreSQL lets every role of the server connect to a new database; afterwards only its owner and the superusers can. |

The `public` schema of a new database belongs to the database's owner (PostgreSQL 15 and later),
so nothing needs granting on it. The core opens up to 10 connections, and every `hd` command run
beside it opens its own.

## Connecting

Give the core the database either as a connection string or as separate parameters, not both.

As a connection string, with `@`, `/` and `:` inside the user name or password percent-encoded
(`%40`, `%2F`, `%3A`):

```sh
HD_DATABASE_URL='postgres://helpdesk:Lantern%40river%2F72%3Aquiet@db.example.com:5432/helpdesk'
```

As separate parameters, with the password as the DBA gives it, without percent-encoding:

```sh
HD_DATABASE_HOST=db.example.com
HD_DATABASE_PORT=5432
HD_DATABASE_NAME=helpdesk
HD_DATABASE_USER=helpdesk
HD_DATABASE_PASSWORD='Lantern@river/72:quiet'
```

Every database variable, which every `hd` command that uses the database reads, also as the
option in the second column:

| Variable | Option | Meaning |
|---|---|---|
| `HD_DATABASE_URL` | `--database-url` | The connection string, `postgres://user:password@host[:port]/database`. |
| `HD_DATABASE_URL_FILE` | `--database-url-file` | A file holding `HD_DATABASE_URL` (see [Secrets from files](#secrets-from-files)). |
| `HD_DATABASE_HOST` | `--database-host` | The server's host name or address. |
| `HD_DATABASE_PORT` | `--database-port` | The server's port; default 5432. |
| `HD_DATABASE_NAME` | `--database-name` | The database, dedicated to the help desk. |
| `HD_DATABASE_USER` | `--database-user` | The user the core connects as, which owns the database and applies the migrations. |
| `HD_DATABASE_PASSWORD` | `--database-password` | That user's password, with no percent-encoding. |
| `HD_DATABASE_PASSWORD_FILE` | `--database-password-file` | A file holding `HD_DATABASE_PASSWORD`. |
| `HD_DATABASE_SSLMODE` | `--database-sslmode` | How the connection is protected (see [TLS](#tls)). |
| `HD_DATABASE_CA_FILE` | `--database-ca-file` | A PEM file of the authorities that issued the database's certificate (see [TLS](#tls)). |

Surrounding whitespace is removed from every value, and a blank one counts as not set; so a
password that starts or ends with a space cannot be given. The core stops, with a message naming
the variables, when it gets:

- `HD_DATABASE_URL` (or its file) together with any separate parameter, the port included;
- the separate parameters in part: the host, the name, the user and the password are all needed;
- neither.

## TLS

Without `HD_DATABASE_SSLMODE`, the mode depends on where the database is:

| Database | Default |
|---|---|
| On another host | `verify-full`: TLS, with a certificate from a trusted authority for the host the core dials. |
| On the core's own host: `localhost`, a loopback address or a Unix socket | `prefer`: TLS if the server offers it, its certificate not verified. |

A `localhost` dialled at an address that is not a loopback one (`hostaddr` in the connection
string, or `PGHOSTADDR`) counts as another host. The PostgreSQL client's own variables
(`PGSSLMODE`) do not change the mode for a database on another host.

`HD_DATABASE_SSLMODE` sets the mode for the separate parameters and for a connection string
without its own `sslmode`:

| Mode | What it leaves unchecked |
|---|---|
| `verify-full` | Nothing. |
| `verify-ca` | The server's certificate is not checked against its host name. |
| `require` | The server's certificate is not verified. |
| `prefer` | The connection is encrypted only if the server offers TLS, and the server's certificate is not verified. |
| `disable` | The connection is not encrypted. |

A connection string that carries `sslmode` (or `ssl-mode`) keeps it; it may also be `allow`,
which the core treats as no TLS. Setting `HD_DATABASE_SSLMODE` beside it stops the core:
`HD_DATABASE_URL carries sslmode=require and HD_DATABASE_SSLMODE is verify-full: give the mode in
one of them, not both`.

For a database on another host, any mode but `verify-full`, `verify-ca` included, from the
variable or from the connection string, is logged as a warning at every start, such as:

```text
WARN hd::database: the database is not on this machine and is reached with sslmode require (set by HD_DATABASE_SSLMODE): the server's certificate is not verified; verify-full is the default
```

A database on the core's own host is never warned about, whatever the mode.

With `verify-full`, give the database by a name (or address) that its certificate lists. A
certificate the core cannot verify stops it with the reason, such as `invalid peer certificate:
UnknownIssuer` for an authority it does not trust.

### A private authority

For a certificate from an authority of your own, point `HD_DATABASE_CA_FILE` at a PEM file of
that authority's certificate; it is trusted besides the public web PKI's authorities. The core
reads the file at startup and stops, naming it, when it cannot. It also stops when the file is
given:

- with a mode that does not verify the certificate (`require`, `prefer`, `disable`, `allow`).
  That includes a database on the core's own host without `HD_DATABASE_SSLMODE`, whose default is
  `prefer`: set `HD_DATABASE_SSLMODE=verify-full` there;
- beside a connection string that carries its own authorities (`sslrootcert`, `ssl-root-cert` or
  `ssl-ca`).

## Secrets from files

Every secret of the core and of the Assistant can be read from a file instead of a variable, such
as a mounted Kubernetes Secret or a Compose secret: set the `_FILE` variable to the file's path.

| Secret | Its file | Read by |
|---|---|---|
| `HD_DATABASE_URL` | `HD_DATABASE_URL_FILE` | Every `hd` command that uses the database. |
| `HD_DATABASE_PASSWORD` | `HD_DATABASE_PASSWORD_FILE` | Every `hd` command that uses the database. |
| `HD_IMAP_URL` | `HD_IMAP_URL_FILE` | `hd serve`. |
| `HD_SMTP_URL` | `HD_SMTP_URL_FILE` | `hd serve`. |
| `HD_OIDC_CLIENT_SECRET` | `HD_OIDC_CLIENT_SECRET_FILE` | `hd serve`, `hd sign-in local off`, `hd sign-in check`. |
| `HD_AGENT_PASSWORD` | `HD_AGENT_PASSWORD_FILE` | `hd agent add`, `hd agent set-password`. |
| `HD_ASSISTANT_TOKEN` | `HD_ASSISTANT_TOKEN_FILE` | The Assistant. |
| `HD_ASSISTANT_EVENT_SECRET` | `HD_ASSISTANT_EVENT_SECRET_FILE` | The Assistant. |
| `HD_ASSISTANT_API_KEY` | `HD_ASSISTANT_API_KEY_FILE` | The Assistant. |

The file's content is used with the surrounding whitespace (such as a final newline) removed.
Setting a variable and its `_FILE` together, or naming a missing or empty file, stops the core or
the Assistant with a message saying which; the Assistant stops even for a provider that takes no
key. Each `_FILE` of the core is also an option, the secret's option with `-file` added:
`--database-url-file`, or `--password-file` beside `hd agent add --password`.

`HD_DATABASE_CA_FILE` and `HD_MAIL_CA_FILE` are not secrets read from files: they name PEM files
of certificate authorities, and have no variable holding the content instead.
