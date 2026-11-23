import { describe, expect, it } from "vitest";
import { layout } from "./layout";
import golden from "../../test/final-snapshot.json";
import type { Snapshot } from "../../ipc";

const snap = golden as unknown as Snapshot;

describe("layout of the Rust golden snapshot", () => {
  const l = layout(snap.projection);
  it("is a fixture, never live", () => {
    expect(snap.provenance.kind).toBe("fixture");
  });
  it("has one lead and at least two subagents", () => {
    const agents = l.nodes.filter((n) => n.kind === "agent");
    expect(agents.filter((n) => n.label === "lead")).toHaveLength(1);
    expect(agents.length).toBeGreaterThanOrEqual(3);
  });
  it("links the lead to each subagent and each agent to its tools", () => {
    expect(l.links.filter((k) => k.kind === "delegation").length).toBeGreaterThanOrEqual(2);
    expect(l.links.filter((k) => k.kind === "call").length).toBeGreaterThanOrEqual(2);
  });
  it("never overlaps two nodes", () => {
    for (const a of l.nodes) for (const b of l.nodes) {
      if (a !== b) expect(a.x === b.x && a.y === b.y).toBe(false);
    }
  });
  it("draws no link to a node it did not place", () => {
    const ids = new Set(l.nodes.map((n) => n.id));
    for (const k of l.links) { expect(ids.has(k.from)).toBe(true); expect(ids.has(k.to)).toBe(true); }
  });
});
