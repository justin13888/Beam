{{/*
The chart name, overridable.
*/}}
{{- define "beam.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
The fully qualified release name. Truncated to 63 characters (the DNS label
limit), and with room left for the "-web" suffix the web client's objects add.
*/}}
{{- define "beam.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 59 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 59 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 59 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{- define "beam.webFullname" -}}
{{- printf "%s-web" (include "beam.fullname" .) }}
{{- end }}

{{- define "beam.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{- define "beam.labels" -}}
helm.sh/chart: {{ include "beam.chart" . }}
app.kubernetes.io/name: {{ include "beam.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels. The component label keeps the server's and the web client's
selectors disjoint.
*/}}
{{- define "beam.selectorLabels" -}}
app.kubernetes.io/name: {{ include "beam.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: server
{{- end }}

{{- define "beam.webSelectorLabels" -}}
app.kubernetes.io/name: {{ include "beam.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/component: web
{{- end }}

{{- define "beam.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "beam.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
The server image. The default tag is the one release.yml publishes for the
release this chart version belongs to.
*/}}
{{- define "beam.image" -}}
{{- printf "%s:%s" .Values.image.repository (default (printf "v%s" .Chart.AppVersion) .Values.image.tag) }}
{{- end }}

{{/*
The Secret the chart creates for values given inline (database.url,
oidc.clientSecret, tmdb.apiToken, tvdb.apiKey, tvdb.pin, telemetry.url).
*/}}
{{- define "beam.secretName" -}}
{{- include "beam.fullname" . }}
{{- end }}

{{/*
The secretKeyRef behind BEAM_DATABASE_URL. The server and the migration Job
both read it from here, so they can never point at different databases.
*/}}
{{- define "beam.databaseUrlSecretKeyRef" -}}
{{- if .Values.database.existingSecret }}
name: {{ .Values.database.existingSecret }}
key: {{ .Values.database.existingSecretKey }}
{{- else }}
name: {{ include "beam.secretName" . }}
key: database-url
{{- end }}
{{- end }}

{{/*
The pod securityContext of every pod that runs the server image: the image's
`appuser`, which owns /data in the image. Fixed, not a value; media access is
widened with supplemental groups instead. `check:chart-invariants` fails if
the server's supplemental groups are not what values set, or if the migration
Job's pod differs from the server's.
*/}}
{{- define "beam.podSecurityContext" -}}
runAsNonRoot: true
runAsUser: 1000
runAsGroup: 1000
fsGroup: 1000
fsGroupChangePolicy: OnRootMismatch
{{- with .Values.server.podSecurityContext.supplementalGroups }}
# Groups the library media is readable by (a NAS `media` group).
supplementalGroups:
  {{- toYaml . | nindent 2 }}
{{- end }}
seccompProfile:
  type: RuntimeDefault
{{- end }}

{{/*
The container securityContext of every container that runs the server image.
*/}}
{{- define "beam.containerSecurityContext" -}}
allowPrivilegeEscalation: false
readOnlyRootFilesystem: true
capabilities:
  drop: ["ALL"]
{{- end }}

{{- define "beam.dataClaimName" -}}
{{- default (printf "%s-data" (include "beam.fullname" .)) .Values.persistence.data.existingClaim }}
{{- end }}

{{/*
BEAM_WEB_URL: explicit, else the public URL when the web client is served from
the same origin, else unset (the server's own default).
*/}}
{{- define "beam.webUrl" -}}
{{- if .Values.server.webUrl }}
{{- .Values.server.webUrl }}
{{- else if .Values.web.enabled }}
{{- .Values.server.publicUrl }}
{{- end }}
{{- end }}

{{/*
BEAM_RATE_LIMIT_TRUST_FORWARDED_FOR: explicit, else whether an ingress is the
only way in -- an ingress, and a ClusterIP Service that clients cannot reach
directly. A NodePort or LoadBalancer Service would let a client forge the
header and pick its own rate-limit bucket.
*/}}
{{- define "beam.trustForwardedFor" -}}
{{- if kindIs "bool" .Values.rateLimit.trustForwardedFor }}
{{- .Values.rateLimit.trustForwardedFor }}
{{- else }}
{{- and .Values.ingress.enabled (eq .Values.service.type "ClusterIP") }}
{{- end }}
{{- end }}

{{/*
The server pod's terminationGracePeriodSeconds: BEAM_SHUTDOWN_TIMEOUT_SECS plus
a margin for the process to exit after draining, so the kubelet's SIGKILL never
cuts a drain short.
*/}}
{{- define "beam.terminationGracePeriodSeconds" -}}
{{- add (int .Values.server.shutdownTimeoutSeconds) 15 }}
{{- end }}

{{/*
Volume source kinds with no readOnly field that are still safe as a library:
the kubelet never applies fsGroup to them and never writes to them. Every other
kind the schema accepts has a readOnly field, which the chart forces true.
*/}}
{{- define "beam.readOnlyWithoutFlag" -}}
{{- list "hostPath" "image" | toJson }}
{{- end }}

{{/*
Fail rendering on combinations the schema cannot express, so `helm template`
refuses them as well as `helm install`.
*/}}
{{- define "beam.validate" -}}
{{- if not .Values.server.publicUrl }}
{{- fail "server.publicUrl is required: the externally reachable origin of the Beam API, e.g. https://beam.example.com" }}
{{- end }}
{{- if and .Values.database.url .Values.database.existingSecret }}
{{- fail "set only one of database.url and database.existingSecret" }}
{{- end }}
{{- if not (or .Values.database.url .Values.database.existingSecret) }}
{{- fail "database.url or database.existingSecret is required: Beam needs an external PostgreSQL" }}
{{- end }}
{{- if and .Values.migrations.job.enabled (not .Values.database.existingSecret) }}
{{- fail "migrations.job.enabled requires database.existingSecret: the Job is a pre-install hook, which runs before the Secret the chart creates for database.url exists" }}
{{- end }}
{{- $names := dict }}
{{- range .Values.libraries }}
{{- if hasKey $names .name }}
{{- fail (printf "libraries: %q is listed twice; each library needs its own name" .name) }}
{{- end }}
{{- $_ := set $names .name true }}
{{- end }}
{{- end }}
