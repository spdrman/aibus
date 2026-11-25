import { useCallback, useEffect, useState } from "react";
import { loadPref, savePref } from "./prefs";

export type ThemeChoice = "system" | "light" | "dark";
const isChoice = (v: unknown): v is ThemeChoice => v === "system" || v === "light" || v === "dark";

// "system" leaves data-theme off, so tokens.css follows prefers-color-scheme; the other two force it.
export function applyTheme(choice: ThemeChoice): void {
  const root = document.documentElement;
  if (choice === "system") root.removeAttribute("data-theme");
  else root.dataset.theme = choice;
}

const systemDark = () => typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches;

export function useTheme() {
  const [choice, setChoice] = useState<ThemeChoice>(() => loadPref("theme", "system", isChoice));
  useEffect(() => {
    applyTheme(choice);
    savePref("theme", choice);
  }, [choice]);
  // The toggle flips what the user currently sees, so "system" resolves first.
  const toggle = useCallback(() => {
    setChoice((c) => ((c === "system" ? (systemDark() ? "dark" : "light") : c) === "dark" ? "light" : "dark"));
  }, []);
  return { choice, setChoice, toggle };
}
