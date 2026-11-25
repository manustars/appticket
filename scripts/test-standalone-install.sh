#!/usr/bin/env bash
# Installs the Helm chart with its own PostgreSQL (postgresql.mode=standalone, ADR 0009) in a
# throwaway kind cluster and checks what only a cluster can show:
#
#   1. the install: the database and the core become ready, /api/health answers that the
#      database is up, the core's log names the connection without TLS, a database dump runs,
#      and with the NetworkPolicy a pod that is neither the core nor the dumps resolves the
#      database's name and gets no answer from it, while one with the labels of the dumps does;
#   2. an upgrade to a later version of the chart: the generated password is the same, the
#      volume is the same, the new core reaches the database, and a row written before is there;
#      an upgrade that moves the generated password to another key is refused;
#   3. `helm uninstall`: the database's volume and the password's Secret are still there;
#   4. installing again under the same name: the core reaches the same data.
#
# Usage:
#   bash scripts/test-standalone-install.sh CORE_IMAGE
#
# CORE_IMAGE is the core's image as the local Docker holds it, such as hd-core:ci, built with
#   docker build -f deploy/Dockerfile --target core -t hd-core:ci .
# It is loaded into the cluster, which never pulls it. Needs kind, kubectl, helm, docker and curl.
# It prints a line per step and exits with 1 at the first check that fails, after printing the
# state of the cluster.
#
# Environment: CHART (the chart's directory, deploy/chart), CLUSTER (the kind cluster's name,
# hd-standalone), PORT (a free local port for reaching the core, 55467), KEEP_CLUSTER (set to
# keep the cluster afterwards; it is deleted otherwise).
set -euo pipefail

core_image=${1:?usage: test-standalone-install.sh CORE_IMAGE}
root=$(cd "$(dirname "$0")/.." && pwd)
chart=${CHART:-$root/deploy/chart}
cluster=${CLUSTER:-hd-standalone}
port=${PORT:-55467}
namespace=helpdesk
context="kind-$cluster"

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
  k get all,persistentvolumeclaims,secrets,networkpolicies -o wide || true
  k describe pods || true
  k get events --sort-by=.lastTimestamp || true
  echo "---- the core's log ----"
  k logs deploy/hd-core --tail=100 || true
  k logs deploy/hd-core --tail=100 --previous || true
  echo "---- the database's log ----"
  k logs hd-postgresql-0 --tail=100 || true
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
    kind delete cluster --name "$cluster" > /dev/null 2>&1 || true
  fi
  rm -rf "$work"
  exit "$status"
}
trap finish EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# The values of every install and upgrade here: the chart's own PostgreSQL, the image built
# here, and the NetworkPolicies.
values=(--set postgresql.mode=standalone
  --set "core.image.repository=${core_image%:*}" --set "core.image.tag=${core_image##*:}"
  --set networkPolicy.enabled=true)

# Waits until the database and the core of the release are ready.
ready() {
  k rollout status statefulset/hd-postgresql --timeout=300s
  k rollout status deployment/hd-core --timeout=600s
}

# Reads /api/health through a port-forward to the core's Service, and expects it to answer that
# the database is up: the core runs a query for it, over the network, with the password.
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

# Runs SQL in the database's own pod.
sql() {
  k exec hd-postgresql-0 -- psql -h 127.0.0.1 -U hd -d hd -v ON_ERROR_STOP=1 -tA -c "$1"
}

# A fingerprint of the generated password, never the password itself.
password_fingerprint() {
  k get secret hd-postgresql -o jsonpath='{.data.password}' | sha256sum | cut -c1-16
}

claim_uid() {
  k get persistentvolumeclaim data-hd-postgresql-0 -o jsonpath='{.metadata.uid}'
}

core_pod_uid() {
  k get pods -l app.kubernetes.io/component=core -o jsonpath='{.items[0].metadata.uid}'
}

note="written by the first install"

expect_row() {
  local found
  found=$(sql "select note from standalone_install_check")
  [ "$found" = "$note" ] || fail "the row written before is not there, but: '$found'"
  ok "the row written by the first install is there"
}

# ---------------------------------------------------------------------------------------------
step "Tools"
kind version
helm version --short
kubectl version --client

