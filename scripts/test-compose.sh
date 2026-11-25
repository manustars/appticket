#!/usr/bin/env bash
# Starts the Compose install (deploy/compose) on this machine and checks it in both of its
# database modes (ADR 0009):
#
#   1. the configuration, every profile on: the bundled PostgreSQL publishes no port on the
#      host, and the core waits for it to be healthy;
#   2. with the profile db: the core starts on the bundled PostgreSQL, reached by its service
#      name, and /api/health answers that the database is up;
#   3. without it: the core starts on a PostgreSQL of the test's own, given to it through the
#      HD_DATABASE_* parameters, with another database, user and password than the bundled
#      one's, and /api/health answers that the database is up.
#
# The install's settings are the shipped .env.example with the test's values put in, written to
# a directory of the test's own and given with --env-file: nothing is written into the Compose
# directory, so a .env there is neither read nor touched.
#
# Usage:
#   bash scripts/test-compose.sh VERSION
#
# VERSION is the tag of ghcr.io/manustars/hd-core that the local engine holds, as the release
# names it (HD_VERSION), such as one built with
#   docker build -f deploy/Dockerfile --target core -t ghcr.io/manustars/hd-core:VERSION .
# The test never pulls it. Needs docker (or podman) with Compose, and curl. It prints a line per
# step and exits with 1 at the first check that fails, after printing the containers' logs.
#
# Environment: COMPOSE_DIR (the Compose directory, deploy/compose), CONTAINER_ENGINE (docker by
# default; podman), COMPOSE (the Compose command, "$CONTAINER_ENGINE compose"), PROJECT (the
# Compose project, hd-test-compose), PORT (a free local port for the core, 55479).
set -euo pipefail

version=${1:?usage: test-compose.sh VERSION}
root=$(cd "$(dirname "$0")/.." && pwd)
compose_dir=${COMPOSE_DIR:-$root/deploy/compose}
engine=${CONTAINER_ENGINE:-docker}
read -r -a compose_command <<< "${COMPOSE:-$engine compose}"
project=${PROJECT:-hd-test-compose}
port=${PORT:-55479}

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
  "${compose_command[@]}" -p "$project" -f "$compose_dir/compose.yaml" \
    --env-file "$work/.env" "$@"
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
    hd_compose --profile db down -v --remove-orphans > /dev/null 2>&1 || true
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

# The settings both modes share, from the shipped example.
settings() {
  cp "$compose_dir/.env.example" "$work/.env"
  setting HD_VERSION "$version"
  setting HD_PUBLIC_URL "http://127.0.0.1:$port"
  setting HD_HTTP_PORT "$port"
}

# Reads /api/health from the core's published port, and expects it to answer that the database
# is up: the core runs a query for it, with the password.
health() {
  local status="" attempt
  for attempt in $(seq 120); do
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

# ---------------------------------------------------------------------------------------------
step "Tools"
"${compose_command[@]}" version
curl --version | head -n 1
"$engine" image inspect "ghcr.io/manustars/hd-core:$version" > /dev/null \
  || fail "the engine holds no ghcr.io/manustars/hd-core:$version: build it first (see the usage)"
ok "the engine holds ghcr.io/manustars/hd-core:$version"

# ---------------------------------------------------------------------------------------------
step "2. Start with the profile db: the core on the bundled PostgreSQL"
settings
setting HD_DATABASE_PASSWORD "$(password)"
hd_compose up -d
health
