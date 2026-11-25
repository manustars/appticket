#!/usr/bin/env bash
# Checks what the Helm chart renders for each database mode (postgresql.mode, ADR 0009): the
# core's database environment, the chart's own PostgreSQL and its NetworkPolicy, the CloudNativePG
# resources, the dumps, the notes, and the values the chart refuses, each with its exit status and
# its message. It also lints the chart in each mode.
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

# notes HELM_ARGUMENTS…: what the chart tells whoever installs it, into $work/notes.txt. A dry
# run on this side only: no cluster is contacted.
notes() {
  if ! helm install hd "$chart" --dry-run=client "$@" > "$work/raw.txt" 2> "$work/error.txt"; then
    problem "helm install --dry-run=client failed: $(tr '\n' ' ' < "$work/error.txt")"
    : > "$work/notes.txt"
    return
  fi
  tr -d '\r' < "$work/raw.txt" | sed -n '/^NOTES:$/,$p' > "$work/notes.txt"
}

expect_note() {
  if ! grep -qF -- "$1" "$work/notes.txt"; then
    problem "the notes do not say '$1'"
  fi
}

expect_no_note() {
  if grep -qF -- "$1" "$work/notes.txt"; then
    problem "the notes say '$1'"
  fi
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
# standalone: PostgreSQL run by the chart itself
# ---------------------------------------------------------------------------------------------

standalone=(--set postgresql.mode=standalone)

# expect_standalone_database SECRET KEY: the core reaches the chart's own PostgreSQL by its
# Service, inside the cluster, without TLS and saying so, with the password from that Secret.
expect_standalone_database() {
  expect_env core.yaml HD_DATABASE_HOST 'value: "hd-postgresql"'
  expect_env core.yaml HD_DATABASE_PORT 'value: "5432"'
  expect_env core.yaml HD_DATABASE_NAME 'value: "hd"'
  expect_env core.yaml HD_DATABASE_USER 'value: "hd"'
  expect_env core.yaml HD_DATABASE_PASSWORD "valueFrom: secretKeyRef: name: $1 key: $2"
  expect_env core.yaml HD_DATABASE_SSLMODE 'value: "disable"'
  expect_no_env core.yaml HD_DATABASE_URL
  expect_no_env core.yaml HD_DATABASE_CA_FILE
  expect_no_text core.yaml 'database-ca'
  expect_no_cluster
}

render "${standalone[@]}"
expect_standalone_database hd-postgresql password
finish "standalone: the core reaches the chart's PostgreSQL without TLS, set explicitly"

# expect_standalone_postgresql SECRET KEY: one instance of the official image at the chart's
# major version, its data on a volume of its own, the password from that Secret.
expect_standalone_postgresql() {
  expect_kind StatefulSet 1
  # The core's Service and the database's.
  expect_kind Service 2
  expect_line standalone.yaml '  name: hd-postgresql'
  expect_line standalone.yaml '  replicas: 1'
  expect_line standalone.yaml '          image: docker.io/library/postgres:18.7-trixie'
  expect_env standalone.yaml POSTGRES_DB 'value: "hd"'
  expect_env standalone.yaml POSTGRES_USER 'value: "hd"'
  expect_env standalone.yaml POSTGRES_PASSWORD "valueFrom: secretKeyRef: name: $1 key: $2"
  # Where the image of PostgreSQL 18 keeps its data: a mount anywhere else loses them.
  expect_entry standalone.yaml data 'mountPath: /var/lib/postgresql'
  expect_text standalone.yaml 'volumeClaimTemplates:'
  expect_line standalone.yaml '    whenDeleted: Retain'
}

render "${standalone[@]}"
expect_standalone_postgresql hd-postgresql password
expect_kind Secret 1
expect_text standalone.yaml 'kind: Secret'
# Kept on uninstall, beside the volume whose data directory accepts no other password.
expect_line standalone.yaml '    helm.sh/resource-policy: keep'
# The password itself changes at every render: only its form is checked, safe wherever it is used.
generated=$(template standalone.yaml | sed -n 's/^  password: "\(.*\)"$/\1/p' | base64 -d)
if ! [[ $generated =~ ^[A-Za-z0-9]{32}$ ]]; then
  problem "standalone.yaml: the generated password is not 32 letters and digits"
fi
finish "standalone: a StatefulSet of PostgreSQL 18 and a generated password in a Secret kept on uninstall"

render "${standalone[@]}" --set postgresql.standalone.existingSecret=hd-own-password \
  --set postgresql.standalone.secretKey=db-password
expect_standalone_postgresql hd-own-password db-password
expect_standalone_database hd-own-password db-password
expect_kind Secret 0
expect_no_text standalone.yaml 'resource-policy'
finish "standalone with existingSecret: the password from the named Secret, none generated"

render "${standalone[@]}" --set postgresql.standalone.storage.size=20Gi \
  --set postgresql.standalone.storage.storageClass=fast \
  --set postgresql.standalone.image.repository=registry.example.com/postgres \
  --set postgresql.standalone.image.tag=18.8
expect_line standalone.yaml '          image: registry.example.com/postgres:18.8'
expect_line standalone.yaml '            storage: 20Gi'
expect_line standalone.yaml '        storageClassName: fast'
finish "standalone: the volume's size and class and a later minor version, as set"

refused "standalone with another major version: refused, naming the chart's" \
  'postgresql.standalone.image.tag is "17.5": the chart runs PostgreSQL 18, and a tag must be of that major version' \
  "${standalone[@]}" --set postgresql.standalone.image.tag=17.5

# A tag that YAML reads as a number would lose its form: 18.10 is the number 18.1.
not_text='must be a string: quote it in a values file ("18.8"), or give it with --set-string'

printf 'postgresql:\n  standalone:\n    image:\n      tag: 18.10\n' > "$work/number-tag.yaml"
refused "standalone with an unquoted tag in a values file: refused, a number is not a tag" \
  "postgresql.standalone.image.tag $not_text" \
  "${standalone[@]}" -f "$work/number-tag.yaml"

printf 'postgresql:\n  standalone:\n    image:\n      tag: 18\n' > "$work/whole-tag.yaml"
refused "standalone with an unquoted whole number as the tag: refused as well" \
  "postgresql.standalone.image.tag $not_text" \
  "${standalone[@]}" -f "$work/whole-tag.yaml"

refused "standalone with a tag that --set reads as a number: refused, naming --set-string" \
  "postgresql.standalone.image.tag $not_text" \
  "${standalone[@]}" --set postgresql.standalone.image.tag=18

printf 'postgresql:\n  standalone:\n    image:\n      tag: "18.10"\n' > "$work/quoted-tag.yaml"
render "${standalone[@]}" -f "$work/quoted-tag.yaml"
expect_line standalone.yaml '          image: docker.io/library/postgres:18.10'
render "${standalone[@]}" --set-string postgresql.standalone.image.tag=18
expect_line standalone.yaml '          image: docker.io/library/postgres:18'
finish "standalone with a quoted tag, or one given with --set-string: used as it is written"

refused "standalone with core.database.existingSecret: refused, it belongs to external" \
  'core.database.existingSecret is for postgresql.mode=external: with postgresql.mode=standalone the core uses the chart'"'"'s own PostgreSQL' \
  "${standalone[@]}" --set core.database.existingSecret=hd-database

refused "standalone with core.database.sslMode: refused, it belongs to external" \
  'core.database.sslMode and core.database.ca are for postgresql.mode=external: the chart'"'"'s own PostgreSQL is reached inside the cluster without TLS' \
  "${standalone[@]}" --set core.database.sslMode=require

refused "standalone with core.database.ca: refused, it belongs to external" \
  'core.database.sslMode and core.database.ca are for postgresql.mode=external: the chart'"'"'s own PostgreSQL is reached inside the cluster without TLS' \
  "${standalone[@]}" --set core.database.ca.existingSecret=hd-database-ca

refused "standalone with postgresql.external: refused, it belongs to external" \
  'postgresql.external is for postgresql.mode=external, and postgresql.mode is standalone' \
  "${standalone[@]}" --set postgresql.external.host=db.example.com

# expect_database_peers COMPONENTS: the NetworkPolicy of the chart's PostgreSQL lets in, on the
# database's port, the pods of exactly these components and nobody else.
expect_database_peers() {
  local policy peers
  policy=$(template networkpolicy.yaml | awk '
    /^---$/ { if (document ~ /\n  name: hd-postgresql\n/) printf "%s", document; document = ""; next }
    { document = document $0 "\n" }
    END { if (document ~ /\n  name: hd-postgresql\n/) printf "%s", document }
  ')
  if [ -z "$policy" ]; then
    problem "networkpolicy.yaml: no NetworkPolicy named hd-postgresql"
    return
  fi
  grep -qFx '      app.kubernetes.io/component: postgresql' <<< "$policy" \
    || problem "networkpolicy.yaml: the database's policy does not select the database's pods"
  grep -qFx '  policyTypes: ["Ingress"]' <<< "$policy" \
    || problem "networkpolicy.yaml: the database's policy is not about ingress"
  grep -qFx '        - port: postgresql' <<< "$policy" \
    || problem "networkpolicy.yaml: the database's policy does not name the database's port"
  # What follows `from:`: one pod selector per component, each with the release's own labels.
  peers=$(sed -n '/^      from:$/,$p' <<< "$policy")
  if [ "$(grep -c -- '- ' <<< "$peers")" != "$(wc -w <<< "$1" | tr -d ' ')" ] \
    || [ "$(grep -c -- '- podSelector:$' <<< "$peers")" != "$(wc -w <<< "$1" | tr -d ' ')" ]; then
    problem "networkpolicy.yaml: the database's peers are not one pod selector each for: $1"
  fi
  if [ "$(sed -n 's/^ *app.kubernetes.io\/component: //p' <<< "$peers" | tr '\n' ' ')" != "$1 " ]; then
    problem "networkpolicy.yaml: the database's peers are not the components: $1"
  fi
}

render "${standalone[@]}" --set networkPolicy.enabled=true
# The core's policy and the database's.
expect_kind NetworkPolicy 2
expect_database_peers "core dumps"
finish "standalone with networkPolicy: only the core and the dumps reach the database"

render "${standalone[@]}" --set networkPolicy.enabled=true --set dumps.enabled=false
expect_kind NetworkPolicy 2
expect_database_peers "core"
finish "standalone with networkPolicy and no dumps: only the core reaches the database"

render "${standalone[@]}"
expect_kind NetworkPolicy 0
render "${cluster[@]}" --set networkPolicy.enabled=true
expect_kind NetworkPolicy 1
expect_no_text networkpolicy.yaml 'postgresql'
finish "no NetworkPolicy for the database without networkPolicy.enabled, or in another mode"

notes "${standalone[@]}"
expect_note 'The database is the chart'"'"'s own PostgreSQL hd-postgresql (postgresql.mode=standalone)'
expect_note 'without TLS'
expect_note 'data-hd-postgresql-0'
expect_note 'Its password was generated into the Secret hd-postgresql'
expect_note 'postgresql.standalone.existingSecret'
expect_note 'Database dumps run on "30 2 * * *", keeping 7.'
expect_no_note 'external'
expect_no_note 'CloudNativePG'
expect_no_note 'do not verify its certificate'
finish "the notes of standalone: its own PostgreSQL, no TLS, what uninstalling keeps, the dumps"

notes "${standalone[@]}" --set postgresql.standalone.existingSecret=hd-own-password \
  --set dumps.enabled=false
expect_note 'Its password is in the Secret hd-own-password'
expect_no_note 'generated'
expect_no_note 'Database dumps run'
finish "the notes of standalone with existingSecret and no dumps: the named Secret, no dumps"

notes "${cluster[@]}"
expect_note 'The database is the CloudNativePG cluster hd-db (postgresql.mode=cloudnativepg)'
expect_no_note 'Database dumps run'
notes "${url_form[@]}" --set dumps.enabled=true
expect_note 'The database is an external one (postgresql.mode=external), given as a Secret holding its URL.'
expect_note 'Database dumps run'
expect_note 'do not verify its certificate'
finish "the notes of cloudnativepg and external: as before"

render "${cluster[@]}"
expect_kind StatefulSet 0
expect_kind Secret 0
render "${url_form[@]}"
expect_kind StatefulSet 0
expect_kind Secret 0
finish "cloudnativepg and external: no PostgreSQL of the chart's own, no Secret"

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

# expect_standalone_dumps SECRET KEY: pg_dump reaches the chart's own PostgreSQL as the core does.
expect_standalone_dumps() {
  expect_kind CronJob 1
  expect_env dumps.yaml PGHOST 'value: "hd-postgresql"'
  expect_env dumps.yaml PGPORT 'value: "5432"'
  expect_env dumps.yaml PGDATABASE 'value: "hd"'
  expect_env dumps.yaml PGUSER 'value: "hd"'
  expect_env dumps.yaml PGPASSWORD "valueFrom: secretKeyRef: name: $1 key: $2"
  expect_env dumps.yaml PGSSLMODE 'value: "disable"'
  expect_no_env dumps.yaml PGSSLROOTCERT
  expect_text dumps.yaml 'pg_dump --format=custom'
  expect_no_text dumps.yaml 'DATABASE_URL'
  expect_no_text dumps.yaml 'database-ca'
}

render "${standalone[@]}"
expect_standalone_dumps hd-postgresql password
finish "dumps with standalone: on by default, reaching the chart's PostgreSQL without TLS"

render "${standalone[@]}" --set postgresql.standalone.existingSecret=hd-own-password \
  --set postgresql.standalone.secretKey=db-password
expect_standalone_dumps hd-own-password db-password
finish "dumps with standalone and existingSecret: the password from the named Secret"

render "${standalone[@]}" --set dumps.enabled=false
expect_kind CronJob 0
expect_kind PersistentVolumeClaim 0
expect_standalone_postgresql hd-postgresql password
finish "dumps with standalone and dumps.enabled=false: off when asked"

render "${standalone[@]}" --set-string dumps.enabled=false
expect_kind CronJob 0
finish "dumps with standalone and dumps.enabled=false as a string: off as well"

render "${standalone[@]}" --set dumps.enabled=true
expect_standalone_dumps hd-postgresql password
finish "dumps with standalone and dumps.enabled=true: on"

render "${cluster[@]}"
expect_kind CronJob 0
render "${url_form[@]}"
expect_kind CronJob 0
render "${separate_values[@]}"
expect_kind CronJob 0
finish "dumps with cloudnativepg and external: off unless asked for"

refused "dumps.enabled that is neither true nor false: refused, naming what it takes" \
  'dumps.enabled is "sometimes": it must be true, false, or empty for on with postgresql.mode=standalone and off otherwise' \
  --set dumps.enabled=sometimes

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
lint "helm lint: standalone" "${standalone[@]}" --set networkPolicy.enabled=true
lint "helm lint: standalone with existingSecret" "${standalone[@]}" \
  --set postgresql.standalone.existingSecret=hd-own-password --set dumps.enabled=false

# ---------------------------------------------------------------------------------------------
# Modes the chart does not have
# ---------------------------------------------------------------------------------------------

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

# How an upgrade that reuses the old values takes postgresql.enabled out of them.
render --set postgresql.enabled=null --set postgresql.mode=cloudnativepg
expect_cluster_database
finish "postgresql.enabled=null beside postgresql.mode: removed, not refused"

echo
if [ "$failures" -gt 0 ]; then
  echo "$failures of $cases cases failed"
  exit 1
fi
echo "$cases cases passed"
