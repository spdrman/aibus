// Run workspace (B1.1 spike, #304): where the data on screen comes from. Today that is the host's
// recorded fixture replay and the banner that labels it; following a live supervised run lands here.
import { StateNotice } from "../../shell/StateNotice";
import type { Snapshot } from "../../ipc";

export { useFixtureReplay, inTauri } from "./useFixtureReplay";
export { ProvenanceBanner } from "./ProvenanceBanner";

// The provenance banner is not here: the shell keeps it above every workspace.
export function RunWorkspace({ snap, error }: { snap: Snapshot | null; error: string | null }) {
  const p = snap?.projection;
  return (
    <div className="panel">
      {!p && error && <StateNotice kind="error" title="No run data">{error}</StateNotice>}
      {!p && !error && <StateNotice kind="empty" title="Waiting for the replay">The host is sending the recorded run.</StateNotice>}
      {p && (
        <table aria-label="Agents in this run">
          <thead>
            <tr><th>Agent</th><th>Role</th><th>Status</th><th>Tools</th></tr>
          </thead>
          <tbody>
            {p.agents.map((a) => (
              <tr key={a.agent_id}>
                <td>{a.agent_id.slice(0, 12)}</td>
                <td>{a.role ?? "unknown"}</td>
                <td>{a.status ?? "unknown"}</td>
                <td>{p.tools.filter((t) => t.agent_id === a.agent_id).length}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
