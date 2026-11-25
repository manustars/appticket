#!/usr/bin/env bash
# Checks the chart's upgrade guard (ADR 0009) in a throwaway kind cluster with the CloudNativePG
# operator: the chart refuses a postgresql.mode that is not the mode of the database the release
# already has, so that no upgrade starts the core on another, empty database.
#
#   1. the released chart, whose default was a CloudNativePG cluster, installs with its default
#      values, and a row is written into the cluster's database;
#   2. an upgrade to this chart with its default values, which are now standalone, is refused
#      with the guard's message, and so is one with postgresql.mode=external; the release, the
#      cluster and the row are as they were;
#   3. an upgrade to this chart with postgresql.mode=cloudnativepg goes through: the new core is
#      ready on the same cluster, and the row is there; after `helm uninstall`, which leaves the
#      cluster, an install with the default values is refused as the upgrade was, and one with
#      postgresql.mode=cloudnativepg takes the cluster and the row back;
#   4. this chart installs with its default values, standalone, beside it; an upgrade with
#      postgresql.mode=cloudnativepg or external is refused with the guard's message, and the
#      release, the database and its row are as they were; with a CloudNativePG cluster of the
#      release's name beside it, every mode is refused;
#   5. once the StatefulSet of the chart's own PostgreSQL is deleted, as the message says, the
#      guard lets another mode through, and the volume is still there.
#
# Usage:
#   bash scripts/test-upgrade-guard.sh CORE_IMAGE
#
# CORE_IMAGE is the core's image as the local Docker holds it, such as hd-core:ci, built with
#   docker build -f deploy/Dockerfile --target core -t hd-core:ci .
# It is loaded into the cluster for this chart's core. The released chart runs its own default
# image, which the cluster pulls. Needs kind, kubectl, helm, docker, curl and git, and a clone
# that can fetch the released tag from `origin`.
# It prints a line per step and exits with 1 at the first check that fails, after printing the
# state of the cluster.
#
# Environment: CHART (the chart's directory, deploy/chart), CLUSTER (the kind cluster's name,
# hd-upgrade-guard), PORT (a free local port for reaching the core, 55468), KEEP_CLUSTER (set to
# keep the cluster afterwards; it is deleted otherwise).
set -euo pipefail

core_image=${1:?usage: test-upgrade-guard.sh CORE_IMAGE}
root=$(cd "$(dirname "$0")/.." && pwd)
chart=${CHART:-$root/deploy/chart}
kind_cluster=${CLUSTER:-hd-upgrade-guard}
port=${PORT:-55468}
context="kind-$kind_cluster"

# The last release whose chart had the CloudNativePG cluster as its default: the one this test
# upgrades from. Pinned, and not "the latest tag": every later release defaults to standalone,
# and an upgrade from it would meet no guard.
released=v0.3.1

# The CloudNativePG operator, pinned: the manifest its documentation gives for this version
# (https://cloudnative-pg.io/documentation/1.30/installation_upgrade/).
operator_manifest=https://raw.githubusercontent.com/cloudnative-pg/cloudnative-pg/release-1.30/releases/cnpg-1.30.2.yaml

# The namespace of the release that `k` and `h` work on: the release on the CloudNativePG
# cluster first, the standalone one afterwards.
namespace=helpdesk

work=$(mktemp -d)
forward=""

step() {
  echo
  echo "==> $*"
}

ok() {
  echo "    ok: $*"
}

k() {
  kubectl --context "$context" -n "$namespace" "$@"
}

h() {
  helm --kube-context "$context" -n "$namespace" "$@"
}

# What the cluster holds, for whoever reads the log of a failure.
diagnose() {
  echo
  echo "---- the state of the cluster ----"
  kubectl --context "$context" get pods -A -o wide || true
  local each
  for each in helpdesk helpdesk-standalone; do
    echo "---- namespace $each ----"
    helm --kube-context "$context" -n "$each" list || true
    kubectl --context "$context" -n "$each" get all,persistentvolumeclaims,secrets -o wide || true
    kubectl --context "$context" -n "$each" get clusters.postgresql.cnpg.io -o wide || true
    kubectl --context "$context" -n "$each" describe pods || true
    kubectl --context "$context" -n "$each" get events --sort-by=.lastTimestamp || true
    echo "---- the core's log ($each) ----"
    kubectl --context "$context" -n "$each" logs deploy/hd-core --tail=100 || true
    kubectl --context "$context" -n "$each" logs deploy/hd-core --tail=100 --previous || true
  done
  echo "---- the operator's log ----"
  kubectl --context "$context" -n cnpg-system logs deploy/cnpg-controller-manager --tail=100 || true
}

