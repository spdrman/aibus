import type { Evidence } from "../../ipc";
import { useTrace } from "./context";

const evidence = (ev: Evidence[]) => ev.map((e) => `#${e.position} ${e.visibility}/${e.origin}`).join(", ");

export function InspectorView() {
  const { snap, selected } = useTrace();
  const p = snap?.projection;
  const agent = p?.agents.find((a) => a.agent_id === selected);
  const tool = p?.tools.find((t) => t.tool_use_id === selected);
  return (
    <div className="panel">
      <h3>{agent ? `Agent ${agent.role ?? ""}` : tool ? `Tool ${tool.name ?? ""}` : "Details"}</h3>
      {!agent && !tool && <p className="muted">Click a node or a timeline row.</p>}
      {agent && (
        <dl>
          <dt>id</dt><dd>{agent.agent_id}</dd>
          <dt>status</dt><dd>{agent.status ?? "unknown"}</dd>
          <dt>evidence</dt><dd>{evidence(agent.evidence)}</dd>
        </dl>
      )}
      {tool && (
        <dl>
          <dt>tool_use_id</dt><dd>{tool.tool_use_id}</dd>
          <dt>agent</dt><dd>{tool.agent_id}</dd>
          <dt>outcome</dt><dd>{tool.outcome}</dd>
          <dt>evidence</dt><dd>{evidence(tool.evidence)}</dd>
        </dl>
      )}
      <h3>Run</h3>
      {p && (
        <dl>
          <dt>agents</dt><dd>{p.agents.length}</dd>
          <dt>tools</dt><dd>{p.tools.length}</dd>
          <dt>events</dt><dd>{p.timeline.length}</dd>
          <dt>gaps</dt><dd>{p.gaps.length}</dd>
          <dt>attempt</dt><dd>{p.attempts[0]?.status ?? "running"}</dd>
        </dl>
      )}
    </div>
  );
}
