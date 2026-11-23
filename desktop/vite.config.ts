/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves ./dist from its own origin; the fixed dev port is what tauri.conf.json's devUrl names.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { port: 5173, strictPort: true },
  build: { target: "es2021", sourcemap: false },
  test: { include: ["src/**/*.test.ts"] },
});
