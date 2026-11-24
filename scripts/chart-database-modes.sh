#!/usr/bin/env bash
# Checks what the Helm chart renders for each database mode (postgresql.mode, ADR 0009): the
# core's database environment, the CloudNativePG resources, the dumps, and the values the chart
# refuses, each with its exit status and its message. It also lints the chart in each mode.
#
# Usage:
#   bash scripts/chart-database-modes.sh
#
# Needs `helm` only: nothing is installed and no cluster is contacted. It prints one line per
# case and exits with 1 when any case fails.
#
# Environment: CHART (the chart's directory, deploy/chart).
set -euo pipefail

cd "$(dirname "$0")/.."
chart=${CHART:-deploy/chart}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cases=0
failures=0
problems=""

problem() {
  problems+="    $1"$'\n'
}

# Closes a case: prints its line, and what went wrong in it.
finish() {
  cases=$((cases + 1))
  if [ -z "$problems" ]; then
    echo "ok      $1"
  else
    failures=$((failures + 1))
    echo "FAILED  $1"
    printf '%s' "$problems"
  fi
  problems=""
}

# Renders the chart as release `hd` into $work/all.yaml, with the given helm arguments.
render() {
  if ! helm template hd "$chart" "$@" > "$work/raw.yaml" 2> "$work/error.txt"; then
    problem "helm template failed: $(tr '\n' ' ' < "$work/error.txt")"
    : > "$work/all.yaml"
    return
  fi
  tr -d '\r' < "$work/raw.yaml" > "$work/all.yaml"
}

# The rendered documents of one template, such as `template core.yaml`.
template() {
  awk -v source="# Source: hd/templates/$1" '
    /^# Source: / { wanted = ($0 == source) }
    wanted { print }
  ' "$work/all.yaml"
}

# The list entries of a rendered template that have a name (environment variables, volumes,
# volume mounts), each with all it holds on one line: `value: "5432"`, or
# `valueFrom: secretKeyRef: name: hd-db-app key: uri`.
entries_of() {
  template "$1" | awk -v name="$2" '
    function indent(line) { match(line, /^ */); return RLENGTH }
    /^ *#/ { next }
    found && (indent($0) < depth || (indent($0) == depth && $0 ~ /^ *- /)) { print entry; found = 0 }
    found { sub(/^ +/, ""); entry = entry (entry == "" ? "" : " ") $0; next }
    $0 ~ ("^ *- name: " name "$") { found = 1; depth = indent($0); entry = "" }
    END { if (found) print entry }
  '
}

# One environment variable of a rendered template. Empty when the variable is not set.
env_of() {
  entries_of "$1" "$2" | sed -n 1p
}

# expect_entry TEMPLATE NAME ENTRY: an entry of that name holds exactly this, and nothing more.
expect_entry() {
  local entries
  entries=$(entries_of "$1" "$2")
  if ! grep -qFx -- "$3" <<< "$entries"; then
    problem "$1: no $2 with exactly '$3', but: $(tr '\n' '|' <<< "$entries")"
  fi
}

# expect_line TEMPLATE LINE: a whole line of what a template rendered, indentation included.
expect_line() {
  local rendered
  rendered=$(template "$1")
  if ! grep -qFx -- "$2" <<< "$rendered"; then
    problem "$1: no line '$2'"
  fi
}

# expect_env TEMPLATE NAME ENTRY: the variable is set exactly so.
expect_env() {
  local entry
  entry=$(env_of "$1" "$2")
  if [ "$entry" != "$3" ]; then
    problem "$1: $2 is '${entry:-not set}', expected '$3'"
  fi
}

expect_no_env() {
  local entry
  entry=$(env_of "$1" "$2")
  if [ -n "$entry" ]; then
    problem "$1: $2 is '$entry', expected it not to be set"
  fi
}

# expect_text TEMPLATE TEXT / expect_no_text TEMPLATE TEXT: a fixed string in what a template
# rendered.
expect_text() {
  local rendered
  rendered=$(template "$1")
  if ! grep -qF -- "$2" <<< "$rendered"; then
    problem "$1: no '$2'"
  fi
}

expect_no_text() {
  local rendered
  rendered=$(template "$1")
  if grep -qF -- "$2" <<< "$rendered"; then
    problem "$1: unexpected '$2'"
  fi
}

