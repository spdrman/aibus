# Deferred

## A3.2 OpenTelemetry export (#292)

Cut from the reduced scope, still wanted:

- A retention policy (per-project retention and deletion of local traces). Export redaction is in; retention is not.
- Reading `Destination` and `Revisions` from the aibus config file and a CLI switch. Today they are plain structs a caller fills in.
- Langfuse interop. The OTLP body follows the generic OTLP/HTTP shape, but I have not run it against Langfuse, so that row is unverified. Langfuse-specific attribute mapping (`langfuse.*`) is not done.
- Wiring the completeness diagnostics into the trace inspector.
- Scheduler decisions and provider sessions as their own spans (route decisions are events on the attempt span for now).
- A bounded queue between the run and the exporter. `spawn` starts one thread per export; a slow collector under many exports would pile threads up, so a bounded queue with drop-on-full belongs here.
