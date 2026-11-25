#!/usr/bin/env bash
# Starts the Compose install (deploy/compose) on this machine and checks it in both of its
# database modes (ADR 0009):
#
#   1. the configuration, every profile on: secrets/ ships with no secret; the bundled
#      PostgreSQL publishes no port on the host, and the core waits for it to be healthy; every
#      secret reaches the services as a file named by its _FILE variable (POSTGRES_PASSWORD_FILE
#      for PostgreSQL), none as a variable, even one set in .env, and an optional secret that
#      .env does not name reaches no service;
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
#   5. the dumps, with the profile db: `up` starts the schedule and not the one-shot dumps,
#      and the schedule takes a dump at the time .env gives; after a customer is added through
#      the core's API, `docker compose run --rm dumps` leaves complete dumps on the volume,
#      the latest HD_DUMPS_KEEP of them and no temporary file, which hold the customers and come
#      out of the volume unchanged; no one is shown the database's password;
#   6. a fresh install, its volumes removed and a new password written, restored from the last
#      of those dumps with the command that deploy/compose/dumps.md gives: the agent signs in
#      with the password of before and reads the customer back through the API;
#   7. the dumps without the profile db: the schedule starts only once the profile dumps is
#      added, and `docker compose run --rm dumps` dumps the test's own PostgreSQL of step 4,
#      over TLS, required but not verified, when .env gives no HD_DATABASE_SSLMODE; no one is
#      shown that database's password.
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
# local engine holds, as the install names it (HD_VERSION), such as images built with
#   docker build -f deploy/Dockerfile --target core      -t ghcr.io/manustars/hd-core:VERSION .
#   docker build -f deploy/Dockerfile --target assistant -t ghcr.io/manustars/hd-assistant:VERSION .
# The test never pulls them; it pulls PostgreSQL, GreenMail and BusyBox. Needs docker (or podman)
# with Compose, curl, and GNU find and stat (as on Linux and in Git Bash). It prints a line per
# step and exits with 1 at the first check that fails, after printing the containers' logs; it
# removes what it started either way.
#
# Environment: COMPOSE_DIR (the Compose directory, deploy/compose; such as an unpacked copy of
# it), CONTAINER_ENGINE (docker by default; podman), COMPOSE (the Compose command,
# "$CONTAINER_ENGINE compose"), PROJECT (the Compose project, hd-test-compose), PORT (a free
# local port for the core, 55479; the two after it are the test mail server's).
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
    hd_compose --profile db --profile assistant --profile greenmail down -v --remove-orphans \
      > /dev/null 2>&1 || true
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
  setting HD_VERSION "$version"
  setting HD_PUBLIC_URL "http://127.0.0.1:$port"
  setting HD_HTTP_PORT "$port"
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
settings
setting COMPOSE_PROFILES db,greenmail,assistant
for variable in $optional_secrets; do
  name_secret "$variable"
done
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
[ -s "$work/db.yaml" ] || fail "the configuration has no service db: $(cat "$work/config.yaml")"
[ -s "$work/core.yaml" ] || fail "the configuration has no service core: $(cat "$work/config.yaml")"
[ -s "$work/assistant.yaml" ] \
  || fail "the configuration has no service assistant: $(cat "$work/config.yaml")"
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
for variable in $secret_variables; do
  if grep -qE "^      $variable:" "$work/config.yaml"; then
    fail "a service is given $variable as a variable"
  fi
done
ok "no secret is given as a variable, even one that .env sets"

# Without the optional secrets named in .env, no service is told of them.
settings
setting COMPOSE_PROFILES db,greenmail,assistant
hd_compose config > "$work/config.yaml"
if grep -E "^      (${optional_secrets// /|})_FILE:" "$work/config.yaml"; then
  fail "an optional secret that .env does not name is given"
fi
ok "an optional secret that .env does not name is given to no service"

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

