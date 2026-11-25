import { DockviewReact, type DockviewReadyEvent } from "dockview-react";
import { loadPref, savePref } from "../../shell/prefs";
import { GraphView } from "./GraphView";
import { InspectorView } from "./InspectorView";
import { TimelineView } from "./TimelineView";

const components = { graph: GraphView, timeline: TimelineView, details: InspectorView } as const;
const isLayout = (v: unknown): v is object => typeof v === "object" && v !== null && "panels" in v && "grid" in v;

function defaultLayout(e: DockviewReadyEvent) {
  const graph = e.api.addPanel({ id: "graph", component: "graph", title: "Graph" });
  e.api.addPanel({ id: "timeline", component: "timeline", title: "Timeline", position: { referencePanel: graph, direction: "below" } });
  e.api.addPanel({ id: "details", component: "details", title: "Details", position: { referencePanel: graph, direction: "right" } });
}

// The dock layout (sizes, splits, moved and tabbed panels) is saved on every change and restored on
// start. A saved layout that no longer loads (a panel was renamed, the file is corrupt) falls back
// to the default instead of leaving an empty window.
function onReady(e: DockviewReadyEvent) {
  const saved = loadPref("trace.layout", null, (v): v is object => isLayout(v));
  try {
    if (saved) e.api.fromJSON(saved as Parameters<typeof e.api.fromJSON>[0]);
    else defaultLayout(e);
  } catch {
    e.api.clear();
    defaultLayout(e);
  }
  e.api.onDidLayoutChange(() => savePref("trace.layout", e.api.toJSON()));
}

export function TraceWorkspace() {
  return <DockviewReact className="dock dockview-theme-spike" components={components} onReady={onReady} />;
}