# expect_kind KIND COUNT: how many resources of a kind the whole chart rendered.
expect_kind() {
  local count
  count=$(grep -c "^kind: $1\$" "$work/all.yaml" || true)
  if [ "$count" != "$2" ]; then
    problem "$count resources of kind $1, expected $2"
  fi
}

# The core verifies the CloudNativePG cluster's certificate against the cluster's own authority,
# mounted without its key.
expect_cluster_database() {
  expect_env core.yaml HD_DATABASE_URL 'valueFrom: secretKeyRef: name: hd-db-app key: uri'
  expect_env core.yaml HD_DATABASE_SSLMODE 'value: "verify-full"'
  expect_env core.yaml HD_DATABASE_CA_FILE 'value: "/etc/hd/database-ca/ca.crt"'
  expect_entry core.yaml database-ca 'mountPath: /etc/hd/database-ca readOnly: true'
  # The Secret also holds the authority's key: the volume takes the certificate and nothing else.
  expect_entry core.yaml database-ca \
    'secret: secretName: hd-db-ca items: - key: ca.crt path: ca.crt'
  local variable
  for variable in HOST PORT NAME USER PASSWORD; do
    expect_no_env core.yaml "HD_DATABASE_$variable"
  done
  expect_kind Cluster 1
  expect_line postgresql.yaml '  name: hd-db'
}

# The dumps of the CloudNativePG cluster get its URL and nothing else about the connection.
expect_cluster_dumps() {
  expect_kind CronJob 1
  expect_env dumps.yaml DATABASE_URL 'valueFrom: secretKeyRef: name: hd-db-app key: uri'
  expect_text dumps.yaml 'pg_dump --dbname="$DATABASE_URL" --format=custom'
  local variable
  for variable in PGHOST PGPORT PGDATABASE PGUSER PGPASSWORD PGSSLMODE PGSSLROOTCERT; do
    expect_no_env dumps.yaml "$variable"
  done
  expect_no_text dumps.yaml 'database-ca'
}

# dumps_tls FORM HELM_ARGUMENTS…: pg_dump reaches an external database, given in that form, over
# TLS as the core does, as far as the dumps image can: it holds no public authorities, so it
# verifies the server's certificate only against the authority of core.database.ca.
dumps_tls() {
  local form=$1
  shift

  render "$@" --set dumps.enabled=true
  expect_env dumps.yaml PGSSLMODE 'value: "require"'
  expect_no_env dumps.yaml PGSSLROOTCERT
  expect_no_text dumps.yaml 'database-ca'
  finish "dumps with $form, no authority: TLS required, the certificate not verified"

  render "$@" --set dumps.enabled=true --set core.database.sslMode=verify-ca
  expect_env dumps.yaml PGSSLMODE 'value: "require"'
  expect_no_env dumps.yaml PGSSLROOTCERT
  finish "dumps with $form, sslMode=verify-ca and no authority: TLS required"

  render "$@" --set dumps.enabled=true --set core.database.sslMode=prefer
  expect_env dumps.yaml PGSSLMODE 'value: "prefer"'
  expect_no_env dumps.yaml PGSSLROOTCERT
  finish "dumps with $form, sslMode=prefer: the weaker mode, as set"

  render "$@" --set dumps.enabled=true --set core.database.ca.existingSecret=hd-database-ca
  expect_env dumps.yaml PGSSLMODE 'value: "verify-full"'
  expect_env dumps.yaml PGSSLROOTCERT 'value: "/etc/hd/database-ca/ca.crt"'
  expect_entry dumps.yaml database-ca 'mountPath: /etc/hd/database-ca readOnly: true'
  expect_entry dumps.yaml database-ca \
    'secret: secretName: hd-database-ca items: - key: ca.crt path: ca.crt'
  finish "dumps with $form and an authority: verify-full against it, mounted alone"

  render "$@" --set dumps.enabled=true --set core.database.sslMode=verify-ca \
    --set core.database.ca.existingSecret=hd-database-ca --set core.database.ca.secretKey=root.pem
  expect_env dumps.yaml PGSSLMODE 'value: "verify-ca"'
  expect_env dumps.yaml PGSSLROOTCERT 'value: "/etc/hd/database-ca/ca.crt"'
  expect_entry dumps.yaml database-ca \
    'secret: secretName: hd-database-ca items: - key: root.pem path: ca.crt'
  finish "dumps with $form, sslMode=verify-ca and an authority: verify-ca against its key"
}

