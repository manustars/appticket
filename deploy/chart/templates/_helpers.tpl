{{/* Names, labels and shared fragments of the help desk chart. */}}

{{- define "hd.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "hd.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "hd.labels" -}}
helm.sh/chart: {{ printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" }}
app.kubernetes.io/name: {{ include "hd.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/* Selector labels of one component: pass (dict "root" . "component" "core"). */}}
{{- define "hd.selectorLabels" -}}
app.kubernetes.io/name: {{ include "hd.name" .root }}
app.kubernetes.io/instance: {{ .root.Release.Name }}
app.kubernetes.io/component: {{ .component }}
{{- end }}

{{- define "hd.coreName" -}}
{{- printf "%s-core" (include "hd.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "hd.assistantName" -}}
{{- printf "%s-assistant" (include "hd.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "hd.dbName" -}}
{{- printf "%s-db" (include "hd.fullname" .) | trunc 50 | trimSuffix "-" }}
{{- end }}

{{/* An image reference: pass (dict "image" .Values.core.image "root" .). */}}
{{- define "hd.image" -}}
{{- printf "%s:%s" .image.repository (default .root.Chart.AppVersion .image.tag) }}
{{- end }}

{{/*
Where the database comes from (postgresql.mode): "cloudnativepg", or for a database of the help
desk's own "external-url" (a Secret holding the URL) or "external-values" (separate values, the
password from a Secret). Every template that depends on
the database includes this, so that values the chart refuses stop it with the same message
whichever template renders first.
*/}}
{{- define "hd.databaseSource" -}}
{{- $pg := .Values.postgresql }}
{{- if hasKey $pg "enabled" }}
{{- fail (printf "postgresql.enabled is no longer a value: set postgresql.mode=%s and remove postgresql.enabled" (ternary "cloudnativepg" "external" (eq (toString $pg.enabled) "true"))) }}
{{- end }}
{{- $mode := toString $pg.mode }}
{{- if not (has $mode (list "standalone" "cloudnativepg" "external")) }}
{{- fail (printf "postgresql.mode is %q: it must be standalone, cloudnativepg or external" $mode) }}
{{- end }}
{{- if eq $mode "standalone" }}
{{- fail "postgresql.mode=standalone is not available yet: set postgresql.mode to cloudnativepg or external" }}
{{- end }}
{{- $db := .Values.core.database }}
{{- $external := $pg.external | default dict }}
{{- $password := ($external.password | default dict).existingSecret }}
{{- if eq $mode "cloudnativepg" }}
{{- if $db.existingSecret }}
{{- fail "core.database.existingSecret is for postgresql.mode=external: with postgresql.mode=cloudnativepg the core uses the Secret of the CloudNativePG cluster" }}
{{- end }}
{{- if or $db.sslMode $db.ca.existingSecret }}
{{- fail "core.database.sslMode and core.database.ca are for postgresql.mode=external: the CloudNativePG cluster is verified against its own authority" }}
{{- end }}
{{- if or $external.host $external.name $external.user $password }}
{{- fail "postgresql.external is for postgresql.mode=external, and postgresql.mode is cloudnativepg" }}
{{- end }}
{{- $mode }}
{{- else }}
{{- $missing := list }}
{{- if not $external.host }}{{ $missing = append $missing "postgresql.external.host" }}{{ end }}
{{- if not $external.name }}{{ $missing = append $missing "postgresql.external.name" }}{{ end }}
{{- if not $external.user }}{{ $missing = append $missing "postgresql.external.user" }}{{ end }}
{{- if not $password }}{{ $missing = append $missing "postgresql.external.password.existingSecret" }}{{ end }}
{{- $separate := lt (len $missing) 4 }}
{{- if and $db.existingSecret $separate }}
{{- fail "postgresql.mode=external takes the database as a Secret holding the URL (core.database.existingSecret) or as separate values (postgresql.external), not both" }}
{{- else if $db.existingSecret }}
{{- "external-url" }}
{{- else if not $separate }}
{{- fail "postgresql.mode=external needs the database: set core.database.existingSecret (a Secret holding the URL), or postgresql.external.host, name, user and password.existingSecret" }}
{{- else if $missing }}
{{- fail (printf "postgresql.external is given in part: set also %s" (join ", " $missing)) }}
{{- else }}
{{- "external-values" }}
{{- end }}
{{- end }}
{{- end }}

{{/* The Secret holding the database URL: the CloudNativePG cluster's, or the one named. */}}
{{- define "hd.databaseSecret" -}}
{{- if eq (include "hd.databaseSource" .) "cloudnativepg" }}
{{- printf "%s-app" (include "hd.dbName" .) }}
{{- else }}
{{- .Values.core.database.existingSecret }}
{{- end }}
{{- end }}

{{/* Restricted pod and container security settings shared by the chart's own images. */}}
{{- define "hd.podSecurityContext" -}}
runAsNonRoot: true
runAsUser: 10001
runAsGroup: 10001
fsGroup: 10001
seccompProfile:
  type: RuntimeDefault
{{- end }}

{{- define "hd.containerSecurityContext" -}}
allowPrivilegeEscalation: false
readOnlyRootFilesystem: true
capabilities:
  drop: ["ALL"]
{{- end }}
