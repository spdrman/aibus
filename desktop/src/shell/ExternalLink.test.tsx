// @vitest-environment jsdom
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ExternalLink } from "./ExternalLink";
import { safeExternalUrl } from "./safeUrl";

describe("safeExternalUrl", () => {
  it.each([
    ["https://example.com/docs?a=1#x", "https://example.com/docs?a=1#x"],
    ["  https://example.com  ", "https://example.com/"],
  ])("accepts %s", (input, out) => expect(safeExternalUrl(input)).toBe(out));

  it.each([
    "javascript:alert(1)", "data:text/html,<script>1</script>", "file:///etc/passwd", "http://example.com", "vscode://x/y",
    "https://user:pw@example.com", "https://user@example.com", "//example.com", "not a url", "",
    "https://exa\u202Emple.com", "https://example.com/\u0000", "https://example.com/\n",
  ])("rejects %j", (input) => expect(safeExternalUrl(input)).toBeNull());
});

describe("ExternalLink", () => {
  it("hands the normalised url to the opener and never navigates the webview", async () => {
    const open = vi.fn();
    render(<ExternalLink href="https://example.com/a" open={open}>Docs</ExternalLink>);
    const a = screen.getByRole("link", { name: /Docs/ });
    expect(a).toHaveAttribute("rel", "noopener noreferrer");
    await userEvent.setup().click(a);
    expect(open).toHaveBeenCalledWith("https://example.com/a");
  });

  it("renders an unsafe link as text with no anchor", () => {
    render(<ExternalLink href="javascript:alert(1)">Click me</ExternalLink>);
    expect(screen.queryByRole("link")).toBeNull();
    expect(screen.getByText("Click me")).toBeInTheDocument();
  });
});