# Runs COMMAND in a container of the dumps that starts nothing else, its output on standard
# output, as the documentation lists and copies the dumps.
in_dumps() {
  hd_compose run --rm --no-deps -T dumps "$@"
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

# Expects the service SERVICE to be running, or with "not" to have no container at all, even
# one that has ended: as the engine lists the containers of the project.
started() {
  local service=$1 expected=${2:-}
  local found
  if [ "$expected" = not ]; then
    found=$("$engine" ps -a --filter "label=com.docker.compose.project=$project" \
      --filter "label=com.docker.compose.service=$service" --format '{{.Names}}')
    [ -z "$found" ] || fail "$service was started: $found"
  else
    found=$(hd_compose ps -q "$service")
    [ -n "$found" ] || fail "$service is not running: $(hd_compose ps -a)"
  fi
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
# The schedule's time, a minute or two from now by the engine's clock, which may not be the
# test's (a virtual machine's): time for the install to start before the schedule's first dump.
read -r dump_day dump_time < <(in_dumps date -u -d '+100 sec' '+%F %H:%M' | tr -d '\r')
[[ $dump_time =~ ^[0-9]{2}:[0-9]{2}$ ]] || fail "the engine's clock said '$dump_day $dump_time'"
setting HD_DUMPS_TIME "$dump_time"
hd_compose up -d
health
started dumps-schedule
started dumps not
expect_log dumps-schedule "next dump at "
# Today's: a schedule started too late for it would wait for tomorrow's.
hd_compose logs --no-color dumps-schedule | grep -qF "next dump at ${dump_day}T$dump_time:00Z" \
  || fail "the schedule is not at $dump_day $dump_time:" \
    "$(hd_compose logs --no-color dumps-schedule)"
ok "up started the schedule, its next dump at $dump_time UTC, and not the one-shot dumps"

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
api "$session" GET "/api/customers/$customer" 200
grep -qF '"name":"Brightfield Lighting"' "$work/answer.json" \
  || fail "the customer reads back as $(cat "$work/answer.json")"
ok "the customer Brightfield Lighting added through the API, by the agent added with hd agent add"

scheduled=""
for _ in $(seq 240); do
  if hd_compose logs --no-color dumps-schedule 2> /dev/null | grep -qE "^.*dumped $dump_name"; then
    scheduled=yes
    break
  fi
  sleep 1
done
[ -n "$scheduled" ] \
  || fail "the schedule took no dump at $dump_time: $(hd_compose logs --no-color dumps-schedule)"
ok "the schedule took a dump at $dump_time UTC"

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
hd_compose run --name "$kept" --no-deps dumps true > "$work/kept.txt" 2>&1 \
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
# The command of deploy/compose/dumps.md, run in the install's directory with the dump there.
hd_compose run --rm -T dumps restore < "$work/$third" > "$work/restore.txt" 2>&1 \
  || fail "the restore failed: $(cat "$work/restore.txt")"
grep -qF "restored" "$work/restore.txt" || fail "the restore said: $(cat "$work/restore.txt")"
started core not
ok "restored from $third, before the core's first start"
hd_compose up -d
health
session=$(sign_in ada.agent@example.com "$agent_password")
[ -n "$session" ] || fail "signing in set no session cookie: $(cat "$work/headers.txt")"
api "$session" GET "/api/customers/$customer" 200
grep -qF '"name":"Brightfield Lighting"' "$work/answer.json" \
  || fail "after the restore the customer reads back as $(cat "$work/answer.json")"
ok "the agent signs in with the password of before and reads Brightfield Lighting back"
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
started dumps-schedule not
ok "without the profiles db and dumps, up starts no schedule"
setting COMPOSE_PROFILES dumps
core_before=$(hd_compose ps -q core)
hd_compose up -d
[ "$(hd_compose ps -q core)" = "$core_before" ] || fail "turning the dumps on recreated the core"
started dumps-schedule
started dumps not
started db not
expect_log dumps-schedule "next dump at "
ok "with the profile dumps, up starts the schedule, and neither the one-shot dumps nor the" \
  "bundled PostgreSQL"
existing=$(dump_now)
dump_holds "$existing" "TABLE public _sqlx_migrations"
ok "$existing is a complete dump of the test's own PostgreSQL, with the core's migrations"
no_secret_shown "$owner_password"
# Without HD_DATABASE_SSLMODE, TLS is required: the test's PostgreSQL, which serves none, is
# refused, where verify-full would have stopped first for want of an authority.
sed -i '/^HD_DATABASE_SSLMODE=/d' "$work/.env"
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

echo
echo "All checks passed."
