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
Where the database comes from (postgresql.mode): "standalone", "cloudnativepg", or for a database
of the help desk's own "external-url" (a Secret holding the URL) or "external-values" (separate
values, the password from a Secret). Every template that depends on the database includes this, so that
values the chart refuses stop it with the same message whichever template renders first.
*/}}
{{- define "hd.databaseSource" -}}
{{- $pg := .Values.postgresql }}
{{- /* A null is how --set removes a value that the reused values of an upgrade still hold. */}}
{{- if and (hasKey $pg "enabled") (not (kindIs "invalid" $pg.enabled)) }}
{{- /* What the chart did before: the Secret's database won over the cluster it still created. */}}
{{- if ne (toString $pg.enabled) "true" }}
{{- fail "postgresql.enabled is no longer a value: set postgresql.mode=external and remove postgresql.enabled" }}
{{- else if .Values.core.database.existingSecret }}
{{- fail (printf "postgresql.enabled is no longer a value: set postgresql.mode=external and remove postgresql.enabled (with core.database.existingSecret the core uses that database, not the CloudNativePG cluster %s the chart made beside it: delete that cluster first, since the chart refuses postgresql.mode=external while it exists)" (include "hd.dbName" .)) }}
{{- else }}
{{- fail "postgresql.enabled is no longer a value: set postgresql.mode=cloudnativepg and remove postgresql.enabled" }}
{{- end }}
{{- end }}
{{- $mode := $pg.mode | default "" | toString }}
{{- if not (has $mode (list "standalone" "cloudnativepg" "external")) }}
{{- fail (printf "postgresql.mode is %q: it must be standalone, cloudnativepg or external" $mode) }}
{{- end }}
{{- $db := .Values.core.database }}
{{- $external := $pg.external | default dict }}
{{- $password := ($external.password | default dict).existingSecret }}
{{- if eq $mode "standalone" }}
{{- if $db.existingSecret }}
{{- fail "core.database.existingSecret is for postgresql.mode=external: with postgresql.mode=standalone the core uses the chart's own PostgreSQL" }}
{{- end }}
{{- if or $db.sslMode $db.ca.existingSecret }}
{{- fail "core.database.sslMode and core.database.ca are for postgresql.mode=external: the chart's own PostgreSQL is reached inside the cluster without TLS" }}
{{- end }}
{{- if or $external.host $external.name $external.user $password }}
{{- fail "postgresql.external is for postgresql.mode=external, and postgresql.mode is standalone" }}
{{- end }}
{{- $tag := $pg.standalone.image.tag }}
{{- include "hd.requireStringTag" (dict "name" "postgresql.standalone.image.tag" "tag" $tag) }}
{{- if not (regexMatch "^18([.@-]|$)" $tag) }}
{{- fail (printf "postgresql.standalone.image.tag is %q: the chart runs PostgreSQL 18, and a tag must be of that major version" $tag) }}
{{- end }}
{{- $mode }}
{{- else if eq $mode "cloudnativepg" }}
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

{{/*
The upgrade guard (ADR 0009): the chart refuses a postgresql.mode that is not the mode of the
database it finds in the cluster, so that no install or upgrade, and no change of the default
mode, starts the core on another, empty database. It finds
- the CloudNativePG cluster of the release, by its name: `helm uninstall` leaves it in place
  (postgresql.keepOnUninstall), so an install finds it as an upgrade does;
- the chart's own PostgreSQL, by its StatefulSet: there while a standalone release is installed.
  Its volume alone, which `helm uninstall` leaves, does not count: whoever moves to another
  database deletes the StatefulSet and keeps the volume until the move has proved good.
Included once, by templates/guard.yaml, after the values have been checked. `lookup` finds
nothing where Helm does not read the cluster (helm template, a dry run that is not
--dry-run=server, Argo CD): the guard does not run there. It fails where the cluster does not
know the kind it asks for, hence the question about CloudNativePG's first.
*/}}
{{- define "hd.databaseGuard" -}}
{{- $source := include "hd.databaseSource" . }}
{{- $mode := .Values.postgresql.mode }}
{{- $namespace := .Release.Namespace }}
{{- $clusterName := include "hd.dbName" . }}
{{- $standaloneName := include "hd.standaloneName" . }}
{{- $cluster := dict }}
{{- if .Capabilities.APIVersions.Has "postgresql.cnpg.io/v1/Cluster" }}
{{- $cluster = lookup "postgresql.cnpg.io/v1" "Cluster" $namespace $clusterName }}
{{- end }}
{{- $standalone := lookup "apps/v1" "StatefulSet" $namespace $standaloneName }}
{{- if and $cluster $standalone }}
{{- fail (printf "the upgrade guard found two databases of this release in the namespace %s, the CloudNativePG cluster %s and the chart's own PostgreSQL (the StatefulSet %s), and cannot tell which one holds the help desk's data. Delete the one the core does not run on (kubectl -n %s delete cluster %s, which deletes its data too, or kubectl -n %s delete statefulset %s, which leaves its volume in place), then install or upgrade with the postgresql.mode of the other" $namespace $clusterName $standaloneName $namespace $clusterName $namespace $standaloneName) }}
{{- else if and $cluster (ne $source "cloudnativepg") }}
{{- fail (printf "the upgrade guard found the CloudNativePG cluster %s of this release in the namespace %s, and postgresql.mode is %s: the core would leave the database it has for another one%s. Set postgresql.mode=cloudnativepg to stay on the cluster: it was the chart's default before 0.4.0, and standalone is since. If the core already runs on another database and the cluster was only left behind, or to leave the cluster for good: keep a dump or a backup of what it holds where it outlives the cluster (in this mode the database dumps run only with dumps.enabled=true), delete the cluster (kubectl -n %s delete cluster %s), then install or upgrade, and restore into the new database unless it holds the data already. The chart refuses every other mode for as long as that cluster exists" $clusterName $namespace $mode (ternary ", the chart's own PostgreSQL, which starts empty" "" (eq $source "standalone")) $namespace $clusterName) }}
{{- else if and $standalone (ne $source "standalone") }}
{{- fail (printf "the upgrade guard found the chart's own PostgreSQL of this release (the StatefulSet %s) in the namespace %s, and postgresql.mode is %s: the core would leave the database it has for another one%s. Set postgresql.mode=standalone to stay on it. To leave it for good: take a database dump, delete the StatefulSet (kubectl -n %s delete statefulset %s, which stops the database and leaves its volume and its password's Secret in place), then upgrade, and restore the dump into the new database unless it holds the data already. The chart refuses every other mode for as long as that StatefulSet exists" $standaloneName $namespace $mode (ternary ", a new CloudNativePG cluster, which starts empty" "" (eq $source "cloudnativepg")) $namespace $standaloneName) }}
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