finish() {
  local status=$?
  if [ -n "$forward" ]; then
    kill "$forward" 2> /dev/null || true
  fi
  if [ "$status" -ne 0 ]; then
    diagnose
  fi
  if [ -z "${KEEP_CLUSTER:-}" ]; then
    kind delete cluster --name "$kind_cluster" > /dev/null 2>&1 || true
  fi
  rm -rf "$work"
  exit "$status"
}
trap finish EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# This chart's core runs the image built here: the image of the chart's own version is
# published only by its release.
image=(--set "core.image.repository=${core_image%:*}" --set "core.image.tag=${core_image##*:}")

# A field of a chart's Chart.yaml, without its quotes.
chart_field() {
  sed -n "s/^$2: *//p" "$1/Chart.yaml" | tr -d "\"'\r"
}

# Reads /api/health through a port-forward to the core's Service, and expects it to answer that
# the database is up: the core runs a query for it.
health() {
  local status="" attempt
  k port-forward service/hd-core "$port:80" > "$work/port-forward.log" 2>&1 &
  forward=$!
  for attempt in $(seq 30); do
    if status=$(curl -sS -o "$work/health.json" -w '%{http_code}' \
      "http://127.0.0.1:$port/api/health" 2> "$work/curl.txt"); then
      break
    fi
    sleep 1
  done
  kill "$forward" 2> /dev/null || true
  wait "$forward" 2> /dev/null || true
  forward=""
  [ "$status" = 200 ] \
    || fail "/api/health answered '$status': $(cat "$work/curl.txt" "$work/port-forward.log")"
  grep -qF '"status":"ok"' "$work/health.json" \
    || fail "/api/health did not say ok: $(cat "$work/health.json")"
  ok "/api/health answered 200 $(cat "$work/health.json")"
}

# release_is REVISION CHART: the release `hd` of the namespace is deployed, at that revision of
# that chart.
release_is() {
  local listed field
  listed=$(h list --filter '^hd$' -o json)
  for field in "\"revision\":\"$1\"" "\"chart\":\"$2\"" '"status":"deployed"'; do
    grep -qF "$field" <<< "$listed" || fail "the release is not at $field: $listed"
  done
  ok "the release hd is deployed at revision $1 of the chart $2"
}

# The checks below ask the cluster in a statement of their own, so that a kubectl or a helm that
# fails stops the script, and is not taken for an answer that names nothing.

# matching KINDS PATTERN…: leaves in $matching the namespace's objects of these kinds whose name
# holds one of the patterns; empty when there is none.
matching() {
  local kinds=$1 listed pattern patterns=()
  shift
  for pattern in "$@"; do
    patterns+=(-e "$pattern")
  done
  listed=$(k get "$kinds" -o name)
  matching=$(grep -F "${patterns[@]}" <<< "$listed" || true)
}

# Leaves in $releases the namespace's releases named hd, whatever their state; empty when there
# is none.
releases() {
  releases=$(h list --filter '^hd$' -q)
}

# Leaves in $clusters the namespace's CloudNativePG clusters; empty when there is none.
clusters() {
  clusters=$(k get clusters.postgresql.cnpg.io -o name)
}

# The core's pod, which there must be.
core_pod_uid() {
  local uid
  uid=$(k get pods -l app.kubernetes.io/component=core -o jsonpath='{.items[0].metadata.uid}')
  [ -n "$uid" ] || fail "the core has no pod"
  echo "$uid"
}

# refused VERB MODE FOUND TO_SET HELM_ARGUMENTS…: an upgrade to this chart with these arguments,
# or an install of it (VERB is upgrade or install), ends with an error that is the guard's
# message: it starts with FOUND, what the guard found, names MODE, the mode asked for, and holds
# TO_SET, the mode to set.
refused() {
  local verb=$1 asked=$2 found=$3 to_set=$4 output
  shift 4
  if h "$verb" hd "$chart" "$@" > "$work/refused.txt" 2>&1; then
    fail "the $verb went through: $(cat "$work/refused.txt")"
  fi
  output=$(tr '\r\n' '  ' < "$work/refused.txt")
  grep -qF -- "$found" <<< "$output" \
    || fail "the $verb failed, but not with the guard's message '$found': $output"
  grep -qF -- "and postgresql.mode is $asked: " <<< "$output" \
    || fail "the guard's message does not name the mode $asked: $output"
  grep -qF -- "$to_set" <<< "$output" \
    || fail "the guard's message does not say '$to_set': $output"
  ok "refused: $output"
}

