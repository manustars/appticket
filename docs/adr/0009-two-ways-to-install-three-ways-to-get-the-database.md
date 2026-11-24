# Two ways to install, three ways to get the database

The help desk installs on Kubernetes with its Helm chart, or on a single host with Compose (Docker
or Podman). Either way the database comes in one of three modes:

- **standalone**, the default: PostgreSQL from the official image, one instance on a persistent
  volume that outlives the installation, its password generated at install, scheduled dumps on by
  default, reachable only by the core and the dumps. The major version is fixed by the release
  and changes only through a documented dump and restore.
- **cloudnativepg**, on Kubernetes only: a cluster managed by the CloudNativePG operator, for help
  desks that already run it and want high availability or point-in-time recovery.
- **external**: a PostgreSQL 16 or later database of the help desk's own, given as a URL or as
  separate parameters, dedicated to the help desk and owned by the core's user, which applies the
  migrations. For a database on another host, TLS with a verified certificate is the default;
  anything weaker is set explicitly and logged at startup. One on the core's own host (loopback,
  `localhost`, a Unix socket) keeps the connection's usual default, unverified TLS when offered.

Every secret can be read from a file, so that Compose keeps them out of its environment file and
Kubernetes can mount them. The chart refuses an upgrade whose database mode does not match the
database it finds, so that changing a default can never point the core at an empty database.

## Considered options

- **CloudNativePG as the only bundled database.** Gives backups, failover and upgrades, but ties
  every install to an operator that needs cluster-wide rights to install; the first chart did
  this, and the maintainer wants an install that needs nothing but the chart.
- **Only an external database.** The simplest chart, but a small help desk would have to run
  PostgreSQL by itself before trying the product.
- **A community PostgreSQL chart or image (Bitnami).** Most of those images stopped being freely
  available in 2025; the official image is the one development and CI already use.
- **A shared database with a schema of its own.** Saves a database, but a restore could no longer
  be done without touching other applications' data.
- **Systemd units or Docker Swarm.** One more way to install, document and test, for what a single
  host with Compose already covers.
