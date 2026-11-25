import { useMemo } from "react";
import { useTrace } from "./context";
import { layout, NODE_H, NODE_W } from "./layout";

export function GraphView() {
  const { snap, selected, select } = useTrace();
  const l = useMemo(() => (snap ? layout(snap.projection) : null), [snap]);
  if (!l) return <div className="panel muted">Waiting for events...</div>;
  const at = new Map(l.nodes.map((n) => [n.id, n]));
  return (
    <div className="panel">
      <svg width={l.width} height={l.height} role="img" aria-label="Agent graph: the lead, its subagents and their tool calls">
        {l.links.map((k) => {
          const a = at.get(k.from)!, b = at.get(k.to)!;
          const x1 = a.x + NODE_W / 2, y1 = a.y + NODE_H, x2 = b.x + NODE_W / 2, y2 = b.y;
          const my = (y1 + y2) / 2;
          return <path key={`${k.from}>${k.to}`} className={`link ${k.kind}`} d={`M${x1},${y1} C${x1},${my} ${x2},${my} ${x2},${y2}`} />;
        })}
        {l.nodes.map((n) => (
          <g
            key={n.id}
            className="node"
            data-kind={n.kind}
            data-status={n.status}
            data-selected={selected === n.id}
            transform={`translate(${n.x},${n.y})`}
            onClick={() => select(n.id)}
          >
            <rect width={NODE_W} height={NODE_H} rx={6} />
            <text x={10} y={18} fontWeight={600}>
              {n.kind === "agent" ? "Agent: " : "Tool: "}
              {n.label}
            </text>
            <text x={10} y={34} className="sub">
              {n.status} · {n.sub}
            </text>
          </g>
        ))}
      </svg>
    </div>
  );
}