# ---------------------------------------------------------------------------------------------
step "Tools"
kind version
helm version --short
kubectl version --client

step "The released chart: deploy/chart at the tag $released"
# A checkout in CI has neither the history nor the tags.
if ! git -C "$root" rev-parse -q --verify "refs/tags/$released^{commit}" > /dev/null; then
  git -C "$root" fetch --depth 1 origin "refs/tags/$released:refs/tags/$released"
fi
mkdir "$work/released"
git -C "$root" archive "$released" deploy/chart | tar -x -C "$work/released"
released_chart=$work/released/deploy/chart
released_version=$(chart_field "$released_chart" version)
this_version=$(chart_field "$chart" version)
[ "v$released_version" = "$released" ] \
  || fail "the chart at $released says version $released_version"
ok "the released chart is hd-$released_version, at commit $(git -C "$root" rev-parse --short "$released^{commit}"); this chart is hd-$this_version"

# What makes the upgrade dangerous: the same default values, another database.
helm template hd "$released_chart" > "$work/released.yaml"
grep -q '^kind: Cluster$' "$work/released.yaml" \
  || fail "the released chart does not render a CloudNativePG cluster with its default values"
helm template hd "$chart" > "$work/this.yaml"
grep -q '^kind: StatefulSet$' "$work/this.yaml" \
  || fail "this chart does not render its own PostgreSQL with its default values"
if grep -q '^kind: Cluster$' "$work/this.yaml"; then
  fail "this chart renders a CloudNativePG cluster with its default values"
fi
ok "default values: the released chart renders a CloudNativePG cluster, this chart its own PostgreSQL and no cluster"

step "Create the kind cluster $kind_cluster and load the core's image $core_image"
kind create cluster --name "$kind_cluster" --wait 180s
kind load docker-image "$core_image" --name "$kind_cluster"

step "Install the CloudNativePG operator from $operator_manifest"
kubectl --context "$context" apply --server-side -f "$operator_manifest" > "$work/operator.txt"
echo "    $(wc -l < "$work/operator.txt" | tr -d ' ') objects applied"
kubectl --context "$context" -n cnpg-system rollout status deployment/cnpg-controller-manager \
  --timeout=300s
# The operator answers for its kinds a moment after its pod is ready: a cluster that the server
# accepts in a dry run has passed the operator's admission.
cat > "$work/probe.yaml" << 'EOF'
apiVersion: postgresql.cnpg.io/v1
kind: Cluster
metadata:
  name: probe
spec:
  instances: 1
  storage:
    size: 1Gi
EOF
accepted=""
for attempt in $(seq 60); do
  if kubectl --context "$context" -n default apply --dry-run=server -f "$work/probe.yaml" \
    > "$work/probe.txt" 2>&1; then
    accepted=yes
    break
  fi
  sleep 2
done
[ -n "$accepted" ] || fail "the operator does not admit a cluster: $(cat "$work/probe.txt")"
ok "the operator is ready and admits clusters"

# ---------------------------------------------------------------------------------------------
step "1. Install the released chart hd-$released_version with its default values"
h install hd "$released_chart" --create-namespace
k wait --for=condition=Ready clusters.postgresql.cnpg.io/hd-db --timeout=600s
k rollout status deployment/hd-core --timeout=600s
release_is 1 "hd-$released_version"
released_image=$(k get deployment hd-core -o jsonpath='{.spec.template.spec.containers[0].image}')
ok "OLD CHART INSTALLED: the CloudNativePG cluster hd-db is ready, and so is the released core ($released_image)"
k get clusters.postgresql.cnpg.io,pods

# Runs SQL in the primary of the CloudNativePG cluster, as the owner of the help desk's database.
cluster_sql() {
  local primary
  primary=$(k get cluster hd-db -o jsonpath='{.status.currentPrimary}')
  k exec "$primary" -c postgres -- psql -U postgres -d hd -v ON_ERROR_STOP=1 -qtA \
    -c "set role hd" -c "$1"
}

cluster_uid() {
  k get cluster hd-db -o jsonpath='{.metadata.uid}'
}

cluster_claim_uid() {
  k get persistentvolumeclaim hd-db-1 -o jsonpath='{.metadata.uid}'
}

