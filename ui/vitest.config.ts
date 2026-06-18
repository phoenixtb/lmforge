import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { svelteTesting } from '@testing-library/svelte/vite';
import { fileURLToPath } from 'node:url';

// Standalone config for component unit tests. We use the plain `svelte()` plugin
// (not `sveltekit()`) so jsdom-based tests don't drag in the SvelteKit server
// runtime; `$app/*` modules are mocked per-test.
export default defineConfig({
  plugins: [svelte(), svelteTesting()],
  resolve: {
    alias: {
      $lib: fileURLToPath(new URL('./src/lib', import.meta.url)),
      '$app/navigation': fileURLToPath(
        new URL('./src/test-stubs/app-navigation.ts', import.meta.url),
      ),
    },
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./vitest-setup.ts'],
    include: ['src/**/*.{test,spec}.{js,ts}'],
  },
});
