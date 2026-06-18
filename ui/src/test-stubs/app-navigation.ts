// Stub for SvelteKit's `$app/navigation` so component tests resolve without the
// SvelteKit server runtime. Aliased in vitest.config.ts.
export const goto = (..._args: unknown[]): Promise<void> => Promise.resolve();