note="written under the released chart"

expect_cluster_row() {
  local row
  row=$(cluster_sql "select note from upgrade_guard_check")
  [ "$row" = "$note" ] || fail "the row written before is not in the cluster's database, but: '$row'"
  ok "DATUM FOUND in the database of the cluster hd-db: '$row'"
}

cluster_sql "create table upgrade_guard_check (note text)"
cluster_sql "insert into upgrade_guard_check values ('$note')"
ok "DATUM WRITTEN into the database of the cluster hd-db: '$note'"
expect_cluster_row
migrations_before=$(cluster_sql "select count(*) from _sqlx_migrations")
[ "$migrations_before" -gt 0 ] || fail "the released core applied no migrations to the cluster's database"
ok "the released core applied $migrations_before migrations to that database"
cluster_before=$(cluster_uid)
claim_before=$(cluster_claim_uid)
core_before=$(core_pod_uid)
echo "    the cluster: $cluster_before; its volume claim: $claim_before"

# After a refused upgrade: the release, the cluster, its volume, the core and the row are as
# they were, and the chart's own PostgreSQL has not appeared.
expect_cluster_untouched() {
  local core_now
  release_is 1 "hd-$released_version"
  [ "$(cluster_uid)" = "$cluster_before" ] || fail "the cluster hd-db is not the one it was"
  [ "$(cluster_claim_uid)" = "$claim_before" ] || fail "the cluster's volume claim is not the one it was"
  core_now=$(core_pod_uid)
  [ "$core_now" = "$core_before" ] || fail "the refused upgrade replaced the core"
  matching statefulsets,persistentvolumeclaims hd-postgresql
  [ -z "$matching" ] || fail "the refused upgrade made the chart's own PostgreSQL: $matching"
  ok "the cluster hd-db, its volume and the released core are the ones they were, and there is no PostgreSQL of the chart's own"
  expect_cluster_row
}

# ---------------------------------------------------------------------------------------------
step "2. Upgrade to this chart hd-$this_version with its default values: refused"
found="the upgrade guard found the CloudNativePG cluster hd-db of this release in the namespace helpdesk"
to_set="Set postgresql.mode=cloudnativepg to stay on the cluster"
refused upgrade standalone "$found" "$to_set"
ok "UPGRADE REFUSED with the guard's message, for the default values (postgresql.mode=standalone)"
expect_cluster_untouched

step "2a. The same upgrade with postgresql.mode=external: refused as well"
refused upgrade external "$found" "$to_set" \
  --set postgresql.mode=external --set core.database.existingSecret=hd-database
ok "UPGRADE REFUSED with the guard's message, for postgresql.mode=external"
expect_cluster_untouched

# ---------------------------------------------------------------------------------------------
step "3. Upgrade to this chart hd-$this_version with postgresql.mode=cloudnativepg"
h upgrade hd "$chart" --set postgresql.mode=cloudnativepg "${image[@]}"
k rollout status deployment/hd-core --timeout=600s
release_is 2 "hd-$this_version"
upgraded_image=$(k get deployment hd-core -o jsonpath='{.spec.template.spec.containers[0].image}')
[ "$upgraded_image" = "$core_image" ] || fail "the core runs $upgraded_image, not $core_image"
core_after=$(core_pod_uid)
[ "$core_after" != "$core_before" ] || fail "the upgrade did not start a new core"
database_secret=$(k get deployment hd-core \
  -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="HD_DATABASE_URL")].valueFrom.secretKeyRef.name}')
[ "$database_secret" = hd-db-app ] || fail "the core's database is not the cluster's, but from '$database_secret'"
ok "UPGRADE SUCCEEDED: the new core ($upgraded_image) is ready, on the database of the cluster's Secret $database_secret"
health
[ "$(cluster_uid)" = "$cluster_before" ] || fail "the upgrade replaced the cluster hd-db"
[ "$(cluster_claim_uid)" = "$claim_before" ] || fail "the upgrade replaced the cluster's volume claim"
ok "the cluster hd-db and its volume are the ones they were"
matching statefulsets,persistentvolumeclaims hd-postgresql
[ -z "$matching" ] || fail "the upgrade made the chart's own PostgreSQL: $matching"
migrations_after=$(cluster_sql "select count(*) from _sqlx_migrations")
[ "$migrations_after" -ge "$migrations_before" ] \
  || fail "the cluster's database holds $migrations_after migrations, fewer than the $migrations_before before"
