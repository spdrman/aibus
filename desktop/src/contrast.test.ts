import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

// WCAG 2.x contrast of the token pairs in styles.css, in both themes. The ratios are printed so the
// PR can quote them (`npm test -- --reporter verbose`).
const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");

function tokens(block: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const m of block.matchAll(/--([a-z-]+):\s*(#[0-9a-f]{6})/gi)) out[m[1]] = m[2];
  return out;
}
const light = tokens(css.slice(css.indexOf(":root {"), css.indexOf("@media")));
const dark = tokens(css.slice(css.indexOf(':root[data-theme="dark"]')).split("\n")[0]);
const darkMedia = tokens(css.slice(css.indexOf("@media"), css.indexOf(':root[data-theme="dark"]')));

function lum(hex: string): number {
  const c = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4));
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
}
export function ratio(a: string, b: string): number {
  const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
}

// [foreground, background, minimum]: 4.5 for text, 3 for UI component edges.
const pairs: [string, string, number][] = [
  ["text", "bg", 4.5], ["text", "surface", 4.5], ["muted", "bg", 4.5], ["muted", "surface", 4.5],
  ["accent", "bg", 4.5], ["ok", "surface", 4.5], ["fail", "surface", 4.5], ["warn", "bg", 4.5],
  ["banner-text", "banner-bg", 4.5], ["tab-text", "tab-bg", 4.5], ["tab-active-text", "tab-active-bg", 4.5],
  ["border", "bg", 3], ["border", "surface", 3], ["accent", "surface", 3], ["ok", "surface", 3], ["fail", "surface", 3],
];

describe.each([["light", light], ["dark", dark], ["dark (media query)", darkMedia]] as const)("%s theme", (name, t) => {
  it.each(pairs)("%s on %s is at least %s:1", (fg, bg, min) => {
    const r = ratio(t[fg], t[bg]);
    console.log(`${name}\t${fg} ${t[fg]} on ${bg} ${t[bg]}\t${r.toFixed(2)}:1 (min ${min})`);
    expect(r).toBeGreaterThanOrEqual(min);
  });
});

describe("the two dark blocks agree", () => {
  it("has the same tokens", () => expect(darkMedia).toEqual(dark));
});
