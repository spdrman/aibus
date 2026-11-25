import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// Most test files run in node; only the ones that opt into jsdom have a window to reset.
afterEach(() => {
  if (typeof window === "undefined") return;
  cleanup();
  window.localStorage?.clear();
  document.documentElement.removeAttribute("data-theme");
});
