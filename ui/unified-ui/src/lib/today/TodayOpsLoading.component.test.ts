import { render, screen } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import Economics from './TodayOpsEconomicsSlide.svelte';
import Operations from './TodayOpsStateSlide.svelte';

describe('Operations first-load states', () => {
    it('shows loading rather than invented zero spend', () => {
        const { container } = render(Economics, { props: { loading: true } });
        expect(screen.getByRole('status')).toHaveTextContent('Loading model spend');
        expect(container.querySelector('.np-spend-dollars')).toBeNull();
    });
    it('shows a failure rather than zero spend when no slice was obtained', () => {
        render(Economics, { props: { unavailable: true } });
        expect(screen.getByRole('status')).toHaveTextContent('temporarily unavailable');
    });
    it('keeps the empty operations state hidden until tasks load', () => {
        render(Operations, { props: { tasks: [], agents: [], nowMs: 0, tasksLoading: true } });
        expect(screen.getByRole('status')).toHaveTextContent('Loading operations');
        expect(screen.queryByText('No tasks yet today.')).not.toBeInTheDocument();
    });
});
