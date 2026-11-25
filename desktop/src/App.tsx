import { useState } from "react";
import type { Workspace } from "./shell/registry";
import { Shell } from "./shell/Shell";
import { ProvidersWorkspace } from "./workspaces/providers";
import { ProvenanceBanner, RunWorkspace, useFixtureReplay } from "./workspaces/run";
import { TraceContext, TraceWorkspace } from "./workspaces/trace";
import { WorkflowWorkspace } from "./workspaces/workflow";

// The data hooks live here, above the shell, so they survive any workspace switch.
export function App() {
  const { snap, error, replay } = useFixtureReplay();
  const [selected, select] = useState<string | null>(null);
  const workspaces: Workspace[] = [
    { id: "workflow", label: "Workflow", render: () => <WorkflowWorkspace /> },
    { id: "run", label: "Run", render: () => <RunWorkspace snap={snap} error={error} /> },
    { id: "trace", label: "Trace", render: () => <TraceWorkspace /> },
    { id: "providers", label: "Providers", render: () => <ProvidersWorkspace /> },
  ];
  return (
    <TraceContext.Provider value={{ snap, selected, select }}>
      <Shell workspaces={workspaces} header={<ProvenanceBanner snap={snap} error={error} onReplay={() => void replay()} />} />
    </TraceContext.Provider>
  );
}
