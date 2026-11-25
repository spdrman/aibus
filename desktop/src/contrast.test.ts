import { colorTokens, contrastRatio, type ColorToken, type Theme } from "@sharedui/design-tokens";
import { describe, expect, it } from "vitest";

// WCAG 2.x contrast of the token pairs styles.css actually uses, in both themes, measured from the
// shared tokens. The ratios are printed so the PR can quote them (npm test -- --reporter verbose).
// [foreground, background, minimum, where]: 4.5 for text, 3 for UI component edges and focus rings.
const pairs: [ColorToken, ColorToken, number, string][] = [
  ["text-primary", "bg-app", 4.5, "body text, panels"],
  ["text-primary", "bg-subtle", 4.5, "nav bar text"],
  ["text-primary", "surface-raised", 4.5, "notices, graph nodes"],
  ["text-primary", "surface-selected", 4.5, "provenance banner, selected row"],
  ["text-secondary", "bg-app", 4.5, "muted text, table headers"],
  ["text-secondary", "bg-subtle", 4.5, "inactive dock tabs"],
  ["text-secondary", "surface-raised", 4.5, "notice body, graph sub-labels"],
  ["text-link", "bg-app", 4.5, "links"],
  ["text-link", "bg-subtle", 4.5, "quiet nav buttons"],
  ["text-danger", "surface-raised", 4.5, "error notice title, failed nodes"],
  ["text-success", "surface-raised", 4.5, "completed nodes"],
  ["text-warning", "bg-app", 4.5, "warnings"],
  ["action-fg", "action-bg", 4.5, "active nav button"],
  ["action-fg", "action-bg", 4.5, "Replay button text on the banner"],
  ["action-bg", "surface-selected", 3, "Replay button edge on the banner"],
  ["border", "bg-app", 3, "table and notice edges"],
  ["border", "surface-raised", 3, "graph node edges"],
  ["border-danger", "surface-raised", 3, "error notice edge"],
  ["text-link", "surface-raised", 3, "selected node edge"],
  ["text-success", "surface-raised", 3, "completed node edge"],
  ["text-danger", "surface-raised", 3, "failed node edge"],
  ["focus-ring", "bg-app", 3, "focus ring"],
  ["focus-ring", "bg-subtle", 3, "focus ring on the nav bar"],
];

describe.each(["light", "dark"] as Theme[])("%s theme", (theme) => {
  it.each(pairs)("%s on %s is at least %s:1 (%s)", (fg, bg, min, where) => {
    const r = contrastRatio(colorTokens[theme][fg], colorTokens[theme][bg]);
    console.log(`${theme}\t${fg} ${colorTokens[theme][fg]} on ${bg} ${colorTokens[theme][bg]}\t${r.toFixed(2)}:1 (min ${min}, ${where})`);
    expect(r).toBeGreaterThanOrEqual(min);
  });
});
