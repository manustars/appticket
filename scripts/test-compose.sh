#!/usr/bin/env bash
# Starts the Compose install (deploy/compose, or a release's archive of it unpacked) on this
# machine and checks it in both of its database modes (ADR 0009):
#
#   1. the configuration, every profile on: the install runs the images of VERSION; secrets/
#      ships with no secret; the bundled PostgreSQL publishes no port on the host, and the core
#      waits for it to be healthy; every secret reaches the services as a file named by its
#      _FILE variable (POSTGRES_PASSWORD_FILE for PostgreSQL), none as a variable, even one set
#      in .env, and an optional secret that .env does not name reaches no service; the
#      certificates of private authorities that .env names reach the core and the dumps as files
#      named by HD_DATABASE_CA_FILE and HD_MAIL_CA_FILE, and none reaches them when it names none;
#   2. with the profile db and no password in .env: `docker compose run --rm db-password`
#      writes a password to secrets/db_password, a file the core and PostgreSQL can read, and
#      the core starts on the bundled PostgreSQL, reached by its service name, with that file
#      the only secret; /api/health answers that the database is up, and the database takes the
#      file's password and refuses another. After `down` and `up -d` the core reaches it again,
#      with the same file, which running the command again keeps;
#   3. the profiles greenmail and assistant added, their secrets in files: a message sent to the
#      test mail server's port on the host is read by the core, and the Assistant, registered on
#      the core with `hd addon add` inside the core's container, reaches the core with its
#      token; neither `compose config` nor `inspect` of any container shows any secret's value;
#   4. without the profile db: the core starts on a PostgreSQL of the test's own, given to it
#      through the HD_DATABASE_* parameters and secrets/db_password, with another database,
#      user and password than the bundled one's; /api/health answers that the database is up,
#      the core's migrations are in that database, and no one is shown that password;
#   5. the dumps, with the profile db: `up` starts the schedule, at 02:30 UTC by default, and
#      not the one-shot dumps, and the schedule takes a dump at the time HD_DUMPS_TIME gives;
#      after a customer is added through the core's API, `docker compose run --rm dumps` leaves
#      complete dumps on the volume, the latest HD_DUMPS_KEEP of them and no temporary file,
#      which hold the customers and come out of the volume unchanged; no one is shown the
#      database's password;
#   6. a fresh install, its volumes removed and a new password written, restored from the last
#      of those dumps with the commands that deploy/compose/dumps.md gives, which start db
#      alone: the agent signs in with the password of before and reads the customer back
#      through the API; and so again after starting over as that file says, the database's
#      volume removed and the dumps kept;
#   7. the dumps without the profile db: the schedule starts only once the profile dumps is
#      added; it and `docker compose run --rm dumps` dump the test's own PostgreSQL of step 4,
#      and a stop ends the schedule at once; no one is shown that database's password. Without
#      HD_DATABASE_SSLMODE the dumps require TLS, which that PostgreSQL does not serve, and warn
#      that they would not verify the server's certificate;
#   8. without the profile db, the test's own PostgreSQL of step 4 serving TLS with a certificate
#      from an authority of the test's own, for the name the core dials, and HD_DATABASE_SSLMODE
#      not set (verify-full): the core refuses the certificate (UnknownIssuer) until .env names
#      the authority's certificate, a file of secrets/; then /api/health answers that the
#      database is up, every connection of the core is over TLS, and `docker compose run --rm
#      dumps` dumps the database without warning that it does not verify it, and fails with
#      another authority's certificate in that file;
#   9. without the profile https, behind nginx started by the test on the host's network, as a
#      proxy on the host is, with the configuration of examples/nginx.conf (its ports changed)
#      and a certificate of the test's own for the host name and at the paths it names. The
#      install's network is HD_SUBNET, or its default without it, and the core trusts it as its
#      proxies. An install made before HD_SUBNET is moved to it by `down` and `up -d` (whether
#      `up -d` alone does is noted): the core then sees the connections to its published port
#      come from the gateway. An agent signs in over HTTPS through nginx and is answered with
#      the session cookie, which a request needing it then takes. A failed sign-in counts under
#      the address nginx saw it from, not the one the client made up, also with
#      HD_TRUSTED_PROXIES set to the gateway in .env, and under nginx's address with it set
#      empty. Caddy is not started;
#  10. with the profile https and the certificate internal: Caddy's root certificate is read as
#      reverse-proxy.md gives it, /api/health answers over HTTPS through Caddy verified against
#      it, plain HTTP is sent to HTTPS, and an agent signs in through Caddy, a failed sign-in
#      counting under the address Caddy saw it from, the gateway;
#  11. with the certificate files: a certificate the test makes, in secrets/, is the one Caddy
#      serves;
#  12. with the certificate acme: Caddy turns the install's Caddyfile into a configuration that
#      asks Let's Encrypt for the host of HD_PUBLIC_URL, without starting (`caddy adapt`), so
#      that Let's Encrypt is never asked.
#
# The install the test runs is a copy of the Compose directory in a directory of the test's own,
# where it writes the secrets as secrets/README.md says, with the permissions it gives. Its
# settings are the shipped .env.example with the test's values put in, written there too and
# given with --env-file: nothing is written into the Compose directory, so a .env or a secret
# there is neither read nor touched.
#
# Usage:
#   bash scripts/test-compose.sh VERSION
#
# VERSION is the tag of ghcr.io/manustars/hd-core and ghcr.io/manustars/hd-assistant that the
# local engine holds, such as images built with
#   docker build -f deploy/Dockerfile --target core      -t ghcr.io/manustars/hd-core:VERSION .
#   docker build -f deploy/Dockerfile --target assistant -t ghcr.io/manustars/hd-assistant:VERSION .
# The repository's compose.yaml reads it from .env (HD_VERSION), where the test sets it. A
# release's archive names its own version in its compose.yaml and reads none: the test sets none,
# and VERSION must be the archive's, as CI tests it with
#   bash scripts/package-compose.sh VERSION DIR
#   tar -xzf DIR/hd-compose-VERSION.tar.gz -C UNPACKED
#   COMPOSE_DIR=UNPACKED/hd-compose-VERSION bash scripts/test-compose.sh VERSION
# The test never pulls them; it pulls PostgreSQL, GreenMail, BusyBox, nginx and Caddy. Needs
# docker (or podman) with Compose, curl, openssl, and GNU find and stat (as on Linux and in Git
# Bash); its sed takes only what POSIX sed does. It prints a line per step and exits with 1 at
# the first check that fails, after printing the containers' logs; it removes what it started
# either way. Of the repository it reads only the Compose directory.
#
# Environment: COMPOSE_DIR (the Compose directory, deploy/compose; such as an unpacked release
# archive), CONTAINER_ENGINE (docker by default; podman), COMPOSE (the Compose command,
# "$CONTAINER_ENGINE compose"), PROJECT (the Compose project, hd-test-compose), PORT (a free
# local port for the core, 55479; the two after it are the test mail server's), PROXY_PORT (the
# first of four free local ports, the three after PORT's by default: nginx's HTTP and HTTPS, then
# Caddy's), SUBNET (the install's network, a /24: 172.16.N.0/24 by default, N being the core's
# port modulo 256).
#
# nginx runs on the host's network, which Docker and Podman on Linux have. With Podman (or Docker)
# on Windows or macOS that network is the one of the engine's machine, and the test reaches
# nginx's ports only where the machine forwards its loopback ports to the host's, as WSL does.
set -euo pipefail

version=${1:?usage: test-compose.sh VERSION}
root=$(cd "$(dirname "$0")/.." && pwd)
compose_dir=${COMPOSE_DIR:-$root/deploy/compose}
engine=${CONTAINER_ENGINE:-docker}
read -r -a compose_command <<< "${COMPOSE:-$engine compose}"
project=${PROJECT:-hd-test-compose}
port=${PORT:-55479}
# The test mail server's ports on the host, for sending it messages: the two after the core's.
smtp_port=$((port + 1))
imap_port=$((port + 2))
# The proxies' ports on the host: nginx's for HTTP and HTTPS, then Caddy's.
proxy_port=${PROXY_PORT:-$((port + 3))}
nginx_http_port=$proxy_port
nginx_https_port=$((proxy_port + 1))
caddy_http_port=$((proxy_port + 2))
caddy_https_port=$((proxy_port + 3))
# The install's network, one of this run's own so that runs on one engine do not collide: a /24,
# whose gateway is the first address.
subnet=${SUBNET:-172.16.$((port % 256)).0/24}
if ! [[ $subnet =~ ^([0-9]{1,3}\.){3}0/24$ ]]; then
  echo "SUBNET must be a /24, such as the default, not $subnet" >&2
  exit 2
fi
gateway=${subnet%.0/24}.1

# The environment would win over the test's settings: Compose takes a variable from it before
# the file given with --env-file.
while read -r variable; do
  unset "$variable"