# refused NAME MESSAGE HELM_ARGUMENTS…: the chart does not render, and says why.
refused() {
  local name=$1 message=$2 output
  shift 2
  if output=$(helm template hd "$chart" "$@" 2>&1); then
    problem "helm template succeeded, expected it to fail"
  elif ! grep -qF -- "$message" <<< "$output"; then
    problem "the message is not '$message' but: $(tr '\r\n' '  ' <<< "$output")"
  fi
  finish "$name"
}

# lint NAME HELM_ARGUMENTS…
lint() {
  local name=$1 output
  shift
  if ! output=$(helm lint "$chart" "$@" 2>&1); then
    problem "helm lint failed: $(tr '\r\n' '  ' <<< "$output")"
  fi
  finish "$name"
}

# ---------------------------------------------------------------------------------------------
# cloudnativepg
# ---------------------------------------------------------------------------------------------

render
expect_cluster_database
expect_kind ScheduledBackup 0
expect_kind CronJob 0
finish "default values: the CloudNativePG cluster, verified against its own authority"

cluster=(--set postgresql.mode=cloudnativepg)

render "${cluster[@]}"
expect_cluster_database
finish "postgresql.mode=cloudnativepg: the same as the default"

refused "cloudnativepg with core.database.existingSecret: refused, it belongs to external" \
  'core.database.existingSecret is for postgresql.mode=external: with postgresql.mode=cloudnativepg the core uses the Secret of the CloudNativePG cluster' \
  "${cluster[@]}" --set core.database.existingSecret=hd-database

refused "cloudnativepg with core.database.sslMode: refused, it belongs to external" \
  'core.database.sslMode and core.database.ca are for postgresql.mode=external: the CloudNativePG cluster is verified against its own authority' \
  "${cluster[@]}" --set core.database.sslMode=require

refused "cloudnativepg with core.database.ca: refused, it belongs to external" \
  'core.database.sslMode and core.database.ca are for postgresql.mode=external: the CloudNativePG cluster is verified against its own authority' \
  "${cluster[@]}" --set core.database.ca.existingSecret=hd-database-ca

refused "cloudnativepg with postgresql.external: refused, it belongs to external" \
  'postgresql.external is for postgresql.mode=external, and postgresql.mode is cloudnativepg' \
  "${cluster[@]}" --set postgresql.external.host=db.example.com

# ---------------------------------------------------------------------------------------------
# external, as a Secret holding the URL
# ---------------------------------------------------------------------------------------------

url_form=(--set postgresql.mode=external --set core.database.existingSecret=hd-database)

expect_no_cluster() {
  expect_kind Cluster 0
  expect_kind ScheduledBackup 0
  expect_no_text core.yaml 'hd-db-app'
  expect_no_text core.yaml 'hd-db-ca'
}

render "${url_form[@]}"
expect_env core.yaml HD_DATABASE_URL 'valueFrom: secretKeyRef: name: hd-database key: uri'
for variable in HOST PORT NAME USER PASSWORD SSLMODE CA_FILE; do
  expect_no_env core.yaml "HD_DATABASE_$variable"
done
expect_no_text core.yaml 'database-ca'
expect_no_cluster
finish "external with a URL Secret: the URL from the named Secret, no cluster"

render "${url_form[@]}" --set core.database.secretKey=url \
  --set core.database.sslMode=verify-ca \
  --set core.database.ca.existingSecret=hd-database-ca --set core.database.ca.secretKey=root.pem
expect_env core.yaml HD_DATABASE_URL 'valueFrom: secretKeyRef: name: hd-database key: url'
expect_env core.yaml HD_DATABASE_SSLMODE 'value: "verify-ca"'
expect_env core.yaml HD_DATABASE_CA_FILE 'value: "/etc/hd/database-ca/ca.crt"'
expect_entry core.yaml database-ca 'mountPath: /etc/hd/database-ca readOnly: true'
expect_entry core.yaml database-ca \
  'secret: secretName: hd-database-ca items: - key: root.pem path: ca.crt'