step "Create the kind cluster $cluster and load the core's image $core_image"
kind create cluster --name "$cluster" --wait 180s
kind load docker-image "$core_image" --name "$cluster"

# ---------------------------------------------------------------------------------------------
step "1. Install the chart with postgresql.mode=standalone"
h install hd "$chart" --create-namespace "${values[@]}"
ready
ok "the database and the core are ready"

# The kubelet says of each image whether it pulled it or found it on the node.
k get events --field-selector reason=Pulled \
  -o jsonpath='{range .items[*]}{.message}{"\n"}{end}' > "$work/pulled.txt"
grep -F "\"$core_image\"" "$work/pulled.txt" > "$work/core-pulled.txt" \
  || fail "no event about the core's image: $(cat "$work/pulled.txt")"
if grep -vqF 'already present on machine' "$work/core-pulled.txt"; then
  fail "the core's image was pulled, not the one loaded into the cluster: $(cat "$work/core-pulled.txt")"
fi
ok "the core runs the image loaded into the cluster: $(sed -n 1p "$work/core-pulled.txt")"

health

# The line of the core's log that names the choice.
k logs deployment/hd-core > "$work/core.log"
warning=$(grep -m 1 -F 'sslmode disable (set by HD_DATABASE_SSLMODE)' "$work/core.log" || true)
[ -n "$warning" ] || fail "the core's log does not name the connection without TLS"
ok "the core's log names the choice: $warning"

sql "create table standalone_install_check (note text)" > /dev/null
sql "insert into standalone_install_check values ('$note')" > /dev/null
expect_row

step "1a. The database dumps are on by default: take one now"
k get cronjob hd-dumps -o name
k create job --from=cronjob/hd-dumps hd-dump-now
k wait --for=condition=complete job/hd-dump-now --timeout=300s
k logs job/hd-dump-now | tee "$work/dump.txt"
grep -q '^dumped hd-.*\.dump' "$work/dump.txt" || fail "the dump job did not write a dump"
ok "pg_dump reached the database with the generated password and wrote a dump"

step "1b. With the NetworkPolicy, a pod that is neither the core nor the dumps gets no answer"
image=$(k get statefulset hd-postgresql -o jsonpath='{.spec.template.spec.containers[0].image}')

# database_answer POD LABELS: runs a pod with these labels that asks the database's Service
# whether it accepts connections. Leaves in $code how the pod ended (pg_isready's own status: 0
# when the server accepts connections, 2 when nothing answers; 9 when the pod cannot even resolve
# the Service's name) and in $answer what it printed: the address the name resolved to, then
# pg_isready's line.
database_answer() {
  local pod=$1 labels=$2 phase attempt
  k run "$pod" --image="$image" --restart=Never --labels="$labels" --command -- sh -c \
    'getent hosts hd-postgresql || exit 9; pg_isready -h hd-postgresql -p 5432 -t 10' > /dev/null
  for attempt in $(seq 60); do
    phase=$(k get pod "$pod" -o jsonpath='{.status.phase}')
    if [ "$phase" = Succeeded ] || [ "$phase" = Failed ]; then
      break
    fi
    sleep 2
  done
  answer=$(k logs "$pod" | tr '\n' ' ')
  code=$(k get pod "$pod" -o jsonpath='{.status.containerStatuses[0].state.terminated.exitCode}')
  k delete pod "$pod" > /dev/null
}

service_address=$(k get service hd-postgresql -o jsonpath='{.spec.clusterIP}')

database_answer outsider run=outsider
# The name resolved, so that no answer is the policy's doing and not the name service's.
grep -qF "$service_address " <<< "$answer" \
  || fail "the outsider did not resolve hd-postgresql to $service_address (ended with '$code'): $answer"
[ "$code" = 2 ] || fail "the outsider's pg_isready ended with '$code' ($answer), expected 2: no answer"
ok "the outsider resolved the Service's name and got no answer (pg_isready ended with 2: $answer)"

# What the guide tells whoever restores a dump by hand: a pod with the labels of the dumps.
database_answer restorer \
  app.kubernetes.io/name=hd,app.kubernetes.io/instance=hd,app.kubernetes.io/component=dumps
[ "$code" = 0 ] || fail "a pod with the dumps' labels ended with '$code' ($answer), expected 0"
ok "a pod with the labels of the dumps reaches the database (pg_isready ended with 0: $answer)"

