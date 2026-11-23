// The IPC payload as aibus-desktop serialises it (`Snapshot` in src-tauri/src/lib.rs). The golden
// file src/test/final-snapshot.json is the Rust side's real output, checked by a Rust test, and the
// vitest suite loads it against these types, so the two sides cannot drift silently.
export type Visibility = "observed" | "correlated" | "inferred" | "unavailable";

export interface Evidence {
  event_id: string;
  position: number;
  visibility: Visibility;
  origin: string;
}

export interface AgentNode {
  attempt_id: string;
  agent_id: string;
  role: string | null;
  status: string | null;
  evidence: Evidence[];
}

export interface Edge {
  attempt_id: string;
  parent: string | null;
  child: string;
  via_tool_use_id: string | null;
  level: Visibility;
  cross_checked: boolean;
  sources: Evidence[];
}

export interface ToolNode {
  attempt_id: string;
  tool_use_id: string;
  agent_id: string | null;
  name: string | null;
  outcome: "completed" | "failed" | "unknown";
  evidence: Evidence[];
}

export interface TimelineRow {
  position: number;
  observed_at: string;
  occurred_at: string | null;
  kind: string;
  visibility: Visibility;
  origin: string;
  agent_id: string | null;
  corroborates: boolean;
}

export interface Projection {
  attempts: { attempt_id: string; status: string | null; requires_reconciliation: boolean }[];
  agents: AgentNode[];
  edges: Edge[];
  tools: ToolNode[];
  timeline: TimelineRow[];
  gaps: { position: number; attempt_id: string; reason: string }[];
}

// Only "fixture" is produced in the spike; "live" is reserved so the badge can never be implied.
export type Provenance =
  | { kind: "fixture"; label: string; capture: string; cli_version: string }
  | { kind: "live" };

export interface Snapshot {
  provenance: Provenance;
  step: number;
  steps: number;
  t_ms: number;
  projection: Projection;
}

export interface StackInfo {
  tauri: string;
  os: string;
  arch: string;
  webview: string | null;
}
