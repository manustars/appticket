#!/usr/bin/env bash
# Starts a PostgreSQL that serves TLS with a certificate from a test authority, for the test that
# connects to it with verify-full (core/tests/database_tls.rs).
#
# The authority and the server's certificate are made here, into DIR, and never committed: the
# server's certificate is for localhost and 127.0.0.1, valid for two days. The key and the
# certificate are copied into the container through standard input, so that no bind mount has
# to give the key to the server's user with the permissions PostgreSQL wants.
#
# Usage:
#   bash scripts/tls-test-database.sh DIR
#
# Prints the two variables the test reads, so that CI can append them to $GITHUB_ENV:
#   HD_TEST_TLS_DATABASE_URL=postgres://hd:hd@127.0.0.1:<port>/hd
#   HD_TEST_TLS_DATABASE_CA_FILE=<DIR>/ca.crt
#
# Environment: CONTAINER_ENGINE (podman by default; docker in CI), NAME (the container,
# hd-dev-db-tls), PORT (the host port, 55455), IMAGE (docker.io/library/postgres:18).
# Remove it afterwards with `podman rm -f -v hd-dev-db-tls`.
set -euo pipefail

dir=${1:?usage: $0 DIR}
engine=${CONTAINER_ENGINE:-podman}
name=${NAME:-hd-dev-db-tls}
port=${PORT:-55455}
image=${IMAGE:-docker.io/library/postgres:18}
# Git Bash would otherwise rewrite the paths inside the container as Windows ones.
export MSYS_NO_PATHCONV=1

mkdir -p "$dir"
cd "$dir"

cat > ca.cnf <<'EOF'
[req]
distinguished_name = dn
x509_extensions = ca
prompt = no
[dn]
CN = hd test authority
[ca]
basicConstraints = critical, CA:TRUE
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
EOF
cat > server.cnf <<'EOF'
[req]
distinguished_name = dn
prompt = no
[dn]
CN = localhost
[server]
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost, IP:127.0.0.1
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid
EOF

openssl req -x509 -new -config ca.cnf -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout ca.key -out ca.crt -days 2 >&2
openssl req -new -config server.cnf -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout server.key -out server.csr >&2
openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -set_serial "$(date +%s)" -days 2 \
  -extfile server.cnf -extensions server -out server.crt >&2

"$engine" rm -f -v "$name" > /dev/null 2>&1 || true
"$engine" run -d --name "$name" -p "127.0.0.1:$port:5432" \
  -e POSTGRES_USER=hd -e POSTGRES_PASSWORD=hd -e POSTGRES_DB=hd "$image" > /dev/null

# The server that initializes the database listens on its socket only: the address answers once
# the real one runs.
for _ in $(seq 60); do
  if "$engine" exec "$name" pg_isready -q -h 127.0.0.1 -U hd -d hd; then
    break
  fi
  sleep 1
done
"$engine" exec "$name" pg_isready -q -h 127.0.0.1 -U hd -d hd

"$engine" exec "$name" mkdir -p /etc/hd-tls
for file in server.crt server.key; do
  "$engine" exec -i "$name" sh -c "cat > /etc/hd-tls/$file" < "$file"
done
"$engine" exec "$name" sh -c \
  'chown postgres:postgres /etc/hd-tls/server.crt /etc/hd-tls/server.key && chmod 0600 /etc/hd-tls/server.key'
"$engine" exec "$name" psql -q -U hd -d hd -v ON_ERROR_STOP=1 \
  -c "ALTER SYSTEM SET ssl_cert_file = '/etc/hd-tls/server.crt'" \
  -c "ALTER SYSTEM SET ssl_key_file = '/etc/hd-tls/server.key'" \
  -c "ALTER SYSTEM SET ssl = on" \
  -c "SELECT pg_reload_conf()" > /dev/null

# A reload that cannot read the key leaves TLS off: check that a connection gets it.
for _ in $(seq 10); do
  if [ "$("$engine" exec -e PGPASSWORD=hd "$name" psql -tA \
    "host=127.0.0.1 user=hd dbname=hd sslmode=require" \
    -c "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()" 2> /dev/null)" = t ]; then
    echo "HD_TEST_TLS_DATABASE_URL=postgres://hd:hd@127.0.0.1:$port/hd"
    # A path the test can open: on Windows, Git Bash's own form of it is not one.
    echo "HD_TEST_TLS_DATABASE_CA_FILE=$(pwd -W 2> /dev/null || pwd)/ca.crt"
    exit 0
  fi
  sleep 1
done
echo "$name does not serve TLS:" >&2
"$engine" logs "$name" >&2
exit 1
