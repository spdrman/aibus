// @vitest-environment jsdom
import { render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

// dockview needs real layout, which jsdom does not have, so the library is replaced by a recorder
// that drives onReady the way the real one does. What is under test is the save and restore logic.
const calls: string[] = [];
let layoutListener: () => void = () => {};
let failLoad = false;
const api = {
  fromJSON: vi.fn((l: unknown) => { if (failLoad) throw new Error("bad layout"); calls.push("fromJSON:" + JSON.stringify(l)); }),
  clear: vi.fn(() => calls.push("clear")),
  addPanel: vi.fn((p: { id: string }) => { calls.push("add:" + p.id); return p.id; }),
  onDidLayoutChange: vi.fn((cb: () => void) => { layoutListener = cb; }),
  toJSON: vi.fn(() => ({ grid: { w: 1 }, panels: { graph: {} } })),
};
vi.mock("dockview-react", () => ({
  DockviewReact: ({ onReady }: { onReady: (e: { api: typeof api }) => void }) => { onReady({ api }); return null; },
}));

const { TraceWorkspace } = await import("./TraceWorkspace");

beforeEach(() => { calls.length = 0; failLoad = false; vi.clearAllMocks(); });

describe("trace dock layout", () => {
  it("builds the default three panels on first start", () => {
    render(<TraceWorkspace />);
    expect(calls).toEqual(["add:graph", "add:timeline", "add:details"]);
  });

  it("saves on every layout change and restores it on the next start", () => {
    render(<TraceWorkspace />);
    layoutListener();
    const saved = window.localStorage.getItem("aibus.desktop.v1.trace.layout");
    expect(saved).toBe(JSON.stringify({ grid: { w: 1 }, panels: { graph: {} } }));
    calls.length = 0;
    render(<TraceWorkspace />);
    expect(calls).toEqual(["fromJSON:" + saved]);
  });

  it("falls back to the default when the saved layout will not load or is not a layout", () => {
    window.localStorage.setItem("aibus.desktop.v1.trace.layout", JSON.stringify({ grid: {}, panels: {} }));
    failLoad = true;
    render(<TraceWorkspace />);
    expect(calls).toEqual(["clear", "add:graph", "add:timeline", "add:details"]);
    calls.length = 0;
    window.localStorage.setItem("aibus.desktop.v1.trace.layout", JSON.stringify("junk"));
    render(<TraceWorkspace />);
    expect(calls).toEqual(["add:graph", "add:timeline", "add:details"]);
  });
});