expect_no_cluster
finish "external with a URL Secret: its key, sslMode and the authority's Secret applied"

refused "external with no database given: refused, naming both forms" \
  'postgresql.mode=external needs the database: set core.database.existingSecret (a Secret holding the URL), or postgresql.external.host, name, user and password.existingSecret' \
  --set postgresql.mode=external

# ---------------------------------------------------------------------------------------------
# external, as separate values with the password from a Secret
# ---------------------------------------------------------------------------------------------

separate_values=(--set postgresql.mode=external --set postgresql.external.host=db.example.com
  --set postgresql.external.name=helpdesk --set postgresql.external.user=helpdesk
  --set postgresql.external.password.existingSecret=hd-database-password)

render "${separate_values[@]}"
expect_env core.yaml HD_DATABASE_HOST 'value: "db.example.com"'
expect_env core.yaml HD_DATABASE_PORT 'value: "5432"'
expect_env core.yaml HD_DATABASE_NAME 'value: "helpdesk"'
expect_env core.yaml HD_DATABASE_USER 'value: "helpdesk"'
expect_env core.yaml HD_DATABASE_PASSWORD \
  'valueFrom: secretKeyRef: name: hd-database-password key: password'
for variable in URL SSLMODE CA_FILE; do
  expect_no_env core.yaml "HD_DATABASE_$variable"
done
expect_no_text core.yaml 'database-ca'
expect_no_cluster
finish "external with separate values: the five parameters, the password from its Secret, no URL"

render "${separate_values[@]}" --set postgresql.external.port=6432 \
  --set postgresql.external.password.secretKey=db-password \
  --set core.database.sslMode=verify-ca \
  --set core.database.ca.existingSecret=hd-database-ca --set core.database.ca.secretKey=root.pem
expect_env core.yaml HD_DATABASE_PORT 'value: "6432"'
expect_env core.yaml HD_DATABASE_PASSWORD \
  'valueFrom: secretKeyRef: name: hd-database-password key: db-password'
expect_no_env core.yaml HD_DATABASE_URL
expect_env core.yaml HD_DATABASE_SSLMODE 'value: "verify-ca"'
expect_env core.yaml HD_DATABASE_CA_FILE 'value: "/etc/hd/database-ca/ca.crt"'
expect_entry core.yaml database-ca 'mountPath: /etc/hd/database-ca readOnly: true'
expect_entry core.yaml database-ca \
  'secret: secretName: hd-database-ca items: - key: root.pem path: ca.crt'
expect_no_cluster
finish "external with separate values: the port, the password's key, sslMode and the authority's Secret applied"

refused "external with both forms: refused, naming both" \
  'postgresql.mode=external takes the database as a Secret holding the URL (core.database.existingSecret) or as separate values (postgresql.external), not both' \
  "${separate_values[@]}" --set core.database.existingSecret=hd-database

refused "external with a URL Secret and one separate value: refused, naming both" \
  'postgresql.mode=external takes the database as a Secret holding the URL (core.database.existingSecret) or as separate values (postgresql.external), not both' \
  "${url_form[@]}" --set postgresql.external.host=db.example.com

refused "external with separate values in part: refused, naming the missing ones" \
  'postgresql.external is given in part: set also postgresql.external.name, postgresql.external.password.existingSecret' \
  --set postgresql.mode=external --set postgresql.external.host=db.example.com \
  --set postgresql.external.user=helpdesk

refused "external with the password's Secret alone: refused, naming the missing ones" \
  'postgresql.external is given in part: set also postgresql.external.host, postgresql.external.name, postgresql.external.user' \
  --set postgresql.mode=external \
  --set postgresql.external.password.existingSecret=hd-database-password

# ---------------------------------------------------------------------------------------------
# Dumps: pg_dump reaches the same database as the core
# ---------------------------------------------------------------------------------------------

render "${cluster[@]}" --set dumps.enabled=true
expect_cluster_dumps
finish "dumps with cloudnativepg: the URL from the cluster's Secret, and nothing else"

