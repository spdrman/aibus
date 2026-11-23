import { DockviewReact, type DockviewReadyEvent } from "dockview-react";
import { useState } from "react";
import { ProvenanceBanner, useFixtureReplay } from "./workspaces/run";
import { GraphView, InspectorView, TimelineView, TraceContext } from "./workspaces/trace";

const components = { graph: GraphView, timeline: TimelineView, details: InspectorView } as const;

function onReady(e: DockviewReadyEvent) {
  const graph = e.api.addPanel({ id: "graph", component: "graph", title: "Graph" });
  e.api.addPanel({ id: "timeline", component: "timeline", title: "Timeline", position: { referencePanel: graph, direction: "below" } });
  e.api.addPanel({ id: "details", component: "details", title: "Details", position: { referencePanel: graph, direction: "right" } });
}

export function App() {
  const { snap, error, replay } = useFixtureReplay();
  const [selected, select] = useState<string | null>(null);
  return (
    <TraceContext.Provider value={{ snap, selected, select }}>
      <div className="app">
        <ProvenanceBanner snap={snap} error={error} onReplay={() => void replay()} />
        <DockviewReact className="dock dockview-theme-spike" components={components} onReady={onReady} />
      </div>
    </TraceContext.Provider>
  );
}
