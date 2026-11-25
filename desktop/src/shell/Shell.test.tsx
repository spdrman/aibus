// @vitest-environment jsdom
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { loadPref } from "./prefs";
import type { Workspace } from "./registry";
import { Shell } from "./Shell";

function Counter({ name }: { name: string }) {
  const [n, setN] = useState(0);
  return <button onClick={() => setN(n + 1)}>{name} count {n}</button>;
}
const list = (): Workspace[] =>
  ["Workflow", "Run", "Trace", "Providers"].map((label) => ({ id: label.toLowerCase(), label, render: () => <Counter name={label} /> }));

describe("shell", () => {
  it("shows the four workspaces and only the active one", () => {
    render(<Shell workspaces={list()} />);
    const nav = screen.getByRole("navigation", { name: "Workspaces" });
    expect(within(nav).getAllByRole("button").map((b) => b.textContent)).toEqual(["Workflow", "Run", "Trace", "Providers"]);
    expect(screen.getByRole("region", { name: "Workflow" })).toBeVisible();
    expect(document.querySelector('section[aria-label="Run"]')).not.toBeVisible();
  });

  it("registers a fifth workspace with no shell change", () => {
    render(<Shell workspaces={[...list(), { id: "replay", label: "Replay and Compare", render: () => <p>replay</p> }]} />);
    expect(screen.getByRole("button", { name: "Replay and Compare" })).toBeInTheDocument();
  });

  it("keeps a workspace's state when the user switches away and back", async () => {
    const user = userEvent.setup();
    render(<Shell workspaces={list()} />);
    await user.click(screen.getByRole("button", { name: "Workflow count 0" }));
    await user.click(screen.getByRole("button", { name: "Trace" }));
    expect(screen.getByRole("button", { name: "Trace count 0" })).toBeVisible();
    await user.click(screen.getByRole("button", { name: "Workflow" }));
    expect(screen.getByRole("button", { name: "Workflow count 1" })).toBeVisible();
  });

  it("restores the active workspace after a restart", async () => {
    const user = userEvent.setup();
    const first = render(<Shell workspaces={list()} />);
    await user.click(screen.getByRole("button", { name: "Providers" }));
    first.unmount();
    render(<Shell workspaces={list()} />);
    expect(screen.getByRole("button", { name: "Providers" })).toHaveAttribute("aria-current", "page");
  });

  it("falls back to the first workspace when the saved one is gone or corrupt", () => {
    window.localStorage.setItem("aibus.desktop.v1.workspace", JSON.stringify("removed"));
    render(<Shell workspaces={list()} />);
    expect(screen.getByRole("button", { name: "Workflow" })).toHaveAttribute("aria-current", "page");
    window.localStorage.setItem("aibus.desktop.v1.workspace", "{not json");
    expect(loadPref("workspace", "x")).toBe("x");
  });

  it("switches with Ctrl+number, arrow keys and toggles the theme from the keyboard", async () => {
    const user = userEvent.setup();
    render(<Shell workspaces={list()} />);
    await user.keyboard("{Control>}2{/Control}");
    expect(screen.getByRole("button", { name: "Run" })).toHaveAttribute("aria-current", "page");
    expect(screen.getByRole("button", { name: "Run" })).toHaveFocus();
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("button", { name: "Trace" })).toHaveAttribute("aria-current", "page");
    await user.keyboard("{Meta>}{Shift>}L{/Shift}{/Meta}");
    expect(document.documentElement.dataset.theme).toMatch(/light|dark/);
    const first = document.documentElement.dataset.theme;
    await user.keyboard("{Control>}{Shift>}L{/Shift}{/Control}");
    expect(document.documentElement.dataset.theme).not.toBe(first);
    expect(loadPref("theme", "system")).toBe(document.documentElement.dataset.theme);
  });

  it("lists the shortcuts on request", async () => {
    const user = userEvent.setup();
    render(<Shell workspaces={list()} />);
    expect(screen.queryByRole("region", { name: "Keyboard shortcuts" })).toBeNull();
    await user.keyboard("{Control>}/{/Control}");
    expect(screen.getByRole("region", { name: "Keyboard shortcuts" })).toBeVisible();
  });
});
