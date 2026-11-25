# OpenTelemetry export (A3.2, #292)

The local trace stays the source of truth and works with nothing else running. Export is a copy, and it is off until you give it a destination.

## What gets sent

`aibus_trace::otel::to_otlp(events, revisions, redactor)` turns a trace into an OTLP/JSON traces request: a `aibus.workflow.run` span, one `aibus.attempt` per attempt, then `aibus.agent`, `aibus.delegation` and `aibus.tool` spans hung under the thing that spawned them (a child agent under its delegation, a tool under its agent). Names are fixed strings; tool name, agent role and provider extension are attributes (model and version labels are not sent yet, see DEFERRED). Route decisions, approvals, artifacts, usage, gaps and warnings are events on the attempt span. Message chunks are not exported.

The trace id is the workflow run id and span ids are derived from it, so exporting twice gives the same ids. Usage carries `aibus.usage.<name>`, `.unit`, `.quality` (`reported`, `derived`, `estimated`, `unknown`) and `.source`; an unknown value is sent with no number. Prompt and workflow revision ids go on the resource (`aibus.prompt_revision`, `aibus.workflow_revision`), never the prompt text. Every string goes through the redactor first.

`Completeness` says what is thin before you export: unfinished spans (also marked `aibus.complete=false`), end events with no start, agents naming a parent that is not in the trace, attempts with no usage.

## Turning it on

`aibus_otel::Destination { endpoint, headers, timeout }` is the opt-in. With `None`, nothing connects (the standard `OTEL_EXPORTER_OTLP_*` variables are ignored on purpose; once a destination is set, reqwest's proxy variables apply to it). `aibus_otel::export` runs the request on a worker thread and waits at most the timeout, so a caller that must not wait puts it on its own thread; a refused connection, a hung collector or a 5xx comes back as `Outcome::Failed` and goes nowhere near the run. The first export builds the TLS client (system roots) inside that timeout, so give the first call a generous one. Redirects are not followed.

Exported traces can contain file paths, agent names and anything a tool name carries, even without payload capture. Redaction is best effort for unregistered secrets.

## Checking against a local collector

```
docker run -d --name otelcol -p 4318:4318 -v $PWD/otelcol.yaml:/etc/otelcol/config.yaml otel/opentelemetry-collector:0.116.1
AIBUS_OTLP_TEST_ENDPOINT=http://127.0.0.1:4318/v1/traces cargo test -p aibus-otel -- --ignored
```

with `otelcol.yaml` receiving OTLP http and exporting to `debug` (detailed verbosity). The test is ignored in gates because it needs the collector. I ran it on 2026-11-25 against `otel/opentelemetry-collector:0.116.1` (default config, in its own container on a throw-away docker network): the collector accepted the body and its debug exporter logged the resource attributes and spans. Langfuse is still unrun.
