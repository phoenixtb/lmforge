import { render, screen, fireEvent, within } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';

// `$app/navigation` is aliased to a stub in vitest.config.ts; mock the API
// client so we don't pull in the Tauri runtime.
const dismissLoadError = vi.fn((_id: string) => Promise.resolve({ status: 'dismissed' }));
vi.mock('$lib/api', () => ({
  dismissLoadError: (id: string) => dismissLoadError(id),
}));

import LoadErrorBanner from './LoadErrorBanner.svelte';
import { dismissedErrors } from '$lib/stores/dismissedErrors';

type Sev = 'user_error' | 'transient' | 'engine_bug';
function entry(
  modelId: string,
  opts: { at?: string; severity?: Sev; message?: string; count?: number } = {},
): [string, any] {
  return [
    modelId,
    {
      at: opts.at ?? '2026-06-18T10:00:00Z',
      message: opts.message ?? 'No .gguf file found',
      severity: opts.severity ?? 'user_error',
      stderr_tail: null,
      count: opts.count ?? 1,
    },
  ];
}

beforeEach(() => {
  sessionStorage.clear();
  dismissedErrors.set(new Set());
  dismissLoadError.mockClear();
});

describe('LoadErrorBanner', () => {
  it('renders a card per active error', () => {
    render(LoadErrorBanner, { errors: [entry('qwen3.5:3b:mtp:4bit')] });
    expect(screen.getByText('qwen3.5:3b:mtp:4bit')).toBeInTheDocument();
    expect(screen.getByRole('alert')).toBeInTheDocument();
  });

  it('dismiss (✕) removes the card and clears it on the daemon', async () => {
    render(LoadErrorBanner, { errors: [entry('qwen3.5:3b:mtp:4bit')] });

    await fireEvent.click(screen.getByLabelText('Dismiss this error'));

    expect(screen.queryByText('qwen3.5:3b:mtp:4bit')).not.toBeInTheDocument();
    expect(dismissLoadError).toHaveBeenCalledWith('qwen3.5:3b:mtp:4bit');
  });

  it('mute (–) collapses the card into a pill, unmute restores it', async () => {
    render(LoadErrorBanner, { errors: [entry('qwen3.5:3b:mtp:4bit')] });

    await fireEvent.click(screen.getByLabelText('Mute this error'));

    // Card (alert) gone; a muted pill is shown instead.
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
    const muted = screen.getByLabelText('Muted load errors');
    expect(within(muted).getByText('qwen3.5:3b:mtp:4bit')).toBeInTheDocument();

    // Clicking the pill un-mutes → card back, no daemon call (mute is client-only).
    await fireEvent.click(within(muted).getByText('qwen3.5:3b:mtp:4bit'));
    expect(screen.getByRole('alert')).toBeInTheDocument();
    expect(dismissLoadError).not.toHaveBeenCalled();
  });

  it('dismissal survives a status re-push of the same occurrence', async () => {
    const { rerender } = render(LoadErrorBanner, {
      errors: [entry('qwen3.5:3b:mtp:4bit', { at: 'T0' })],
    });

    await fireEvent.click(screen.getByLabelText('Dismiss this error'));
    expect(screen.queryByText('qwen3.5:3b:mtp:4bit')).not.toBeInTheDocument();

    // Daemon (pre-suppression) re-pushes the same occurrence (same `at`).
    await rerender({ errors: [entry('qwen3.5:3b:mtp:4bit', { at: 'T0' })] });
    expect(screen.queryByText('qwen3.5:3b:mtp:4bit')).not.toBeInTheDocument();
  });

  it('a NEW occurrence (new timestamp) re-appears after dismissal', async () => {
    const { rerender } = render(LoadErrorBanner, {
      errors: [entry('qwen3.5:3b:mtp:4bit', { at: 'T0' })],
    });

    await fireEvent.click(screen.getByLabelText('Dismiss this error'));
    expect(screen.queryByText('qwen3.5:3b:mtp:4bit')).not.toBeInTheDocument();

    // A genuinely new failure (different `at`) is a new occurrence → resurfaces.
    await rerender({ errors: [entry('qwen3.5:3b:mtp:4bit', { at: 'T1' })] });
    expect(screen.getByText('qwen3.5:3b:mtp:4bit')).toBeInTheDocument();
  });

  it('shows an occurrence badge only when count > 1', () => {
    const { rerender } = render(LoadErrorBanner, {
      errors: [entry('m', { count: 1 })],
    });
    expect(screen.queryByTitle(/occurrences/)).not.toBeInTheDocument();

    rerender({ errors: [entry('m', { count: 412 })] });
    expect(screen.getByText('412×')).toBeInTheDocument();
  });

  it('renders nothing when there are no errors', () => {
    const { container } = render(LoadErrorBanner, { errors: [] });
    expect(container.querySelector('.leb')).toBeNull();
  });
});