render "${url_form[@]}" --set core.database.secretKey=url --set dumps.enabled=true
expect_kind CronJob 1
expect_env dumps.yaml DATABASE_URL 'valueFrom: secretKeyRef: name: hd-database key: url'
expect_text dumps.yaml 'pg_dump --dbname="$DATABASE_URL" --format=custom'
for variable in PGHOST PGPORT PGDATABASE PGUSER PGPASSWORD; do
  expect_no_env dumps.yaml "$variable"
done
finish "dumps with external and a URL Secret: the URL from the named Secret"

render "${separate_values[@]}" --set postgresql.external.port=6432 --set dumps.enabled=true
expect_kind CronJob 1
expect_env dumps.yaml PGHOST 'value: "db.example.com"'
expect_env dumps.yaml PGPORT 'value: "6432"'
expect_env dumps.yaml PGDATABASE 'value: "helpdesk"'
expect_env dumps.yaml PGUSER 'value: "helpdesk"'
expect_env dumps.yaml PGPASSWORD 'valueFrom: secretKeyRef: name: hd-database-password key: password'
expect_text dumps.yaml 'pg_dump --format=custom'
# The script runs with `set -u`: a reference to a variable that is not set would stop it.
expect_no_text dumps.yaml 'DATABASE_URL'
finish "dumps with external and separate values: pg_dump's own variables, the password from its Secret"

dumps_tls "external and a URL Secret" "${url_form[@]}"
dumps_tls "external and separate values" "${separate_values[@]}"

# ---------------------------------------------------------------------------------------------
# The staging example, and helm lint in each mode
# ---------------------------------------------------------------------------------------------

render -f "$chart/values-staging.example.yaml"
expect_cluster_database
expect_cluster_dumps
finish "the staging example: the CloudNativePG cluster and its dumps"

render "${cluster[@]}" --set postgresql.backup.enabled=true \
  --set postgresql.backup.volumeSnapshot.className=snapshots
expect_cluster_database
expect_kind ScheduledBackup 1
expect_text postgresql.yaml 'className: snapshots'
finish "cloudnativepg with backups: a ScheduledBackup of the cluster"

lint "helm lint: default values"
lint "helm lint: the staging example" -f "$chart/values-staging.example.yaml"
lint "helm lint: external with a URL Secret" "${url_form[@]}" --set dumps.enabled=true
lint "helm lint: external with separate values" "${separate_values[@]}" --set dumps.enabled=true

# ---------------------------------------------------------------------------------------------
# Modes the chart does not have
# ---------------------------------------------------------------------------------------------

refused "postgresql.mode=standalone: refused, not available yet" \
  'postgresql.mode=standalone is not available yet: set postgresql.mode to cloudnativepg or external' \
  --set postgresql.mode=standalone

refused "an unknown postgresql.mode: refused, naming the three modes" \
  'postgresql.mode is "operator": it must be standalone, cloudnativepg or external' \
  --set postgresql.mode=operator

refused "an empty postgresql.mode: refused, naming the three modes" \
  'postgresql.mode is "": it must be standalone, cloudnativepg or external' \
  --set postgresql.mode=

# ---------------------------------------------------------------------------------------------
# postgresql.enabled, which postgresql.mode replaces
# ---------------------------------------------------------------------------------------------

refused "postgresql.enabled=true: refused, naming postgresql.mode=cloudnativepg" \
  'postgresql.enabled is no longer a value: set postgresql.mode=cloudnativepg and remove postgresql.enabled' \
  --set postgresql.enabled=true

# Before, the core then used the Secret's database, not the cluster the chart still created.
refused "postgresql.enabled=true with core.database.existingSecret: refused, naming postgresql.mode=external" \
  'postgresql.enabled is no longer a value: set postgresql.mode=external and remove postgresql.enabled (with core.database.existingSecret the core uses that database; the CloudNativePG cluster then leaves the release, and stays in place only with postgresql.keepOnUninstall)' \
  --set postgresql.enabled=true --set core.database.existingSecret=hd-database

refused "postgresql.enabled=false: refused, naming postgresql.mode=external" \
  'postgresql.enabled is no longer a value: set postgresql.mode=external and remove postgresql.enabled' \
  --set postgresql.enabled=false --set core.database.existingSecret=hd-database

echo
if [ "$failures" -gt 0 ]; then
  echo "$failures of $cases cases failed"
  exit 1
fi
echo "$cases cases passed"
