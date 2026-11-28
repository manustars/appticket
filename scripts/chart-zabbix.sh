#!/usr/bin/env bash
# Checks what the Helm chart renders for the Zabbix add-on (zabbix.*): off by default; enabled,
# its configuration file with the sections the chart writes and zabbix.config after them, its
# secrets mounted as files from the existing Secret, its Service, the Ingress of its webhook, its
# NetworkPolicy and the notes; and the values the chart refuses, each with its message. It also
# lints the chart with the add-on enabled.
#
# Usage:
#   bash scripts/chart-zabbix.sh
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

# The rendered documents of one template, such as `template zabbix.yaml`.
template() {
  awk -v source="# Source: hd/templates/$1" '
    /^# Source: / { wanted = ($0 == source) }
    wanted { print }
  ' "$work/all.yaml"
}

# The rendered document of TEMPLATE whose metadata names NAME and whose kind is KIND.
document() {
  template "$1" | awk -v kind="kind: $2" -v name="  name: $3" '
    /^---$/ { if (keep) printf "%s", doc; doc = ""; keep = 0; is_kind = 0; next }
    { doc = doc $0 "\n" }
    $0 == kind { is_kind = 1 }
    is_kind && $0 == name { keep = 1 }
    END { if (keep) printf "%s", doc }
  '
}

# expect_line TEMPLATE LINE: a whole line of what a template rendered, indentation included.
expect_line() {
  if ! template "$1" | grep -qFx -- "$2"; then
    problem "$1: no line '$2'"
  fi
}

expect_no_line() {
  if template "$1" | grep -qFx -- "$2"; then
    problem "$1: unexpected line '$2'"
  fi
}