password_before=$(password_fingerprint)
claim_before=$(claim_uid)
core_before=$(core_pod_uid)
database_before=$(k get pod hd-postgresql-0 -o jsonpath='{.metadata.uid}')
echo "    the password's fingerprint: $password_before; the volume claim: $claim_before"

# ---------------------------------------------------------------------------------------------
step "2. Upgrade the release to a later version of the chart"
# A copy of the chart with a later version, as the next release's chart is to this one's: its
# labels change, which a StatefulSet accepts only outside its selector and its volume claim.
cp -r "$chart" "$work/later"
sed -i 's/^version:.*/version: 99.0.0/' "$work/later/Chart.yaml"
h upgrade hd "$work/later" "${values[@]}"
ready
h list
[ "$(h list -o json | grep -c '"chart":"hd-99.0.0"')" = 1 ] || fail "the release is not at the later chart"
ok "the release is at the later chart, and the database and the core are ready"

[ "$(core_pod_uid)" != "$core_before" ] || fail "the upgrade did not start a new core"
ok "the upgrade started a new core"
[ "$(password_fingerprint)" = "$password_before" ] || fail "the upgrade changed the generated password"
ok "the password is the same after the upgrade (fingerprint $(password_fingerprint))"
[ "$(claim_uid)" = "$claim_before" ] || fail "the upgrade replaced the database's volume claim"
ok "the database's volume claim is the same"
[ "$(k get pod hd-postgresql-0 -o jsonpath='{.metadata.uid}')" = "$database_before" ] \
  || fail "the upgrade restarted the database, though nothing of its own changed"
ok "the database was not restarted: nothing of its own changed"
health
expect_row

step "2a. An upgrade that moves the generated password to another key is refused"
# It would generate another password under that key, which the database does not accept.
if h upgrade hd "$work/later" "${values[@]}" --set postgresql.standalone.secretKey=other \
  > "$work/refused.txt" 2>&1; then
  fail "the upgrade with another postgresql.standalone.secretKey went through"
fi
refusal='the Secret hd-postgresql holds the generated password of the chart'"'"'s own PostgreSQL, but not under the key "other" (postgresql.standalone.secretKey)'
grep -qF "$refusal" "$work/refused.txt" \
  || fail "the upgrade was refused with another message: $(cat "$work/refused.txt")"
ok "refused: $(grep -m 1 -oF "$refusal" "$work/refused.txt")"
[ "$(password_fingerprint)" = "$password_before" ] || fail "the refused upgrade changed the password"
[ "$(k get secret hd-postgresql -o jsonpath='{.data.other}')" = "" ] \
  || fail "the refused upgrade added the other key to the Secret"
ok "the Secret is unchanged after the refused upgrade"

# ---------------------------------------------------------------------------------------------
step "3. Uninstall the release"
h uninstall hd --wait --timeout 300s
for attempt in $(seq 90); do
  if [ -z "$(k get pods -l app.kubernetes.io/instance=hd -o name)" ]; then
    break
  fi
  sleep 2
done
[ -z "$(k get statefulsets,deployments,pods -l app.kubernetes.io/instance=hd -o name)" ] \
  || fail "the release's workloads are still there"
ok "the core and the database are gone"
k get persistentvolumeclaims
[ "$(claim_uid)" = "$claim_before" ] || fail "the database's volume claim is not there after helm uninstall"
[ "$(k get persistentvolumeclaim data-hd-postgresql-0 -o jsonpath='{.status.phase}')" = Bound ] \
  || fail "the database's volume claim is no longer bound to its volume"
ok "the database's volume claim data-hd-postgresql-0 is still there, bound to its volume"
[ "$(password_fingerprint)" = "$password_before" ] || fail "the password's Secret is not there after helm uninstall"
ok "the password's Secret is still there, with the same password"

# ---------------------------------------------------------------------------------------------
step "4. Install again under the same name"
h install hd "$chart" "${values[@]}"
ready
[ "$(password_fingerprint)" = "$password_before" ] || fail "installing again changed the password"
ok "the password is the same after installing again"
[ "$(claim_uid)" = "$claim_before" ] || fail "installing again made another volume claim"
ok "the database runs on the volume that was kept"
health
expect_row

echo
echo "The standalone install passed: install, upgrade, uninstall and install again."
