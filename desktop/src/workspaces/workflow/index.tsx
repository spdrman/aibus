import { StateNotice } from "../../shell/StateNotice";

// Workflow workspace: reserved for the workflow editor slices of EPIC B. Until one lands it shows an
// honest empty state rather than a disabled placeholder.
export function WorkflowWorkspace() {
  return (
    <StateNotice kind="empty" title="No workflow open">
      The workflow editor is not built yet. Runs you record show up under Run and Trace.
    </StateNotice>
  );
}