{{/* The chart's own PostgreSQL (postgresql.mode=standalone): its StatefulSet and its Service. */}}
{{- define "hd.standaloneName" -}}
{{- printf "%s-postgresql" (include "hd.fullname" .) | trunc 52 | trimSuffix "-" }}
{{- end }}

{{/* The Secret holding the password of the chart's own PostgreSQL: the one named, or the chart's. */}}
{{- define "hd.standaloneSecret" -}}
{{- .Values.postgresql.standalone.existingSecret | default (include "hd.standaloneName" .) }}
{{- end }}

{{/*
The database as separate parameters, for the core and for pg_dump alike: host, port, name, user,
and the Secret and key of the password, as YAML. Those of the chart's own PostgreSQL, or the
values of an external database; empty when the database is a URL from a Secret.
*/}}
{{- define "hd.databaseParameters" -}}
{{- $source := include "hd.databaseSource" . }}
{{- if eq $source "standalone" }}
host: {{ include "hd.standaloneName" . | quote }}
port: 5432
name: hd
user: hd
secret: {{ include "hd.standaloneSecret" . | quote }}
key: {{ .Values.postgresql.standalone.secretKey | quote }}
{{- else if eq $source "external-values" }}
{{- $external := .Values.postgresql.external }}
host: {{ $external.host | quote }}
port: {{ $external.port }}
name: {{ $external.name | quote }}
user: {{ $external.user | quote }}
secret: {{ $external.password.existingSecret | quote }}
key: {{ $external.password.secretKey | quote }}
{{- end }}
{{- end }}

{{/*
Whether the database dumps run (dumps.enabled): "true" or empty. Left empty they are on with the
chart's own PostgreSQL, which has no other backup, and off in the other modes.
*/}}
{{- define "hd.dumpsEnabled" -}}
{{- $enabled := .Values.dumps.enabled }}
{{- $text := ternary "" (toString $enabled) (kindIs "invalid" $enabled) }}
{{- if not (has $text (list "" "true" "false")) }}
{{- fail (printf "dumps.enabled is %q: it must be true, false, or empty for on with postgresql.mode=standalone and off otherwise" $text) }}
{{- end }}
{{- if or (eq $text "true") (and (eq $text "") (eq (include "hd.databaseSource" .) "standalone")) }}
{{- "true" }}
{{- end }}
{{- end }}

{{/*
Refuses an image's tag that is not a string, named by its value (dict "name" "tag"): a number
would lose its form, as YAML reads 18.10 as 18.1.
*/}}
{{- define "hd.requireStringTag" -}}
{{- if not (kindIs "string" .tag) }}
{{- fail (printf "%s must be a string: quote it in a values file (\"18.8\"), or give it with --set-string" .name) }}
{{- end }}
{{- end }}

{{/* The name of the dumps' CronJob, and of the claim the chart makes for them. */}}
{{- define "hd.dumpsName" -}}
{{- printf "%s-dumps" (include "hd.fullname" .) | trunc 52 | trimSuffix "-" }}
{{- end }}

{{/*
The dumps' claim when the chart's own PostgreSQL also mounts it: "true" or empty. A StorageClass
that binds a volume only for its first pod (WaitForFirstConsumer) would leave the claim pending
until the first dump, and helm install --wait running into its timeout; mounted by the database,
it binds when the database starts. The claim is ReadWriteOnce, so the dumps then run on the
database's node. A claim of dumps.storage.existingClaim is the user's, and left alone.
*/}}
{{- define "hd.dumpsBesideDatabase" -}}
{{- if and (include "hd.dumpsEnabled" .) (not .Values.dumps.storage.existingClaim) (eq (include "hd.databaseSource" .) "standalone") }}
{{- "true" }}
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