# expect_lines TEMPLATE KIND NAME LINES: the document holds LINES (several, newline-separated) one
# after the other, indentation included.
expect_lines() {
  local rendered
  rendered=$(document "$1" "$2" "$3")
  if ! awk -v want="$4" '
      BEGIN { n = split(want, lines, "\n") }
      { seen[NR] = $0 }
      END {
        for (start = 1; start <= NR - n + 1; start++) {
          for (i = 1; i <= n && seen[start + i - 1] == lines[i]; i++) {}
          if (i > n) exit 0
        }
        exit 1
      }' <<< "$rendered"; then
    problem "$1: the $2 $3 does not have, in a row: $(tr '\n' '|' <<< "$4")"
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

# expect_none TEXT: nothing the chart rendered holds this text.
expect_none() {
  if grep -qF -- "$1" "$work/all.yaml"; then
    problem "unexpected '$1' in what the chart rendered"
  fi
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

# notes HELM_ARGUMENTS…: what the chart tells whoever installs it, into $work/notes.txt.
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

# The add-on's own configuration, as an install writes it in zabbix.config.
printf '%s\n' '[zabbix]' 'url = "https://zabbix.example.com"' '' '[assets]' \
  'default_kind = "Server"' > "$work/config.toml"
printf '%s\n' '[core]' 'url = "http://core.example.com"' '' '[zabbix]' \
  'url = "https://zabbix.example.com"' > "$work/with-core.toml"
zabbix=(--set zabbix.enabled=true --set zabbix.secret.existingSecret=hd-zabbix
  --set-file "zabbix.config=$work/config.toml")

# ---------------------------------------------------------------------------------------------
# Off by default
# ---------------------------------------------------------------------------------------------

render --set networkPolicy.enabled=true
expect_none 'hd-zabbix'
expect_kind ConfigMap 0
finish "off by default: no add-on, and no NetworkPolicy of its own"

notes
expect_note 'To add the Zabbix add-on, register it first:'
expect_note '--url http://hd-zabbix:8091 \'
expect_note '--scope directory:read --scope catalog:write --scope names:write --scope conflicts:write \'
expect_note '--scope tickets:read --scope tickets:write --event ticket.state_changed'
finish "off: the notes say how to register it, with its scopes and its event"

# ---------------------------------------------------------------------------------------------
# Enabled
# ---------------------------------------------------------------------------------------------

render "${zabbix[@]}"
expect_lines zabbix.yaml ConfigMap hd-zabbix '  zabbix.toml: |
    # Written by the chart: zabbix.coreUrl, zabbix.uiUrl and zabbix.webhook.*.
    [core]
    url = "http://hd-core:80"

    [addon]
    listen = "0.0.0.0:8091"

    [webhook]
    secret_file = "/run/secrets/hd-zabbix/webhook-secret"

    # zabbix.config
    [zabbix]
    url = "https://zabbix.example.com"'
expect_lines zabbix.yaml ConfigMap hd-zabbix '    [assets]
    default_kind = "Server"'
expect_no_line zabbix.yaml '    ui_url = ""'
expect_lines zabbix.yaml Deployment hd-zabbix '          image: ghcr.io/manustars/hd-zabbix:'"$(sed -n 's/^appVersion: "\(.*\)"$/\1/p' "$chart/Chart.yaml")"
expect_lines zabbix.yaml Deployment hd-zabbix '            - name: HD_ZABBIX_CONFIG
              value: /etc/hd/zabbix.toml
            - name: HD_ZABBIX_TOKEN_FILE
              value: "/run/secrets/hd-zabbix/token"
            - name: HD_ZABBIX_EVENT_SECRET_FILE
              value: "/run/secrets/hd-zabbix/event-secret"
            - name: HD_ZABBIX_API_TOKEN_FILE
              value: "/run/secrets/hd-zabbix/api-token"'
expect_lines zabbix.yaml Deployment hd-zabbix '        - name: secrets
          secret:
            secretName: hd-zabbix
            items:
              - key: token
                path: token
              - key: event-secret
                path: event-secret
              - key: api-token
                path: api-token
              - key: webhook-secret
                path: webhook-secret'
expect_lines zabbix.yaml Deployment hd-zabbix '  strategy:
    type: Recreate'
expect_lines zabbix.yaml Deployment hd-zabbix '            readOnlyRootFilesystem: true'
expect_lines zabbix.yaml Service hd-zabbix '  type: ClusterIP'
expect_lines zabbix.yaml Service hd-zabbix '    - name: http
      port: 8091
      targetPort: http'
# No secret's value is ever in a value of the chart, so none can be in what it renders.
expect_none 'HD_ZABBIX_TOKEN:'
expect_kind Ingress 0
expect_none 'kind: NetworkPolicy'
finish "enabled: its configuration file, its secrets as files, its Service; no Ingress"

render "${zabbix[@]}" --set core.publicUrl=https://helpdesk.example.com
expect_line zabbix.yaml '    ui_url = "https://helpdesk.example.com"'
finish "core.publicUrl is the UI's address Zabbix's event menu opens"

render "${zabbix[@]}" --set ingress.enabled=true --set ingress.host=helpdesk.example.com
expect_line zabbix.yaml '    ui_url = "https://helpdesk.example.com"'
finish "with the core's Ingress, its host is the UI's address"

render "${zabbix[@]}" --set core.publicUrl=https://helpdesk.example.com \
  --set zabbix.uiUrl=https://desk.example.com --set zabbix.coreUrl=http://core.helpdesk.test:8080
expect_line zabbix.yaml '    ui_url = "https://desk.example.com"'
expect_line zabbix.yaml '    url = "http://core.helpdesk.test:8080"'
finish "zabbix.uiUrl and zabbix.coreUrl, when set, are the add-on's"

render "${zabbix[@]}" --set zabbix.webhook.mediaType="Help desk (production)"
expect_lines zabbix.yaml ConfigMap hd-zabbix '    [webhook]
    secret_file = "/run/secrets/hd-zabbix/webhook-secret"
    media_type = "Help desk (production)"'
finish "zabbix.webhook.mediaType names the media type"

render "${zabbix[@]}" --set zabbix.webhook.enabled=false
expect_no_line zabbix.yaml '    [webhook]'
expect_no_line zabbix.yaml '              - key: webhook-secret'
finish "the webhook off: no [webhook], and the Secret needs no webhook-secret"

render "${zabbix[@]}" --set zabbix.service.type=LoadBalancer
expect_lines zabbix.yaml Service hd-zabbix '  type: LoadBalancer'
finish "zabbix.service.type exposes the add-on's Service"

render "${zabbix[@]}" --set ingress.enabled=true --set ingress.host=helpdesk.example.com \
  --set ingress.className=nginx --set zabbix.ingress.enabled=true \
  --set ingress.tls.secretName=helpdesk-tls
expect_kind Ingress 2
expect_lines zabbix.yaml Ingress hd-zabbix '  ingressClassName: nginx
  tls:
    - hosts:
        - "helpdesk.example.com"
      secretName: helpdesk-tls
  rules:
    - host: "helpdesk.example.com"
      http:
        paths:
          - path: /webhook
            pathType: Exact
            backend:
              service:
                name: hd-zabbix
                port:
                  name: http'
finish "zabbix.ingress: the path /webhook alone of the help desk's host, over its TLS"

# ---------------------------------------------------------------------------------------------
# NetworkPolicies
# ---------------------------------------------------------------------------------------------

render "${zabbix[@]}" --set networkPolicy.enabled=true
expect_lines networkpolicy.yaml NetworkPolicy hd-core '        - podSelector:
            matchLabels:
              app.kubernetes.io/name: hd
              app.kubernetes.io/instance: hd
              app.kubernetes.io/component: zabbix'
expect_lines networkpolicy.yaml NetworkPolicy hd-zabbix '  policyTypes: ["Ingress", "Egress"]
  ingress:
    - ports:
        - port: http
      from:
        - podSelector:
            matchLabels:
              app.kubernetes.io/name: hd
              app.kubernetes.io/instance: hd
              app.kubernetes.io/component: core
  egress:'
expect_lines networkpolicy.yaml NetworkPolicy hd-zabbix '    - ports:
        - port: 53
          protocol: UDP
        - port: 53
          protocol: TCP
    - ports:
        - port: http
      to:
        - podSelector:
            matchLabels:
              app.kubernetes.io/name: hd
              app.kubernetes.io/instance: hd
              app.kubernetes.io/component: core'
expect_lines networkpolicy.yaml NetworkPolicy hd-zabbix '    - ports:
        - port: 443
          protocol: TCP
        - port: 80
          protocol: TCP'
finish "NetworkPolicy: the core reaches the add-on, which reaches the name service, the core and Zabbix's API"

render "${zabbix[@]}" --set networkPolicy.enabled=true \
  --set 'networkPolicy.zabbix.webhookFrom[0].ipBlock.cidr=192.0.2.10/32' \
  --set 'networkPolicy.zabbix.apiTo[0].ipBlock.cidr=192.0.2.10/32' \
  --set 'networkPolicy.zabbix.apiPorts={8080}'
expect_lines networkpolicy.yaml NetworkPolicy hd-zabbix '              app.kubernetes.io/component: core
        - ipBlock:
            cidr: 192.0.2.10/32
  egress:'
expect_lines networkpolicy.yaml NetworkPolicy hd-zabbix '    - ports:
        - port: 8080
          protocol: TCP
      to:
        - ipBlock:
            cidr: 192.0.2.10/32'
finish "NetworkPolicy: Zabbix's server reaches the webhook, and the add-on Zabbix's API, as given"

render "${zabbix[@]}" --set networkPolicy.enabled=true --set ingress.enabled=true \
  --set zabbix.ingress.enabled=true \
  --set 'networkPolicy.coreFrom[0].namespaceSelector.matchLabels.kubernetes\.io/metadata\.name=ingress-nginx'
expect_lines networkpolicy.yaml NetworkPolicy hd-zabbix '        # The ingress controller, as it reaches the core.
        - namespaceSelector:
            matchLabels:
              kubernetes.io/metadata.name: ingress-nginx
  egress:'
finish "NetworkPolicy with zabbix.ingress: the ingress controller reaches the webhook"

# ---------------------------------------------------------------------------------------------
# Notes, refusals, lint
# ---------------------------------------------------------------------------------------------

notes "${zabbix[@]}" --namespace helpdesk
expect_no_note 'To add the Zabbix add-on'
expect_note 'http://hd-zabbix.helpdesk.svc:8091/webhook'
finish "enabled: the notes give the webhook's address inside the cluster"

notes "${zabbix[@]}" --set ingress.enabled=true --set ingress.host=helpdesk.example.com \
  --set zabbix.ingress.enabled=true
expect_note 'https://helpdesk.example.com/webhook'
finish "with zabbix.ingress: the notes give the webhook's address through the Ingress"

refused "refused: no Secret" "zabbix.secret.existingSecret is required" \
  --set zabbix.enabled=true --set-file "zabbix.config=$work/config.toml"
refused "refused: no configuration" "zabbix.config is required" \
  --set zabbix.enabled=true --set zabbix.secret.existingSecret=hd-zabbix
refused "refused: a section the chart writes" \
  "zabbix.config has a [core], [addon] or [webhook] section, which the chart writes itself" \
  --set zabbix.enabled=true --set zabbix.secret.existingSecret=hd-zabbix \
  --set-file "zabbix.config=$work/with-core.toml"
refused "refused: the webhook's Ingress without the webhook" \
  "zabbix.ingress.enabled needs zabbix.webhook.enabled" \
  "${zabbix[@]}" --set ingress.host=helpdesk.example.com --set zabbix.ingress.enabled=true \
  --set zabbix.webhook.enabled=false

lint "helm lint: the add-on enabled" "${zabbix[@]}" --set networkPolicy.enabled=true \
  --set ingress.enabled=true --set zabbix.ingress.enabled=true
lint "helm lint: the staging example with the add-on enabled" \
  -f "$chart/values-staging.example.yaml" --set zabbix.enabled=true

echo
if [ "$failures" -gt 0 ]; then
  echo "$failures of $cases cases failed"
  exit 1
fi
echo "all $cases cases passed"
