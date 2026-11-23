import type { Projection } from "../../ipc";

export interface GNode {
  id: string;
  kind: "agent" | "tool";
  label: string;
  sub: string;
  status: string;
  x: number;
  y: number;
}
export interface GLink {
  from: string;
  to: string;
  kind: "delegation" | "call";
  crossChecked: boolean;
}
export interface Layout {
  nodes: GNode[];
  links: GLink[];
  width: number;
  height: number;
}

export const NODE_W = 180;
export const NODE_H = 44;
const COL = 210;
const ROW = 64;
const TOP = 24;
const LEFT = 24;

/** The lead on top, its subagents in a row below it, and each agent's tool calls stacked under it. */
export function layout(p: Projection): Layout {
  const childIds = new Set(p.edges.map((e) => e.child));
  const lead = p.agents.find((a) => a.role === "lead") ?? p.agents.find((a) => !childIds.has(a.agent_id));
  const subs = p.agents.filter((a) => a.agent_id !== lead?.agent_id);
  const nodes: GNode[] = [];
  const links: GLink[] = [];
  const colOf = new Map<string, number>();
  subs.forEach((a, i) => colOf.set(a.agent_id, i));
  const leadCol = subs.length > 1 ? (subs.length - 1) / 2 : 0;
  const agentStatus = (a: { status: string | null }) => a.status ?? "unknown";

  const place = (a: (typeof p.agents)[number], col: number, row: number) => {
    nodes.push({
      id: a.agent_id,
      kind: "agent",
      label: a.role ?? a.agent_id,
      sub: a.agent_id.slice(0, 12),
      status: agentStatus(a),
      x: LEFT + col * COL,
      y: TOP + row * ROW,
    });
    const mine = p.tools.filter((t) => t.agent_id === a.agent_id);
    mine.forEach((t, i) => {
      nodes.push({
        id: t.tool_use_id,
        kind: "tool",
        label: t.name ?? "tool",
        sub: t.outcome,
        status: t.outcome,
        x: LEFT + col * COL,
        y: TOP + (row + 1 + i) * ROW + (row === 0 ? ROW : 0),
      });
      links.push({ from: a.agent_id, to: t.tool_use_id, kind: "call", crossChecked: false });
    });
  };
  if (lead) place(lead, leadCol, 0);
  subs.forEach((a, i) => place(a, i, 2));
  for (const e of p.edges) {
    if (e.parent) links.push({ from: e.parent, to: e.child, kind: "delegation", crossChecked: e.cross_checked });
  }
  const known = new Set(nodes.map((n) => n.id));
  const kept = links.filter((l) => known.has(l.from) && known.has(l.to));
  const width = LEFT * 2 + Math.max(1, subs.length, leadCol + 1) * COL;
  const height = TOP * 2 + Math.max(0, ...nodes.map((n) => n.y)) + NODE_H;
  return { nodes, links: kept, width, height };
}
