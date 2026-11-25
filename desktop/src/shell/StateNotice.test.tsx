// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { AuthFailedNotice, MissingCliNotice, StateNotice } from "./StateNotice";

describe("state notices", () => {
  it("announces a missing provider CLI as an alert, named by its title", () => {
    render(<MissingCliNotice provider="Claude Code" command="claude" />);
    const alert = screen.getByRole("alert", { name: "Claude Code CLI not found" });
    expect(alert).toHaveTextContent("claude");
  });

  it("announces a failed authorization as an alert", () => {
    render(<AuthFailedNotice provider="Codex" hint="Run codex login in a terminal, then check again." />);
    expect(screen.getByRole("alert", { name: "Codex sign-in failed" })).toHaveTextContent("codex login");
  });

  it("an empty state is a status, not an alert", () => {
    render(<StateNotice kind="empty" title="Nothing here">Try again later.</StateNotice>);
    expect(screen.getByRole("status", { name: "Nothing here" })).toBeInTheDocument();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
