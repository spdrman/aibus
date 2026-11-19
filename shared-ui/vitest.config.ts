import { defineConfig } from 'vitest/config';

export default defineConfig({
  esbuild: { jsx: 'automatic' },
  test: {
    environment: 'jsdom',
    setupFiles: ['./vitest.setup.ts'],
    include: ['packages/*/src/**/*.test.{ts,tsx}', 'consumers/*/src/**/*.test.{ts,tsx}'],
    globals: false,
  },
});