done < <(compgen -e | grep -E '^(HD_|COMPOSE_)' || true)
# Git Bash would otherwise rewrite the paths given to commands inside the containers as Windows
# ones; the paths of the test's own files are given in a form that both understand instead.
export MSYS_NO_PATHCONV=1
native_path() {
  if command -v cygpath > /dev/null; then cygpath -m "$1"; else echo "$1"; fi
}
compose_dir=$(native_path "$compose_dir")
work=$(native_path "$(mktemp -d)")
# The install, a copy of the Compose directory, and its secrets.
install="$work/install"
secrets="$install/secrets"

step() {
  echo
  echo "==> $*"
}

ok() {
  echo "    ok: $*"
}

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# Compose on the install's own file, with the test's settings.
hd_compose() {
  local files=(-f "$install/compose.yaml")
  if [ -f "$work/external.yaml" ]; then
    files+=(-f "$work/external.yaml")
  fi
  "${compose_command[@]}" -p "$project" "${files[@]}" --env-file "$work/.env" "$@"
}

# The test's own PostgreSQL, outside the install, on a network of its own that the core joins
# through the test's addition to the install's file: as a database on another host is reached.
external_network="$project-external"
external_db="$project-external-db"
external_started=""

# Runs SQL in the test's own PostgreSQL, as USER in DATABASE.
external_sql() {
  local user=$1 database=$2 sql=$3
  "$engine" exec "$external_db" psql -h 127.0.0.1 -U "$user" -d "$database" \
    -v ON_ERROR_STOP=1 -tA -c "$sql"
}

# What the containers say, for whoever reads the log of a failure.
diagnose() {
  echo
  echo "---- the containers ----"
  hd_compose ps -a || true
  echo "---- their logs ----"
  hd_compose logs --no-color --tail=100 || true
}

finish() {
  local status=$?
  if [ -f "$work/.env" ]; then
    if [ "$status" -ne 0 ]; then
      diagnose
    fi
    hd_compose --profile db --profile assistant --profile greenmail --profile https down -v \
      --remove-orphans > /dev/null 2>&1 || true
  fi
  if [ -n "${proxy_started:-}" ]; then
    if [ "$status" -ne 0 ]; then
      echo "---- the test's own nginx ----"
      "$engine" logs --tail=50 "$proxy" || true
    fi
    "$engine" rm -f "$proxy" > /dev/null 2>&1 || true
  fi
  if [ -n "$external_started" ]; then
    if [ "$status" -ne 0 ]; then
      echo "---- the test's own PostgreSQL ----"
      "$engine" logs --tail=50 "$external_db" || true
    fi
    "$engine" rm -f -v "$external_db" > /dev/null 2>&1 || true
    "$engine" network rm "$external_network" > /dev/null 2>&1 || true
  fi
  rm -rf "$work"
  exit "$status"
}
trap finish EXIT

# A password made for this run, letters and digits only.
password() {
  od -An -N16 -tx1 /dev/urandom | tr -d ' \n'
}

# Sets KEY to VALUE in the test's settings: on the line of .env.example that sets it, or that
# shows it commented out; the test fails when the example has neither, since an install would
# not find the setting there.
setting() {
  local key=$1 value=$2
  grep -qE "^#? ?$key=" "$work/.env" || fail ".env.example has no line for $key"
  awk -v key="$key" -v value="$value" '
    !done && $0 ~ "^#? ?" key "=" { print key "=" value; done = 1; next }
    { print }
  ' "$work/.env" > "$work/.env.new"
  mv "$work/.env.new" "$work/.env"
}

# Adds KEY=VALUE to the test's settings, for what an install does not usually set.
extra() {
  echo "$1=$2" >> "$work/.env"
}

# Writes VALUE to the install's secret file NAME, as secrets/README.md says, with the mode it
# gives.
secret() {
  printf '%s\n' "$2" > "$secrets/$1"
  chmod 644 "$secrets/$1"
}

# The files of the directory DIRECTORY, separated by spaces, in order.
files_of() {
  find "$1" -mindepth 1 -maxdepth 1 -printf '%f\n' | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//'
}

# Expects the directory DIRECTORY, which WHAT names, to hold the files EXPECTED and no other.
holds() {
  local directory=$1 what=$2 expected=$3 found
  found=$(files_of "$directory")
  [ "$found" = "$expected" ] || fail "$what holds '$found', not '$expected'"
}

# The secrets an install may go without, as the core and the Assistant name them. Each is a file
# of secrets/ named after it (secrets/imap_url) only when .env names that file, in
# HD_SECRET_IMAP_URL and so on.
optional_secrets="HD_IMAP_URL HD_SMTP_URL HD_OIDC_CLIENT_SECRET HD_ASSISTANT_API_KEY"