ok "the new core found the $migrations_before migrations of the released one there, and the database now holds $migrations_after"
expect_cluster_row

# ---------------------------------------------------------------------------------------------
step "3a. Uninstall the release, which leaves the cluster, and install this chart with its default values: refused"
h uninstall hd --wait --timeout 300s
releases
[ -z "$releases" ] || fail "the release is still there after helm uninstall: $releases"
[ "$(cluster_uid)" = "$cluster_before" ] || fail "helm uninstall did not leave the cluster hd-db"
ok "the release is gone, and the cluster hd-db is still there"
refused install standalone "$found" "$to_set" "${image[@]}"
ok "INSTALL REFUSED with the guard's message, for the default values beside the cluster that was left"
releases
[ -z "$releases" ] || fail "the refused install left a release: $releases"
matching statefulsets,deployments,persistentvolumeclaims hd-postgresql hd-core
[ -z "$matching" ] || fail "the refused install made something: $matching"
ok "the refused install left no release, no core and no PostgreSQL of the chart's own"
expect_cluster_row

step "3b. Install this chart with postgresql.mode=cloudnativepg: the cluster is taken back"
h install hd "$chart" --set postgresql.mode=cloudnativepg "${image[@]}"
k rollout status deployment/hd-core --timeout=600s
release_is 1 "hd-$this_version"
[ "$(cluster_uid)" = "$cluster_before" ] || fail "installing again replaced the cluster hd-db"
[ "$(cluster_claim_uid)" = "$claim_before" ] || fail "installing again replaced the cluster's volume claim"
ok "the core is ready on the cluster that was left, which is the one it was"
health
expect_cluster_row

# ---------------------------------------------------------------------------------------------
namespace=helpdesk-standalone

step "4. Install this chart hd-$this_version with its default values, in the namespace $namespace"
# With the operator in the cluster: the guard asks for a CloudNativePG cluster and finds none.
h install hd "$chart" --create-namespace "${image[@]}"
k rollout status statefulset/hd-postgresql --timeout=300s
k rollout status deployment/hd-core --timeout=600s
release_is 1 "hd-$this_version"
clusters
[ -z "$clusters" ] || fail "the default values made a CloudNativePG cluster: $clusters"
ok "the default values installed the chart's own PostgreSQL and no CloudNativePG cluster, and the core is ready"

standalone_sql() {
  k exec hd-postgresql-0 -- psql -h 127.0.0.1 -U hd -d hd -v ON_ERROR_STOP=1 -qtA -c "$1"
}

standalone_uid() {
  k get statefulset hd-postgresql -o jsonpath='{.metadata.uid}'
}

standalone_claim_uid() {
  k get persistentvolumeclaim data-hd-postgresql-0 -o jsonpath='{.metadata.uid}'
}

standalone_note="written in the PostgreSQL of the chart"
standalone_sql "create table upgrade_guard_check (note text)"
standalone_sql "insert into upgrade_guard_check values ('$standalone_note')"
standalone_before=$(standalone_uid)
standalone_claim_before=$(standalone_claim_uid)
standalone_pod_before=$(k get pod hd-postgresql-0 -o jsonpath='{.metadata.uid}')
ok "a row is written into the chart's own PostgreSQL: '$standalone_note'"

expect_standalone_untouched() {
  local row
  release_is 1 "hd-$this_version"
  [ "$(standalone_uid)" = "$standalone_before" ] || fail "the StatefulSet hd-postgresql is not the one it was"
  [ "$(standalone_claim_uid)" = "$standalone_claim_before" ] || fail "the database's volume claim is not the one it was"
  [ "$(k get pod hd-postgresql-0 -o jsonpath='{.metadata.uid}')" = "$standalone_pod_before" ] \
    || fail "the refused upgrade restarted the database"
  clusters
  [ -z "$clusters" ] || fail "the refused upgrade made a CloudNativePG cluster: $clusters"
  row=$(standalone_sql "select note from upgrade_guard_check")
  [ "$row" = "$standalone_note" ] || fail "the row is not in the chart's own PostgreSQL, but: '$row'"
  ok "the chart's own PostgreSQL, its volume and its row ('$row') are as they were, and there is no CloudNativePG cluster"
}

found="the upgrade guard found the chart's own PostgreSQL of this release (the StatefulSet hd-postgresql) in the namespace helpdesk-standalone"
to_set="Set postgresql.mode=standalone to stay on it"

