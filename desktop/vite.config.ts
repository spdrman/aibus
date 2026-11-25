/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves ./dist from its own origin; the fixed dev port is what tauri.conf.json's devUrl names.
export default defineConfig({
  plugins: [react()],
  // The shared packages are linked from ../shared-ui, so without this they would load their own copy of React.
  resolve: { dedupe: ["react", "react-dom"] },
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "es2021", sourcemap: false },
  test: { include: ["src/**/*.test.{ts,tsx}"], setupFiles: ["./vitest.setup.ts"] },
});
