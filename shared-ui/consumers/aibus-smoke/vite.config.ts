import { defineConfig } from 'vite';

// No React plugin on purpose: it pulls in Babel and caniuse-lite (CC-BY-4.0), which is outside the licence allow list.
export default defineConfig({ esbuild: { jsx: 'automatic' }, base: './' });
