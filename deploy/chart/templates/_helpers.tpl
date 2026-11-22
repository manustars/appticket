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

{{/* The Secret and key holding the database URL; fails when there is none. */}}
{{- define "hd.databaseSecret" -}}
{{- if .Values.core.database.existingSecret }}
{{- .Values.core.database.existingSecret }}
{{- else if .Values.postgresql.enabled }}
{{- printf "%s-app" (include "hd.dbName" .) }}
{{- else }}
{{- fail "set core.database.existingSecret (a Secret with the database URL) or postgresql.enabled=true" }}
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
