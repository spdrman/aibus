import { useEffect, useRef } from "react";
import { useTrace } from "./context";

export function TimelineView() {
  const { snap, selected, select } = useTrace();
  const end = useRef<HTMLTableRowElement>(null);
  useEffect(() => end.current?.scrollIntoView({ block: "nearest" }), [snap?.step]);
  if (!snap) return <div className="panel muted">Waiting for events...</div>;
  return (
    <div className="panel">
      <table aria-label="Timeline of normalised events">
        <thead>
          <tr><th>#</th><th>Kind</th><th>Agent</th><th>Evidence</th><th>Origin</th></tr>
        </thead>
        <tbody>
          {snap.projection.timeline.map((r) => (
            <tr key={r.position} data-selected={r.agent_id !== null && r.agent_id === selected} tabIndex={0} aria-label={`Event ${r.position}, ${r.kind}`} onClick={() => select(r.agent_id)} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); select(r.agent_id); } }}>
              <td>{r.position}</td>
              <td>{r.kind}{r.corroborates ? " (corroborates)" : ""}</td>
              <td>{r.agent_id?.slice(0, 12) ?? ""}</td>
              <td>{r.visibility}</td>
              <td>{r.origin}</td>
            </tr>
          ))}
          <tr ref={end} />
        </tbody>
      </table>
    </div>
  );
}