step "4a. Upgrade the standalone release with postgresql.mode=cloudnativepg: refused"
refused upgrade cloudnativepg "$found" "$to_set" --set postgresql.mode=cloudnativepg "${image[@]}"
ok "STANDALONE REFUSAL with the guard's message, for postgresql.mode=cloudnativepg"
expect_standalone_untouched

step "4b. Upgrade the standalone release with postgresql.mode=external: refused"
refused upgrade external "$found" "$to_set" --set postgresql.mode=external \
  --set core.database.existingSecret=hd-database "${image[@]}"
ok "STANDALONE REFUSAL with the guard's message, for postgresql.mode=external"
expect_standalone_untouched

step "4c. A dry run that reads the cluster (--dry-run=server) is refused in the same way"
refused upgrade external "$found" "$to_set" --dry-run=server --set postgresql.mode=external \
  --set core.database.existingSecret=hd-database "${image[@]}"
expect_standalone_untouched

step "4d. With a CloudNativePG cluster hd-db beside the chart's own PostgreSQL: every mode is refused"
# A cluster under the name the chart would give its own, as one left behind by an earlier
# install: the guard cannot tell which of the two holds the help desk's data.
sed 's/^  name: probe$/  name: hd-db/' "$work/probe.yaml" > "$work/beside.yaml"
k apply -f "$work/beside.yaml"
both="the upgrade guard found two databases of this release in the namespace helpdesk-standalone, the CloudNativePG cluster hd-db and the chart's own PostgreSQL (the StatefulSet hd-postgresql), and cannot tell which one holds the help desk's data"
for mode in standalone cloudnativepg; do
  if h upgrade hd "$chart" --set "postgresql.mode=$mode" "${image[@]}" > "$work/both.txt" 2>&1; then
    fail "the upgrade with postgresql.mode=$mode went through beside both databases: $(cat "$work/both.txt")"
  fi
  output=$(tr '\r\n' '  ' < "$work/both.txt")
  grep -qF -- "$both" <<< "$output" \
    || fail "the upgrade with postgresql.mode=$mode failed, but not with the guard's message '$both': $output"
  for advice in "kubectl -n helpdesk-standalone delete cluster hd-db" \
    "kubectl -n helpdesk-standalone delete statefulset hd-postgresql"; do
    grep -qF -- "$advice" <<< "$output" || fail "the guard's message does not give '$advice': $output"
  done
  ok "refused with postgresql.mode=$mode: $output"
done
ok "BOTH FOUND: refused with the guard's message, in the mode of either"
k delete clusters.postgresql.cnpg.io hd-db --wait=true --timeout=120s
expect_standalone_untouched

step "4e. Upgrade the standalone release with its own mode: not refused"
h upgrade hd "$chart" "${image[@]}"
k rollout status statefulset/hd-postgresql --timeout=300s
k rollout status deployment/hd-core --timeout=600s
release_is 2 "hd-$this_version"
[ "$(standalone_claim_uid)" = "$standalone_claim_before" ] || fail "the upgrade replaced the database's volume claim"
ok "the guard lets the upgrade with postgresql.mode=standalone through"

# ---------------------------------------------------------------------------------------------
step "5. With the StatefulSet deleted, as the guard's message says, another mode passes the guard"
k scale deployment/hd-core --replicas=0
k delete statefulset hd-postgresql --wait=true --timeout=120s
# A dry run that reads the cluster: it shows what the guard answers, and changes nothing.
h upgrade hd "$chart" --dry-run=server --set postgresql.mode=external \
  --set core.database.existingSecret=hd-database "${image[@]}" > "$work/passed.txt" 2>&1 \
  || fail "the guard still refuses postgresql.mode=external: $(cat "$work/passed.txt")"
ok "the guard lets postgresql.mode=external through once the StatefulSet is gone"
[ "$(standalone_claim_uid)" = "$standalone_claim_before" ] || fail "the database's volume claim went with the StatefulSet"
[ "$(k get persistentvolumeclaim data-hd-postgresql-0 -o jsonpath='{.status.phase}')" = Bound ] \
  || fail "the database's volume claim is no longer bound to its volume"
[ -n "$(k get secret hd-postgresql -o jsonpath='{.data.password}')" ] || fail "the password's Secret went with the StatefulSet"
ok "the database's volume claim data-hd-postgresql-0 and the password's Secret are still there"

echo
echo "The upgrade guard passed: the released chart's cluster kept through the upgrade, and both refusals."