# Names in the test's settings the file of the optional secret VARIABLE, as .env.example shows.
name_secret() {
  local name=${1#HD_}
  setting "HD_SECRET_$name" "./secrets/${name,,}"
}

# Expects the service whose configuration is in FILE to be given VARIABLE=VALUE.
given() {
  local file=$1 variable=$2 value=$3
  grep -qE "^      $variable: $value$" "$work/$file" \
    || fail "${file%.yaml} is not given $variable=$value: $(cat "$work/$file")"
}

# Runs the command that secrets/README.md gives before the first start, and expects it to say
# TEXT.
db_password_command() {
  local text=$1
  hd_compose run --rm db-password > "$work/db-password.txt" 2>&1 \
    || fail "db-password failed: $(cat "$work/db-password.txt")"
  grep -qF "$text" "$work/db-password.txt" \
    || fail "db-password did not say '$text': $(cat "$work/db-password.txt")"
}

# Whether the bundled PostgreSQL takes PASSWORD, or without it the password of secrets/db_password
# as its container reads it, over the network, where it asks for one (it trusts its own loopback
# address).
db_takes() {
  # shellcheck disable=SC2016 # expanded inside the container
  hd_compose exec -T db sh -c 'PGPASSWORD=${1:-$(cat /run/secrets/db_password)} psql -h db \
    -U "$POSTGRES_USER" -d "$POSTGRES_DB" -tAc "SELECT 1"' sh "${1:-}" > /dev/null 2>&1
}

# Waits up to a minute for the log of SERVICE to hold TEXT.
expect_log() {
  local service=$1 text=$2
  for _ in $(seq 60); do
    if hd_compose logs --no-color "$service" 2> /dev/null | grep -qF "$text"; then
      return 0
    fi
    sleep 1
  done
  fail "the log of $service does not say '$text'"
}

# The settings both modes share, from the shipped example.
settings() {
  cp "$install/.env.example" "$work/.env"
  if [ -n "$version_from_env" ]; then
    setting HD_VERSION "$version"
  fi
  setting HD_PUBLIC_URL "http://127.0.0.1:$port"
  setting HD_HTTP_PORT "$port"
  setting HD_SUBNET "$subnet"
}

# The service NAME of the configuration that `compose config` prints on standard input, with its
# lines as printed, or nothing when there is no such service.
service() {
  awk -v name="$1" '
    /^[^ ]/ { services = ($0 == "services:"); inside = 0; next }
    services && /^  [^ ]/ { inside = ($0 == "  " name ":") }
    inside { print }
  '
}

# Reads /api/health from the core's published port, and expects it to answer that the database
# is up: the core runs a query for it, with the password.
health() {
  local status=""
  for _ in $(seq 120); do
    if status=$(curl -sS -o "$work/health.json" -w '%{http_code}' \
      "http://127.0.0.1:$port/api/health" 2> "$work/curl.txt") && [ "$status" = 200 ]; then
      break
    fi
    sleep 1
  done
  [ "$status" = 200 ] || fail "/api/health answered '$status': $(cat "$work/curl.txt")"
  grep -qF '"status":"ok"' "$work/health.json" \
    || fail "/api/health did not say ok: $(cat "$work/health.json")"
  ok "/api/health answered 200 $(cat "$work/health.json")"
}

# Expects none of the secret VALUES given as arguments in what Compose and the engine show of the
# install: its configuration (`compose config`) and everything `inspect` says of each of its
# containers, their environment included. A value found is named by its position, not printed.
no_secret_shown() {
  hd_compose config > "$work/shown.txt"
  local containers
  containers=$(hd_compose ps -aq)
  [ -n "$containers" ] || fail "the install has no container to inspect"
  local id
  for id in $containers; do
    "$engine" inspect "$id" >> "$work/shown.txt"
  done
  local count index=0 value
  count=$(echo "$containers" | wc -w)
  for value in "$@"; do
    index=$((index + 1))
    [ -n "$value" ] || fail "secret $index is empty: the check would find nothing"
    if grep -qF -- "$value" "$work/shown.txt"; then
      fail "secret $index of $# is shown by compose config or by inspect"
    fi
  done
  ok "no secret's value is shown by compose config or by inspect (secrets: $#, containers: $count)"
}

# The certificates the test makes for itself: an authority of its own, and a certificate it signs
# for each host name, in $work/tls.
tls="$work/tls"

# Makes the authority NAME (authority unless given), once: $tls/NAME.pem. The certificates below
# are signed by the one named authority.
authority() {
  local name=${1:-authority}
  [ -f "$tls/$name.pem" ] && return 0
  mkdir -p "$tls"
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -days 1 \
    -subj "/CN=The test $name of the help desk" \
    -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign \
    -keyout "$tls/$name.key" -out "$tls/$name.pem" 2> "$tls/openssl.txt" \
    || fail "openssl made no $name: $(cat "$tls/openssl.txt")"
}

# Makes a certificate for the host NAME, signed by the authority: $tls/NAME.pem and NAME.key.
certificate() {
  local name=$1
  authority
  printf '%s\n' "subjectAltName=DNS:$name" "basicConstraints=CA:FALSE" \
    "keyUsage=critical,digitalSignature" "extendedKeyUsage=serverAuth" > "$tls/$name.ext"
  openssl req -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes -subj "/CN=$name" \
    -keyout "$tls/$name.key" -out "$tls/$name.csr" 2> "$tls/openssl.txt" \
    || fail "openssl made no key for $name: $(cat "$tls/openssl.txt")"
  openssl x509 -req -in "$tls/$name.csr" -CA "$tls/authority.pem" -CAkey "$tls/authority.key" \
    -CAcreateserial -days 1 -extfile "$tls/$name.ext" -out "$tls/$name.pem" \
    2> "$tls/openssl.txt" || fail "openssl made no certificate for $name: $(cat "$tls/openssl.txt")"
}

# ---------------------------------------------------------------------------------------------
step "Tools"
"${compose_command[@]}" version
curl --version | head -n 1
for image in hd-core hd-assistant; do
  "$engine" image inspect "ghcr.io/manustars/$image:$version" > /dev/null \
    || fail "the engine holds no ghcr.io/manustars/$image:$version: build it first (see the usage)"
  ok "the engine holds ghcr.io/manustars/$image:$version"
done

# ---------------------------------------------------------------------------------------------
step "1. The configuration, every profile on"
holds "$compose_dir/secrets" "the shipped secrets/" ".gitignore README.md"
ok "secrets/ ships with its README and .gitignore alone"
cp -R "$compose_dir/." "$install"
chmod 700 "$secrets"
# The repository's compose.yaml reads the images' version from .env (HD_VERSION), which the
# test then sets; a release's archive names its own and reads none, so the test sets none.
version_from_env=
if grep -q HD_VERSION "$install/compose.yaml"; then
  version_from_env=yes
fi
settings
setting COMPOSE_PROFILES db,greenmail,assistant,https
for variable in $optional_secrets; do
  name_secret "$variable"
done
setting HD_DATABASE_CA ./secrets/database_ca.crt
setting HD_MAIL_CA ./secrets/mail_ca.crt
# The secrets as variables, as .env set them before they were files: none may be passed on.
secret_variables="HD_DATABASE_URL HD_DATABASE_PASSWORD $optional_secrets HD_ASSISTANT_TOKEN"
secret_variables="$secret_variables HD_ASSISTANT_EVENT_SECRET POSTGRES_PASSWORD"
for variable in $secret_variables; do
  extra "$variable" "$(password)"
done
hd_compose config > "$work/config.yaml"
service db < "$work/config.yaml" > "$work/db.yaml"
service core < "$work/config.yaml" > "$work/core.yaml"
service assistant < "$work/config.yaml" > "$work/assistant.yaml"
service dumps-schedule < "$work/config.yaml" > "$work/dumps-schedule.yaml"
[ -s "$work/db.yaml" ] || fail "the configuration has no service db: $(cat "$work/config.yaml")"
[ -s "$work/core.yaml" ] || fail "the configuration has no service core: $(cat "$work/config.yaml")"
[ -s "$work/assistant.yaml" ] \
  || fail "the configuration has no service assistant: $(cat "$work/config.yaml")"
[ -s "$work/dumps-schedule.yaml" ] \
  || fail "the configuration has no service dumps-schedule: $(cat "$work/config.yaml")"
for image in core assistant; do
  grep -qxF "    image: ghcr.io/manustars/hd-$image:$version" "$work/$image.yaml" \
    || fail "the install runs $(sed -n 's/^    image: //p' "$work/$image.yaml")," \
      "not hd-$image:$version"
done
if [ -n "$version_from_env" ]; then
  ok "the install runs the images of $version, which HD_VERSION names"
else
  ok "the install runs the images of $version, which its compose.yaml names without HD_VERSION"
fi
# The same reading finds the core's port, so that finding none for the database means something.
grep -qE "^ +published: \"?$port\"?$" "$work/core.yaml" \
  || fail "the core is not published on the port $port: $(cat "$work/core.yaml")"
if grep -qE '^    (ports|network_mode):' "$work/db.yaml"; then
  fail "the bundled PostgreSQL is reachable from the host: $(cat "$work/db.yaml")"
fi
ok "the bundled PostgreSQL publishes no port on the host, and the core publishes $port"
grep -A 2 -E '^      db:$' "$work/core.yaml" | grep -qE '^        condition: service_healthy$' \
  || fail "the core does not wait for the bundled PostgreSQL to be healthy: $(cat "$work/core.yaml")"
ok "the core waits for the bundled PostgreSQL to be healthy"

given core.yaml HD_DATABASE_PASSWORD_FILE /run/secrets/db_password
given core.yaml HD_IMAP_URL_FILE /run/secrets/imap_url
given core.yaml HD_SMTP_URL_FILE /run/secrets/smtp_url
given core.yaml HD_OIDC_CLIENT_SECRET_FILE /run/secrets/oidc_client_secret
given db.yaml POSTGRES_PASSWORD_FILE /run/secrets/db_password
given assistant.yaml HD_ASSISTANT_TOKEN_FILE /run/secrets/assistant_token
given assistant.yaml HD_ASSISTANT_EVENT_SECRET_FILE /run/secrets/assistant_event_secret
given assistant.yaml HD_ASSISTANT_API_KEY_FILE /run/secrets/assistant_api_key
ok "every secret is given to its services as a file, through its _FILE variable"
given core.yaml HD_DATABASE_CA_FILE /run/secrets/database_ca
given core.yaml HD_MAIL_CA_FILE /run/secrets/mail_ca
given dumps-schedule.yaml HD_DATABASE_CA_FILE /run/secrets/database_ca
ok "the authorities' certificates that .env names are given to the core and the dumps as files"
for variable in $secret_variables; do
  if grep -qE "^      $variable:" "$work/config.yaml"; then
    fail "a service is given $variable as a variable"
  fi
done
ok "no secret is given as a variable, even one that .env sets"

# Without the optional secrets named in .env, no service is told of them.
settings
setting COMPOSE_PROFILES db,greenmail,assistant,https
hd_compose config > "$work/config.yaml"
if grep -E "^      (${optional_secrets// /|})_FILE:" "$work/config.yaml"; then
  fail "an optional secret that .env does not name is given"
fi
ok "an optional secret that .env does not name is given to no service"
if grep -E "^      HD_(DATABASE|MAIL)_CA_FILE:" "$work/config.yaml"; then
  fail "an authority's certificate that .env does not name is given"
fi
ok "without an authority's certificate named in .env, no service is given one"

# ---------------------------------------------------------------------------------------------
step "2. Start with the profile db and no password in .env: one is written to secrets/"
settings
if grep -iE '^[^#]*password' "$work/.env"; then
  fail ".env sets a password"
fi
holds "$secrets" "secrets/" ".gitignore README.md"
db_password_command "secrets/db_password written"
[ -f "$secrets/db_password" ] && [ ! -L "$secrets/db_password" ] \
  || fail "secrets/db_password is not a regular file: $(ls -la "$secrets")"
db_password=$(cat "$secrets/db_password")
[ "${#db_password}" -ge 32 ] || fail "the password written is ${#db_password} characters long"
# Readable inside the containers, whose users are not the host's, and belonging to the user that
# runs Compose, as the directory does.
[ "$(stat -c %a "$secrets/db_password")" = 644 ] \
  || fail "secrets/db_password has the mode $(stat -c %a "$secrets/db_password"), not 644"
ids=$(stat -c %u:%g "$secrets/db_password")
[ "$ids" = "$(stat -c %u:%g "$secrets")" ] \
  || fail "secrets/db_password belongs to $ids, not to the user and group of the directory"
ok "secrets/db_password written: ${#db_password} characters, mode 644, the directory's user's"
hd_compose up -d
health
holds "$secrets" "the started install's secrets/" ".gitignore README.md db_password"
ok "started with secrets/db_password the only secret"
db_takes || fail "the bundled PostgreSQL refuses the password of secrets/db_password"
if db_takes not-the-password; then
  fail "the bundled PostgreSQL takes a password other than that of secrets/db_password"
fi
ok "the bundled PostgreSQL takes the password of secrets/db_password, and refuses another"
hd_compose down
hd_compose up -d
health
[ "$(cat "$secrets/db_password")" = "$db_password" ] \
  || fail "secrets/db_password changed across down and up -d"
ok "after down and up -d the core reaches the database, secrets/db_password unchanged"
db_password_command "secrets/db_password is there: kept"
[ "$(cat "$secrets/db_password")" = "$db_password" ] \
  || fail "running db-password again changed secrets/db_password"
ok "running db-password again keeps secrets/db_password"

# ---------------------------------------------------------------------------------------------
step "3. Add the profiles greenmail and assistant beside db, their secrets in files"
imap_url="imap://helpdesk:helpdesk@127.0.0.1:3143/INBOX"
smtp_url="smtp://127.0.0.1:3025"
secret imap_url "$imap_url"
secret smtp_url "$smtp_url"
setting COMPOSE_PROFILES db,greenmail
name_secret HD_IMAP_URL
name_secret HD_SMTP_URL
setting HD_MAIL_FROM helpdesk@helpdesk.test
setting HD_GREENMAIL_SMTP_PORT "$smtp_port"
setting HD_GREENMAIL_IMAP_PORT "$imap_port"
# Not settings of an install: the messages already in the mailbox are read too, every two
# seconds, so that the test's message is read whenever it arrives.
extra HD_IMAP_IMPORT_EXISTING true
extra HD_IMAP_POLL_SECONDS 2
hd_compose up -d
health
cat > "$work/message.eml" <<'MAIL'
From: Ada Lindqvist <ada@customer.example.com>
To: helpdesk@helpdesk.test
Subject: The printer on the second floor jams

It has jammed since this morning.
MAIL
# GreenMail takes a few seconds to listen once started.
sent=""
for _ in $(seq 60); do
  if curl -sS "smtp://127.0.0.1:$smtp_port" --mail-from ada@customer.example.com \
    --mail-rcpt helpdesk@helpdesk.test -T "$work/message.eml" 2> "$work/curl.txt"; then
    sent=yes
    break
  fi
  sleep 1
done
[ -n "$sent" ] || fail "the test mail server took no message: $(cat "$work/curl.txt")"
expect_log core "read new mail"
ok "a message sent to the test mail server's port reached the core"

# Once the core answers, so that the Assistant's first question to it is answered: the
# Assistant asks again only minutes later.
hd_compose exec -T core hd addon add assistant --url http://assistant:8090 \
  --event message.received --event ticket.opened \
  --scope directory:read --scope tickets:read --scope proposals:write > "$work/addon.txt"
token=$(sed -n 's/^token: //p' "$work/addon.txt")
event_secret=$(sed -n 's/^event secret: //p' "$work/addon.txt")
[ -n "$token" ] && [ -n "$event_secret" ] \
  || fail "hd addon add printed no token and event secret: $(cat "$work/addon.txt")"
ok "the Assistant is registered on the core"
secret assistant_token "$token"
secret assistant_event_secret "$event_secret"
# A model service on the Assistant's own machine, which takes a key when one is given: the test
# sends it nothing.
api_key=$(password)
secret assistant_api_key "$api_key"
setting COMPOSE_PROFILES db,greenmail,assistant
setting HD_ASSISTANT_MODEL_PROVIDER ollama
setting HD_ASSISTANT_MODEL test-model
name_secret HD_ASSISTANT_API_KEY
core_before=$(hd_compose ps -q core)
hd_compose up -d
[ "$(hd_compose ps -q core)" = "$core_before" ] \
  || fail "adding the Assistant's settings recreated the core"
expect_log assistant "connected to the core"
ok "the Assistant reached the core with its token, and the core kept running"
no_secret_shown "$db_password" "$imap_url" "$smtp_url" "$token" "$event_secret" "$api_key"
hd_compose down -v
ok "stopped, with its volume removed"

# ---------------------------------------------------------------------------------------------
step "4. Start without the profile db: the core on a PostgreSQL of the test's own"
admin_password=$(password)
owner_password=$(password)
external_started=yes
"$engine" network create "$external_network" > /dev/null
"$engine" run -d --name "$external_db" --network "$external_network" \
  -e POSTGRES_PASSWORD="$admin_password" docker.io/library/postgres:18.7-trixie > /dev/null
for _ in $(seq 60); do
  if "$engine" exec "$external_db" pg_isready -h 127.0.0.1 -U postgres > /dev/null 2>&1; then
    break
  fi
  sleep 1
done
# What the DBA creates, as docs/external-database.md gives it: a user that is not a superuser,
# owning a database of its own.
external_sql postgres postgres "CREATE ROLE helpdesk LOGIN PASSWORD '$owner_password'" > /dev/null
external_sql postgres postgres \
  "CREATE DATABASE helpdesk OWNER helpdesk ENCODING 'UTF8' TEMPLATE template0" > /dev/null
external_sql postgres postgres "REVOKE ALL ON DATABASE helpdesk FROM PUBLIC" > /dev/null
ok "the database helpdesk, owned by the user helpdesk, at $external_db"

cat > "$work/external.yaml" <<YAML
services:
  core:
    networks: [default, external]
networks:
  external:
    name: $external_network
    external: true
YAML
# A fresh install's secrets/, with the existing database's password alone.
find "$secrets" -mindepth 1 ! -name README.md ! -name .gitignore -delete
secret db_password "$owner_password"
settings
setting COMPOSE_PROFILES ""
setting HD_DATABASE_HOST "$external_db"
setting HD_DATABASE_PORT 5432
setting HD_DATABASE_NAME helpdesk
setting HD_DATABASE_USER helpdesk
# The test's PostgreSQL serves no TLS; a real one is verified (verify-full, the default).
setting HD_DATABASE_SSLMODE disable
hd_compose up -d
started=$("$engine" ps -a --filter "label=com.docker.compose.project=$project" \
  --filter "label=com.docker.compose.service=db" --format '{{.Names}}')
[ -z "$started" ] || fail "the bundled PostgreSQL was started: $started"
ok "the bundled PostgreSQL was not started"
health
migrations=$(external_sql helpdesk helpdesk "SELECT count(*) FROM _sqlx_migrations")
[ "$migrations" -gt 0 ] || fail "the core applied no migration to the test's own PostgreSQL"
ok "the core applied its $migrations migrations to the test's own PostgreSQL, as the user" \
  "whose password is secrets/db_password"
no_secret_shown "$owner_password"
hd_compose down -v
ok "stopped"

# ---------------------------------------------------------------------------------------------
# What steps 5 to 7 share: the dumps, and the core's API.

# The name of a dump, as the dumps give it.
dump_name='hd-[0-9]{8}T[0-9]{6}Z\.dump'

# Takes a dump now, with the command that deploy/compose/dumps.md gives, and prints the name of
# the dump it says it took.
dump_now() {
  hd_compose run --rm dumps > "$work/dump.txt" 2>&1 \
    || fail "docker compose run --rm dumps failed: $(cat "$work/dump.txt")"
  local name
  name=$(tr -d '\r' < "$work/dump.txt" | sed -nE "s/^dumped ($dump_name); kept:$/\1/p")
  [ -n "$name" ] || fail "docker compose run --rm dumps named no dump: $(cat "$work/dump.txt")"
  echo "$name"
}

# Runs COMMAND in a container of the dumps, its output on standard output, as
# deploy/compose/dumps.md lists the dumps and copies them off the host.
in_dumps() {
  hd_compose run --rm -T dumps "$@"
}

# The files on the dumps' volume, hidden ones included, separated by spaces, in order.
dumps_on_volume() {
  in_dumps ls -1A /dumps | tr -d '\r' | LC_ALL=C sort | tr '\n' ' ' | sed 's/ $//'
}

# Expects the dump NAME on the volume to be complete, pg_restore reading it to its end, and to
# hold TEXT in its list of contents.
dump_holds() {
  local name=$1 text=$2
  in_dumps pg_restore --file=/dev/null "/dumps/$name" > "$work/read.txt" 2>&1 \
    || fail "pg_restore cannot read $name to its end: $(cat "$work/read.txt")"
  in_dumps pg_restore --list "/dumps/$name" > "$work/list.txt" 2>&1 \
    || fail "pg_restore cannot list $name: $(cat "$work/list.txt")"
  grep -qF -- "$text" "$work/list.txt" \
    || fail "$name does not hold '$text': $(cat "$work/list.txt")"
}

# Expects the service SERVICE to be running.
expect_running() {
  [ -n "$(hd_compose ps -q "$1")" ] || fail "$1 is not running: $(hd_compose ps -a)"
}

# Expects the service SERVICE to have no container at all, even one that has ended, as the
# engine lists the containers of the project.
expect_no_container() {
  local found
  found=$("$engine" ps -a --filter "label=com.docker.compose.project=$project" \
    --filter "label=com.docker.compose.service=$1" --format '{{.Names}}')
  [ -z "$found" ] || fail "$1 was started: $found"
}

# The schedule's log.
schedule_log() {
  hd_compose logs --no-color dumps-schedule 2> /dev/null | tr -d '\r'
}

# Moves the schedule's dump to the next minute but one by the engine's clock, which may not be
# the test's (a virtual machine's), so that it comes within a minute and a quarter: sets
# HD_DUMPS_TIME, and `up -d` recreates the schedule alone. Expects the schedule to say so, and
# tries again with a later minute when it started too late for today's. Leaves the time in
# dump_time.
schedule_soon() {
  local day
  for _ in 1 2 3; do
    read -r day dump_time < <(hd_compose exec -T dumps-schedule date -u -d '+75 sec' '+%F %H:%M' \
      | tr -d '\r')
    [[ $dump_time =~ ^[0-9]{2}:[0-9]{2}$ ]] || fail "the engine's clock said '$day $dump_time'"
    setting HD_DUMPS_TIME "$dump_time"
    hd_compose up -d > /dev/null 2>&1 || fail "up -d failed: $(hd_compose ps -a)"
    expect_log dumps-schedule "next dump at "
    if schedule_log | grep -qF "next dump at ${day}T$dump_time:00Z"; then
      return 0
    fi
  done
  fail "the schedule is not at $dump_time today: $(schedule_log)"
}

# Waits for the schedule to take its dump at dump_time, and prints the dump's name.
scheduled_dump() {
  local name
  for _ in $(seq 150); do
    name=$(schedule_log | sed -nE "s/.*dumped ($dump_name); kept:$/\1/p" | tail -n 1)
    if [ -n "$name" ]; then
      echo "$name"
      return 0
    fi
    sleep 1
  done
  fail "the schedule took no dump at $dump_time: $(schedule_log)"
}

# Signs in to the core's API as EMAIL with PASSWORD and prints the session's token. The cookie
# that carries it is Secure, which curl does not send back over http, so it is read from the
# headers and sent as one.
sign_in() {
  local email=$1 password=$2 status
  status=$(curl -sS -o "$work/answer.json" -D "$work/headers.txt" -w '%{http_code}' \
    -H 'Content-Type: application/json' \
    --data "{\"email\":\"$email\",\"password\":\"$password\"}" \
    "http://127.0.0.1:$port/api/session") || fail "signing in as $email failed"
  [ "$status" = 204 ] || fail "signing in as $email answered $status: $(cat "$work/answer.json")"
  tr -d '\r' < "$work/headers.txt" | sed -n 's/^[Ss]et-[Cc]ookie: hd_session=\([^;]*\).*/\1/p'
}

# Calls the core's API with METHOD on PATH, with the JSON BODY when given, signed in with the
# token TOKEN; the answer is in $work/answer.json. Expects the status EXPECTED.
api() {
  local token=$1 method=$2 path=$3 expected=$4 body=${5:-} status
  local arguments=(-sS -o "$work/answer.json" -w '%{http_code}' -X "$method"
    -H "Cookie: hd_session=$token")
  if [ -n "$body" ]; then
    arguments+=(-H 'Content-Type: application/json' --data "$body")
  fi
  status=$(curl "${arguments[@]}" "http://127.0.0.1:$port$path") || fail "$method $path failed"
  [ "$status" = "$expected" ] \
    || fail "$method $path answered $status, not $expected: $(cat "$work/answer.json")"
}

# Expects the agent of step 5 to sign in with the password of then, and to read the customer
# added then back through the API.
customer_reads_back() {
  local session
  session=$(sign_in ada.agent@example.com "$agent_password")
  [ -n "$session" ] || fail "signing in set no session cookie: $(cat "$work/headers.txt")"
  api "$session" GET "/api/customers/$customer" 200
  grep -qF '"name":"Brightfield Lighting"' "$work/answer.json" \
    || fail "the customer reads back as $(cat "$work/answer.json")"
}

# The services that have containers, separated by spaces, in order.
services_started() {
  hd_compose ps -a --format '{{.Service}}' | tr -d '\r' | LC_ALL=C sort | tr '\n' ' ' \
    | sed 's/ $//'
}

# A fresh install's secrets/, with nothing but what it ships.
fresh_secrets() {
  find "$secrets" -mindepth 1 ! -name README.md ! -name .gitignore -delete
}

# ---------------------------------------------------------------------------------------------
step "5. The dumps with the profile db: on a schedule, and one now with run --rm dumps"
rm -f "$work/external.yaml"
fresh_secrets
settings
setting HD_DUMPS_KEEP 2
db_password_command "secrets/db_password written"
db_password=$(cat "$secrets/db_password")
hd_compose up -d
health
expect_running dumps-schedule
expect_no_container dumps
expect_log dumps-schedule "next dump at "
schedule_log | grep -qE "next dump at [0-9-]{10}T02:30:00Z" \
  || fail "the schedule is not at 02:30 by default: $(schedule_log)"
ok "up started the schedule, its next dump at 02:30 UTC by default, and not the one-shot dumps"
schedule_soon
ok "with HD_DUMPS_TIME=$dump_time, the schedule's next dump is at $dump_time UTC today"

# What a help desk writes: an agent added with the core's command, a customer through the API.
hd_compose exec -T core hd agent add ada.agent@example.com --name "Ada Agent" > "$work/agent.txt"
agent_password=$(sed -n 's/^password: //p' "$work/agent.txt" | tr -d '\r')
[ -n "$agent_password" ] || fail "hd agent add printed no password: $(cat "$work/agent.txt")"
session=$(sign_in ada.agent@example.com "$agent_password")
[ -n "$session" ] || fail "signing in set no session cookie: $(cat "$work/headers.txt")"
api "$session" POST /api/customers/all 201 \
  '{"name":"Brightfield Lighting","domains":["brightfield.example.com"]}'
customer=$(sed -n 's/.*"id":"\([0-9a-f-]*\)".*/\1/p' "$work/answer.json")
[ -n "$customer" ] || fail "the new customer has no id: $(cat "$work/answer.json")"
customer_reads_back
ok "the customer Brightfield Lighting added through the API, by the agent added with hd agent add"

scheduled=$(scheduled_dump)
ok "the schedule took $scheduled at $dump_time UTC"

first=$(dump_now)
second=$(dump_now)
third=$(dump_now)
[ "$first" != "$second" ] && [ "$second" != "$third" ] \
  || fail "two dumps have the same name: $first $second $third"
holds_now=$(dumps_on_volume)
[ "$holds_now" = "$second $third" ] \
  || fail "the volume holds '$holds_now', not the latest two dumps '$second $third'"
ok "three dumps now, $first, $second and $third: the volume holds the latest two and nothing else"
dump_holds "$third" "TABLE DATA public customers"
ok "$third is complete, and holds the customers"
in_dumps cat "/dumps/$third" > "$work/$third"
inside=$(in_dumps sha256sum "/dumps/$third" | tr -d '\r' | cut -d ' ' -f 1)
outside=$(sha256sum "$work/$third" | cut -d ' ' -f 1)
[ "$inside" = "$outside" ] \
  || fail "$third copied out of the volume is not the same: $outside, $inside"
ok "$third copied out of the volume as it is ($(wc -c < "$work/$third") bytes)"
no_secret_shown "$db_password"
# The one-shot dumps' container, which `run --rm` removes when it ends, kept once to be inspected:
# it is the schedule's, but for its command.
kept="$project-dumps-kept"
hd_compose run --name "$kept" dumps true > "$work/kept.txt" 2>&1 \
  || fail "docker compose run dumps true failed: $(cat "$work/kept.txt")"
"$engine" inspect "$kept" > "$work/kept.txt"
"$engine" rm -f "$kept" > /dev/null
if grep -qF -- "$db_password" "$work/kept.txt"; then
  fail "inspect of the container of docker compose run dumps shows the database's password"
fi
grep -qF /run/secrets/db_password "$work/kept.txt" \
  || fail "the container of docker compose run dumps does not mount secrets/db_password"
ok "inspect of the container of docker compose run dumps does not show the database's password"

# ---------------------------------------------------------------------------------------------
step "6. A fresh install restored from that dump: the same help desk"
hd_compose down -v
left=$("$engine" volume ls -q --filter "label=com.docker.compose.project=$project")
[ -z "$left" ] || fail "down -v left the volumes $left"
fresh_secrets
db_password_command "secrets/db_password written"
[ "$(cat "$secrets/db_password")" != "$db_password" ] \
  || fail "the fresh install has the same password"
ok "a fresh install: no volume, and a new secrets/db_password"
# The commands of deploy/compose/dumps.md, run in the install's directory with the dump there.
hd_compose run --rm -T dumps restore < "$work/$third" > "$work/restore.txt" 2>&1 \
  || fail "the restore failed: $(cat "$work/restore.txt")"
grep -qF "restored" "$work/restore.txt" || fail "the restore said: $(cat "$work/restore.txt")"
[ "$(services_started)" = db ] || fail "the restore started $(services_started), not db alone"
ok "restored from $third, with db alone started: before the core's first start"
hd_compose up -d
health
customer_reads_back
ok "the agent signs in with the password of before and reads Brightfield Lighting back"

# Starting over once the core has started, as deploy/compose/dumps.md says: the database's
# volume removed alone, and the dumps kept.
before_over=$(dump_now)
hd_compose down
"$engine" volume rm "${project}_db-data" > /dev/null
hd_compose run --rm -T dumps restore < "$work/$third" > "$work/restore.txt" 2>&1 \
  || fail "the restore after starting over failed: $(cat "$work/restore.txt")"
[[ " $(dumps_on_volume) " == *" $before_over "* ]] \
  || fail "starting over removed the dump $before_over: $(dumps_on_volume)"
hd_compose up -d
health
customer_reads_back
ok "started over with the database's volume removed: restored again, $before_over kept"
hd_compose down -v
ok "stopped, with its volumes removed"

# ---------------------------------------------------------------------------------------------
step "7. The dumps without the profile db: of the test's own PostgreSQL, once turned on"
cat > "$work/external.yaml" <<YAML
services:
  core:
    networks: [default, external]
  dumps:
    networks: [default, external]
  dumps-schedule:
    networks: [default, external]
networks:
  external:
    name: $external_network
    external: true
YAML
fresh_secrets
secret db_password "$owner_password"
settings
setting COMPOSE_PROFILES ""
setting HD_DATABASE_HOST "$external_db"
setting HD_DATABASE_PORT 5432
setting HD_DATABASE_NAME helpdesk
setting HD_DATABASE_USER helpdesk
setting HD_DATABASE_SSLMODE disable
hd_compose up -d
health
expect_no_container dumps-schedule
ok "without the profiles db and dumps, up starts no schedule"
setting COMPOSE_PROFILES dumps
core_before=$(hd_compose ps -q core)
hd_compose up -d
[ "$(hd_compose ps -q core)" = "$core_before" ] || fail "turning the dumps on recreated the core"
expect_running dumps-schedule
expect_no_container dumps
expect_no_container db
expect_log dumps-schedule "next dump at "
ok "with the profile dumps, up starts the schedule, and neither the one-shot dumps nor the" \
  "bundled PostgreSQL"
schedule_soon
existing=$(dump_now)
dump_holds "$existing" "TABLE public _sqlx_migrations"
ok "run --rm dumps took $existing of the test's own PostgreSQL, with the core's migrations"
no_secret_shown "$owner_password"
scheduled=$(scheduled_dump)
dump_holds "$scheduled" "TABLE public _sqlx_migrations"
ok "the schedule took $scheduled of the test's own PostgreSQL at $dump_time UTC"
# A stop finds the schedule waiting: it ends at once, not after its stop_grace_period.
stop_start=$SECONDS
hd_compose stop dumps-schedule > /dev/null 2>&1
[ $((SECONDS - stop_start)) -lt 15 ] \
  || fail "the schedule took $((SECONDS - stop_start)) seconds to stop"
ok "the schedule stopped at once, in $((SECONDS - stop_start)) s, not after its grace period"
# Without HD_DATABASE_SSLMODE, TLS is required: the test's PostgreSQL, which serves none, is
# refused, where verify-full would have stopped first for want of an authority.
grep -v '^HD_DATABASE_SSLMODE=' "$work/.env" > "$work/.env.new"
mv "$work/.env.new" "$work/.env"
if hd_compose run --rm dumps > "$work/dump.txt" 2>&1; then
  fail "a dump without HD_DATABASE_SSLMODE reached a server without TLS: $(cat "$work/dump.txt")"
fi
grep -qF "SSL was required" "$work/dump.txt" \
  || fail "a dump without HD_DATABASE_SSLMODE did not fail for TLS: $(cat "$work/dump.txt")"
grep -qF "the server's certificate is not verified" "$work/dump.txt" \
  || fail "the dumps did not warn that the certificate is not verified: $(cat "$work/dump.txt")"
ok "without HD_DATABASE_SSLMODE the dumps require TLS, and warn that they do not verify it"
hd_compose down -v
ok "stopped"

# ---------------------------------------------------------------------------------------------
step "8. Without the profile db, over TLS: the test's own PostgreSQL, from an authority of its own"
# The test's own PostgreSQL serves TLS from now on, with a certificate from the test's authority
# for the name the core dials, given to the server as scripts/tls-test-database.sh gives it: the
# key through standard input, so that no bind mount hands it to the server's user.
certificate "$external_db"
"$engine" exec "$external_db" mkdir -p /etc/hd-tls
for file in pem key; do
  "$engine" exec -i "$external_db" sh -c "cat > /etc/hd-tls/server.$file" \
    < "$tls/$external_db.$file"
done
"$engine" exec "$external_db" sh -c \
  'chown postgres:postgres /etc/hd-tls/server.* && chmod 0600 /etc/hd-tls/server.key'
external_sql postgres postgres "ALTER SYSTEM SET ssl_cert_file = '/etc/hd-tls/server.pem'" \
  > /dev/null
external_sql postgres postgres "ALTER SYSTEM SET ssl_key_file = '/etc/hd-tls/server.key'" \
  > /dev/null
external_sql postgres postgres "ALTER SYSTEM SET ssl = on" > /dev/null
external_sql postgres postgres "SELECT pg_reload_conf()" > /dev/null
# A reload that cannot read the key leaves TLS off: a connection must get it.
tls_on=""
for _ in $(seq 10); do
  if [ "$("$engine" exec "$external_db" psql -tA \
    "host=127.0.0.1 user=postgres dbname=postgres sslmode=require" \
    -c "SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()" 2> /dev/null | tr -d '\r')" \
    = t ]; then
    tls_on=yes
    break
  fi
  sleep 1
done
[ -n "$tls_on" ] || fail "the test's own PostgreSQL does not serve TLS"
ok "the test's own PostgreSQL serves TLS, with a certificate from the test's authority for" \
  "$external_db"

# The settings of step 7, without HD_DATABASE_SSLMODE: verify-full, the default for a database
# on another host.
fresh_secrets
secret db_password "$owner_password"
settings
setting COMPOSE_PROFILES ""
setting HD_DATABASE_HOST "$external_db"
setting HD_DATABASE_PORT 5432
setting HD_DATABASE_NAME helpdesk
setting HD_DATABASE_USER helpdesk
grep -v '^HD_DATABASE_SSLMODE=' "$work/.env" > "$work/.env.new"
mv "$work/.env.new" "$work/.env"
hd_compose up -d
expect_log core "UnknownIssuer"
ok "without the authority's certificate, the core refuses the database's (UnknownIssuer)"

# The authority's certificate in a file of secrets/, named in .env, as .env.example shows.
cp "$tls/authority.pem" "$secrets/database_ca.crt"
chmod 644 "$secrets/database_ca.crt"
setting HD_DATABASE_CA ./secrets/database_ca.crt
hd_compose up -d
health
connections=$(external_sql postgres postgres "SELECT count(*) FILTER (WHERE s.ssl) || ' ' ||
  count(*) FROM pg_stat_activity a JOIN pg_stat_ssl s USING (pid) WHERE a.usename = 'helpdesk'" \
  | tr -d '\r')
read -r encrypted all <<< "$connections"
[ "$all" -gt 0 ] && [ "$encrypted" = "$all" ] \
  || fail "of the core's $all connections to the database, $encrypted are over TLS"
ok "with the authority's certificate named in .env, the core verifies the database's" \
  "(verify-full): its $all connections are over TLS"

existing=$(dump_now)
if grep -qF "not verified" "$work/dump.txt"; then
  fail "the dumps did not verify the database's certificate: $(cat "$work/dump.txt")"
fi
dump_holds "$existing" "TABLE public _sqlx_migrations"
ok "run --rm dumps verified the database's certificate against that authority and took $existing"
# Another authority's certificate in its place: the dumps refuse the database's.
authority other-authority
cp "$tls/other-authority.pem" "$secrets/database_ca.crt"
if hd_compose run --rm dumps > "$work/dump.txt" 2>&1; then
  fail "a dump trusting another authority reached the database: $(cat "$work/dump.txt")"
fi
grep -qF "certificate verify failed" "$work/dump.txt" \
  || fail "a dump trusting another authority did not fail for it: $(cat "$work/dump.txt")"
ok "with another authority's certificate in that file, the dumps refuse the database's"
no_secret_shown "$owner_password"
hd_compose down -v
ok "stopped"

# ---------------------------------------------------------------------------------------------
# Behind a reverse proxy (deploy/compose/reverse-proxy.md): steps 9 to 12.

# The SHA-256 fingerprint of the certificate on standard input.
fingerprint() {
  openssl x509 -noout -fingerprint -sha256 | sed 's/.*=//'
}

# curl to the host NAME at PORT on this machine, over HTTPS, trusting only the authority whose
# certificate is in the file AUTHORITY; the other arguments are curl's. Built on Schannel (curl
# for Windows), curl would also ask for the revocation status that the test's certificates and
# Caddy's own do not give, and refuse them.
curl_tls=()
if curl --version | grep -q Schannel; then
  curl_tls=(--ssl-no-revoke)
fi
https() {
  local name=$1 port=$2 authority=$3
  shift 3
  curl -sS "${curl_tls[@]}" --cacert "$authority" --resolve "$name:$port:127.0.0.1" "$@"
}

# Replaces TEXT with REPLACEMENT in FILE, and fails when FILE does not hold TEXT: the example
# would no longer be the one that the test runs.
replace() {
  local file=$1 text=$2 replacement=$3 content
  content=$(cat "$file")
  [[ $content == *"$text"* ]] || fail "$(basename "$file") holds no '$text'"
  printf '%s\n' "${content//"$text"/"$replacement"}" > "$file"
}

# Runs SQL in the bundled PostgreSQL, as the core's user.
db_sql() {
  # shellcheck disable=SC2016 # expanded inside the container
  hd_compose exec -T db sh -c 'psql -h 127.0.0.1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
    -v ON_ERROR_STOP=1 -tA -c "$1"' sh "$1"
}

# The address that the failed sign-in as ACCOUNT counted under, as the core keeps it.
failure_address() {
  db_sql "SELECT address FROM sign_in_failures WHERE account = '$1'"
}

# A sign-in as ACCOUNT with a wrong password, at BASE (the address of the help desk) with the
# curl arguments that follow.
fail_sign_in() {
  local account=$1 base=$2 status
  shift 2
  status=$("$@" -o "$work/answer.txt" -w '%{http_code}' -H 'Content-Type: application/json' \
    --data "{\"email\":\"$account\",\"password\":\"not the password at all\"}" \
    "$base/api/session") || fail "the sign-in as $account did not reach the core"
  [ "$status" = 401 ] || fail "the wrong password for $account was answered $status, not 401"
}

# The agent the test signs in as, made with `hd agent add` inside the core's container.
agent_email=ines.varga@example.com
add_agent() {
  agent_password=$(password)
  hd_compose exec -T core env HD_AGENT_PASSWORD="$agent_password" \
    hd agent add "$agent_email" --name "Ines Varga" > /dev/null \
    || fail "hd agent add failed"
}

# Signs the agent in at BASE with the curl arguments that follow, and expects the session cookie,
# marked Secure and HttpOnly, and a request that needs it to be answered with the agent.
sign_in_through() {
  local base=$1 status
  shift
  rm -f "$work/cookies.txt"
  status=$("$@" -o "$work/answer.txt" -D "$work/headers.txt" -c "$work/cookies.txt" \
    -w '%{http_code}' -H 'Content-Type: application/json' \
    --data "{\"email\":\"$agent_email\",\"password\":\"$agent_password\"}" \
    "$base/api/session") || fail "the sign-in did not reach the core"
  [ "$status" = 204 ] || fail "the sign-in was answered $status: $(cat "$work/headers.txt")"
  grep -iE '^set-cookie: hd_session=' "$work/headers.txt" | grep -i 'secure' | grep -qi 'httponly' \
    || fail "the sign-in set no Secure, HttpOnly session cookie: $(cat "$work/headers.txt")"
  status=$("$@" -o "$work/answer.txt" -w '%{http_code}' "$base/api/session")
  [ "$status" = 401 ] || fail "GET /api/session without the cookie was answered $status, not 401"
  status=$("$@" -o "$work/session.json" -w '%{http_code}' -b "$work/cookies.txt" \
    "$base/api/session")
  [ "$status" = 200 ] \
    || fail "GET /api/session with the cookie was answered $status: $(cat "$work/session.json")"
  grep -qF "$agent_email" "$work/session.json" \
    || fail "GET /api/session answered with another agent: $(cat "$work/session.json")"
  ok "signed in at $base: the session cookie is set, and GET /api/session answers with the agent"
}

# The test's own nginx, on the host's network as a proxy installed on the host is.
proxy="$project-proxy"

# ---------------------------------------------------------------------------------------------
step "9. Without the profile https: an agent signs in through nginx, a proxy on the host"
# The addition to the install's file for the test's own PostgreSQL (steps 4, 7 and 8) is not
# wanted any more.
rm -f "$work/external.yaml"
fresh_secrets
db_password_command "secrets/db_password written"
settings
setting COMPOSE_PROFILES db
hd_compose config > "$work/config.yaml"
service core < "$work/config.yaml" > "$work/core.yaml"
given core.yaml HD_TRUSTED_PROXIES "$subnet"
grep -qE "^ +- subnet: $subnet$" "$work/config.yaml" \
  || fail "the install's network is not $subnet: $(cat "$work/config.yaml")"
ok "the install's network is HD_SUBNET ($subnet), and the core trusts it as its proxies"
# The same without HD_SUBNET, as the shipped settings leave it: the network's default subnet and
# the core's default trusted proxies are one.
grep -v '^HD_SUBNET=' "$work/.env" > "$work/.env.new"
mv "$work/.env.new" "$work/.env"
hd_compose config > "$work/config.yaml"
default_subnet=$(sed -n 's/^ *- subnet: //p' "$work/config.yaml")
service core < "$work/config.yaml" > "$work/core.yaml"
[ -n "$default_subnet" ] || fail "the install's network has no subnet: $(cat "$work/config.yaml")"
given core.yaml HD_TRUSTED_PROXIES "$default_subnet"
ok "without HD_SUBNET the network is $default_subnet, which the core trusts as its proxies"

# An install made before HD_SUBNET, whose compose.yaml had no networks: its network is the one
# the engine chose. The new file must move the install to HD_SUBNET, with `down` and `up -d` as
# reverse-proxy.md gives it, or the core would trust a network its proxy does not connect from.
settings
setting COMPOSE_PROFILES db
cp "$install/compose.yaml" "$work/compose.yaml"
awk '/^networks:/ { skip = 1; next } skip && /^[^ ]/ { skip = 0 } !skip { print }' \
  "$work/compose.yaml" > "$install/compose.yaml"
if grep -q "^networks:" "$install/compose.yaml"; then
  fail "the install's file still names its network"
fi
hd_compose up -d
health
cp "$work/compose.yaml" "$install/compose.yaml"
# Not what reverse-proxy.md says, only noted: whether `up -d` alone, which makes the network
# anew, gives the containers back their service names on it (Docker and Podman give them none).
hd_compose up -d
named=$(hd_compose exec -T db getent hosts db 2> /dev/null | awk '{ print $1 }') || true
if [ "${named%.*}" = "${gateway%.*}" ]; then
  echo "    note: up -d alone moved the install to HD_SUBNET, the name db included"
else
  echo "    note: after up -d alone the name db is '${named:-not found}', not in HD_SUBNET"
fi
hd_compose down
hd_compose up -d
health
ok "the core answers on the port that .env sets (HD_HTTP_PORT=$port)"
started=$(hd_compose ps -a --format '{{.Service}}')
if grep -qx caddy <<< "$started"; then
  fail "Caddy was started without the profile https"
fi
ok "Caddy was not started"
add_agent

# What the core sees of a connection to its published port, as a proxy on the host makes one.
fail_sign_in direct@example.com "http://127.0.0.1:$port" curl -sS
peer=$(failure_address direct@example.com)
[ "$peer" = "$gateway" ] \
  || fail "a connection to the core's published port comes from '$peer', not from $gateway"
ok "the core sees connections to its published port come from $gateway, the gateway of" \
  "HD_SUBNET, to which down and up -d moved the install made before HD_SUBNET"

# The documented example, with the test's ports in place of 80, 443 and the core's 8080, and a
# certificate of the test's for its host name at the paths it names: all read from the example,
# so that the test runs whatever it holds.
cp "$install/examples/nginx.conf" "$work/nginx.conf"
replace "$work/nginx.conf" "listen 80;" "listen $nginx_http_port;"
replace "$work/nginx.conf" "listen 443 ssl;" "listen $nginx_https_port ssl;"
replace "$work/nginx.conf" "proxy_pass http://127.0.0.1:8080;" \
  "proxy_pass http://127.0.0.1:$port;"
# The first value of the nginx DIRECTIVE in the example.
directive() {
  awk -v name="$1" '$1 == name { sub(/;$/, "", $2); print $2; exit }' "$work/nginx.conf"
}
host=$(directive server_name)
certificate_path=$(directive ssl_certificate)
key_path=$(directive ssl_certificate_key)
[ -n "$host" ] && [ -n "$certificate_path" ] && [ -n "$key_path" ] \
  || fail "examples/nginx.conf names no server_name, ssl_certificate and ssl_certificate_key"
certificate "$host"
proxy_started=yes
"$engine" run -d --name "$proxy" --network host \
  -v "$work/nginx.conf:/etc/nginx/conf.d/default.conf:ro" \
  -v "$tls/$host.pem:$certificate_path:ro" -v "$tls/$host.key:$key_path:ro" \
  docker.io/library/nginx:1.30.8-alpine > /dev/null
base="https://$host:$nginx_https_port"
through_nginx=(https "$host" "$nginx_https_port" "$tls/authority.pem")
status=""
for _ in $(seq 30); do
  status=$("${through_nginx[@]}" -o "$work/health.json" -w '%{http_code}' \
    "$base/api/health" 2> "$work/curl.txt") && [ "$status" = 200 ] && break
  sleep 1
done
[ "$status" = 200 ] || fail "/api/health through nginx answered '$status': $(cat "$work/curl.txt")"
ok "/api/health answers over HTTPS through nginx, with the example's configuration for $host"
sign_in_through "$base" "${through_nginx[@]}"

# A failed sign-in through nginx, as ACCOUNT, with an address the client makes up, which only a
# trusted proxy could vouch for: the address it counts under.
through_nginx_address() {
  fail_sign_in "$1" "$base" "${through_nginx[@]}" -H 'X-Forwarded-For: 203.0.113.7'
  failure_address "$1"
}
address=$(through_nginx_address through-nginx@example.com)
[ "$address" = 127.0.0.1 ] \
  || fail "a sign-in through nginx counted under '$address', not under 127.0.0.1, nginx's client"
ok "a failed sign-in through nginx counts under the address nginx saw it from (127.0.0.1)," \
  "not under nginx's ($gateway) nor the one the client made up"

# HD_TRUSTED_PROXIES set in .env: the gateway alone, then empty, which trusts no proxy. Only the
# core is recreated.
setting HD_TRUSTED_PROXIES "$gateway"
hd_compose up -d
health
address=$(through_nginx_address trusting-the-gateway@example.com)
[ "$address" = 127.0.0.1 ] \
  || fail "with HD_TRUSTED_PROXIES=$gateway a sign-in through nginx counted under '$address'"
ok "with HD_TRUSTED_PROXIES=$gateway in .env, it counts under 127.0.0.1 too"
setting HD_TRUSTED_PROXIES ""
hd_compose up -d
health
address=$(through_nginx_address trusting-none@example.com)
[ "$address" = "$gateway" ] \
  || fail "with HD_TRUSTED_PROXIES empty a sign-in through nginx counted under '$address'"
ok "with HD_TRUSTED_PROXIES empty in .env, no proxy is trusted: it counts under nginx's" \
  "address ($gateway)"
"$engine" rm -f "$proxy" > /dev/null
proxy_started=""
hd_compose down -v
ok "stopped"

# The settings of the profile https, with the certificate MODE, for the host NAME on the test's
# ports.
https_settings() {
  local mode=$1 name=$2
  settings
  setting COMPOSE_PROFILES db,https
  setting HD_PUBLIC_URL "https://$name:$caddy_https_port"
  setting HD_HTTPS_CERTIFICATE "$mode"
  setting HD_CADDY_HTTP_PORT "$caddy_http_port"
  setting HD_CADDY_HTTPS_PORT "$caddy_https_port"
}

# Waits up to a minute for /api/health to answer 200 over HTTPS through Caddy, at the host NAME,
# trusting the authority in the file AUTHORITY.
health_through_caddy() {
  local name=$1 authority=$2 status=""
  for _ in $(seq 60); do
    status=$(https "$name" "$caddy_https_port" "$authority" -o "$work/health.json" \
      -w '%{http_code}' "https://$name:$caddy_https_port/api/health" 2> "$work/curl.txt") \
      && [ "$status" = 200 ] && break
    sleep 1
  done
  [ "$status" = 200 ] \
    || fail "/api/health through Caddy answered '$status': $(cat "$work/curl.txt")"
}

# ---------------------------------------------------------------------------------------------
step "10. The profile https with the certificate internal: Caddy's own authority"
fresh_secrets
db_password_command "secrets/db_password written"
https_settings internal helpdesk.test
hd_compose up -d
health
# As reverse-proxy.md gives it, once Caddy has made its authority.
caddy_root=""
for _ in $(seq 30); do
  if hd_compose exec -T caddy cat /data/caddy/pki/authorities/local/root.crt \
    > "$work/caddy-root.crt" 2> /dev/null \
    && grep -q 'BEGIN CERTIFICATE' "$work/caddy-root.crt"; then
    caddy_root=yes
    break
  fi
  sleep 1
done
[ -n "$caddy_root" ] || fail "Caddy's root certificate could not be read"
ok "Caddy's root certificate read from its container"
base="https://helpdesk.test:$caddy_https_port"
health_through_caddy helpdesk.test "$work/caddy-root.crt"
ok "/api/health answers over HTTPS through Caddy, verified against Caddy's root certificate"
status=$(curl -sS -o "$work/answer.txt" -w '%{http_code} %{redirect_url}' \
  --resolve "helpdesk.test:$caddy_http_port:127.0.0.1" "http://helpdesk.test:$caddy_http_port/")
# To the HTTPS port only when it is 443: Caddy leaves the port it listens on out of the address.
[ "$status" = "308 https://helpdesk.test/" ] || fail "plain HTTP to Caddy was answered '$status'"
ok "plain HTTP to Caddy is sent to https://helpdesk.test/"
add_agent
through_caddy=(https helpdesk.test "$caddy_https_port" "$work/caddy-root.crt")
sign_in_through "$base" "${through_caddy[@]}"
fail_sign_in through-caddy@example.com "$base" "${through_caddy[@]}" \
  -H 'X-Forwarded-For: 203.0.113.7'
address=$(failure_address through-caddy@example.com)
caddy_address=$("$engine" inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' \
  "$(hd_compose ps -q caddy)")
# The test's curl reaches Caddy's published port from the gateway, as it reaches the core's.
[ "$address" = "$gateway" ] \
  || fail "a sign-in through Caddy counted under '$address', not under $gateway, Caddy's client"
ok "a failed sign-in through Caddy counts under the address Caddy saw it from ($gateway)," \
  "not under Caddy's ($caddy_address) nor the one the client made up"

# ---------------------------------------------------------------------------------------------
step "11. The same install with the certificate files: one of the test's own, in secrets/"
certificate helpdesk.test
secret tls_certificate "$(cat "$tls/helpdesk.test.pem")"
secret tls_key "$(cat "$tls/helpdesk.test.key")"
https_settings files helpdesk.test
setting HD_SECRET_TLS_CERTIFICATE ./secrets/tls_certificate
setting HD_SECRET_TLS_KEY ./secrets/tls_key
hd_compose up -d
health_through_caddy helpdesk.test "$tls/authority.pem"
ok "/api/health answers over HTTPS through Caddy, verified against the test's authority"
served=$(openssl s_client -connect "127.0.0.1:$caddy_https_port" -servername helpdesk.test \
  < /dev/null 2> /dev/null | fingerprint)
made=$(fingerprint < "$tls/helpdesk.test.pem")
[ -n "$made" ] && [ "$served" = "$made" ] \
  || fail "Caddy serves the certificate '$served', not the test's '$made'"
ok "Caddy serves the test's certificate (SHA-256 $made)"
hd_compose down -v
ok "stopped, with its volumes removed"

# ---------------------------------------------------------------------------------------------
step "12. The certificate acme, read by Caddy without starting it: Let's Encrypt is not asked"
settings
setting COMPOSE_PROFILES https
setting HD_PUBLIC_URL https://helpdesk.example.com
setting HD_HTTPS_CERTIFICATE acme
# Caddy only turns the install's Caddyfile, with the install's settings, into its configuration.
hd_compose run --rm --no-deps -T caddy caddy adapt --config /etc/caddy/Caddyfile \
  > "$work/caddy.json" 2> "$work/adapt.txt" \
  || fail "caddy adapt refused the Caddyfile: $(cat "$work/adapt.txt")"
# Each key and value as Caddy writes it, whatever the order of the keys around it.
for expected in '"listen":[":443"]' '"host":["helpdesk.example.com"]' '"http_port":80' \
  '"subjects":["helpdesk.example.com"]' '"dial":"core:8080"' '"protocols":["h1","h2"]'; do
  grep -qF "$expected" "$work/caddy.json" \
    || fail "Caddy's configuration holds no $expected: $(cat "$work/caddy.json")"
done
# The modules that issue certificates, and nothing else, have a "module" key here: ACME alone,
# with no authority of its own ("ca"), so Let's Encrypt; no certificate is loaded from files.
modules=$(grep -oE '"module":"[^"]*"' "$work/caddy.json" | LC_ALL=C sort -u | tr '\n' ' ')
[ "$modules" = '"module":"acme" ' ] \
  || fail "Caddy's certificates come from $modules, not from ACME alone: $(cat "$work/caddy.json")"
for unexpected in '"ca":' '"load_files":'; do
  if grep -qF "$unexpected" "$work/caddy.json"; then
    fail "Caddy's configuration holds $unexpected: $(cat "$work/caddy.json")"
  fi
done
ok "Caddy would serve helpdesk.example.com on 443 with a certificate from Let's Encrypt" \
  "(ACME, its default authority), and pass requests on to the core"
hd_compose down -v

echo
echo "All checks passed."
