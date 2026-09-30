import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';

import MemoryEntryCard from './MemoryEntryCard.svelte';
import type { MemoryEntry } from './memoryEntry';

afterEach(() => cleanup());

function inferredEntry(): MemoryEntry {
	return {
		tier: 'preferences',
		key: 'avoid_vendor_calls',
		source_type: 'insight',
		trust: 'inferred',
		kind: 'normative',
		value: 'Avoid vendor calls this quarter'
	};
}

function untrustedEntry(): MemoryEntry {
	return {
		tier: 'screen_observations',
		key: 's1',
		source_type: 'screen_capture',
		trust: 'untrusted',
		kind: 'episodic'
	};
}

describe('MemoryEntryCard', () => {
	it('shows provenance and lets an inferred entry be confirmed', async () => {
		const onConfirm = vi.fn();
		render(MemoryEntryCard, { entry: inferredEntry(), onConfirm });
		expect(screen.getByText('Inferred')).toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
		expect(onConfirm).toHaveBeenCalledWith(
			expect.objectContaining({ key: 'avoid_vendor_calls' })
		);
	});

	it('surfaces a stated-vs-behaviour conflict instead of hiding it', () => {
		render(MemoryEntryCard, {
			entry: {
				...inferredEntry(),
				trust: 'stated',
				conflict: 'still applying preferences: avoid_vendor_calls — recent engagement disagrees'
			}
		});
		expect(
			screen.getByText(/still applying preferences: avoid_vendor_calls/)
		).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Keep' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Edit' })).toBeInTheDocument();
	});

	it('focuses the topics input when Edit is chosen on a conflict', async () => {
		render(MemoryEntryCard, {
			entry: {
				...inferredEntry(),
				trust: 'stated',
				conflict: 'still applying preferences: avoid_vendor_calls — recent engagement disagrees'
			},
			onSaveScope: vi.fn()
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
		expect(screen.getByLabelText('Topics')).toHaveFocus();
	});

	it('does not invent a behaviour ratio when the backend omitted counts', () => {
		render(MemoryEntryCard, {
			entry: {
				...inferredEntry(),
				trust: 'stated',
				conflict: 'still applying preferences: avoid_vendor_calls — recent engagement disagrees'
			}
		});
		expect(screen.queryByText(/opened anyway/)).not.toBeInTheDocument();
	});

	it('renders the behaviour ratio only when both counts arrive', () => {
		render(MemoryEntryCard, {
			entry: {
				...inferredEntry(),
				trust: 'stated',
				conflict: 'still applying preferences: avoid_vendor_calls — recent engagement disagrees',
				conflict_agree: 1,
				conflict_disagree: 4
			}
		});
		expect(screen.getByText('4/5 opened anyway')).toBeInTheDocument();
	});

	it('never offers confirmation for untrusted provenance', () => {
		render(MemoryEntryCard, { entry: untrustedEntry() });
		expect(screen.queryByRole('button', { name: 'Confirm' })).not.toBeInTheDocument();
	});

	it('offers Keep and Edit on an untrusted conflict without Confirm', async () => {
		const onConfirm = vi.fn();
		render(MemoryEntryCard, {
			entry: {
				...untrustedEntry(),
				conflict: 'still applying screen_observations: s1 — recent engagement disagrees'
			},
			onConfirm
		});
		expect(screen.getByRole('button', { name: 'Keep' })).toBeInTheDocument();
		expect(screen.getByRole('button', { name: 'Edit' })).toBeInTheDocument();
		expect(screen.queryByRole('button', { name: 'Confirm' })).not.toBeInTheDocument();
		await fireEvent.click(screen.getByRole('button', { name: 'Keep' }));
		expect(onConfirm).not.toHaveBeenCalled();
	});

	it('lets the owner edit scope and refuses to pretend an empty scope attaches', async () => {
		const onSaveScope = vi.fn();
		render(MemoryEntryCard, { entry: inferredEntry(), onSaveScope });
		expect(screen.getByText('No scope — will not attach')).toBeInTheDocument();
		await fireEvent.input(screen.getByLabelText('Topics'), { target: { value: 'vendor' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Save scope' }));
		expect(onSaveScope).toHaveBeenCalledWith(
			expect.objectContaining({ key: 'avoid_vendor_calls' }),
			expect.objectContaining({ topics: ['vendor'], entities: [], applies_to: [] })
		);
	});

	it('lets an inferred research finding be confirmed', async () => {
		const onConfirm = vi.fn();
		render(MemoryEntryCard, {
			entry: {
				tier: 'research_findings',
				key: 'r1',
				source_type: 'insight',
				trust: 'inferred',
				kind: 'factual',
				value: 'Owner said vendor first'
			},
			onConfirm
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
		expect(onConfirm).toHaveBeenCalledWith(expect.objectContaining({ key: 'r1' }));
	});

	it('does not offer confirm on agent workflow rows', () => {
		render(MemoryEntryCard, {
			entry: {
				tier: 'workflows',
				key: 'w1',
				source_type: 'insight',
				trust: 'inferred',
				kind: 'procedural'
			},
			onConfirm: vi.fn()
		});
		expect(screen.queryByRole('button', { name: 'Confirm' })).not.toBeInTheDocument();
	});

	it('does not wipe an existing server scope when the editor is cleared', async () => {
		const onSaveScope = vi.fn();
		render(MemoryEntryCard, {
			entry: {
				...inferredEntry(),
				scope: { topics: ['vendor'], entities: [], applies_to: [] }
			},
			onSaveScope
		});
		await fireEvent.input(screen.getByLabelText('Topics'), { target: { value: '' } });
		await fireEvent.click(screen.getByRole('button', { name: 'Save scope' }));
		expect(onSaveScope).not.toHaveBeenCalled();
	});

	it('keeps an inferred conflict by confirming the written memory', async () => {
		const onConfirm = vi.fn();
		render(MemoryEntryCard, {
			entry: {
				...inferredEntry(),
				conflict: 'still applying preferences: avoid_vendor_calls — recent engagement disagrees'
			},
			onConfirm
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Keep' }));
		expect(onConfirm).toHaveBeenCalledWith(
			expect.objectContaining({ key: 'avoid_vendor_calls', trust: 'inferred' })
		);
		expect(screen.queryByRole('status')).not.toBeInTheDocument();
	});

	it('keeps a stated conflict without promoting trust', async () => {
		const onConfirm = vi.fn();
		const onKeepConflict = vi.fn();
		render(MemoryEntryCard, {
			entry: {
				...inferredEntry(),
				trust: 'stated',
				conflict: 'still applying preferences: avoid_vendor_calls — recent engagement disagrees'
			},
			onConfirm,
			onKeepConflict
		});
		await fireEvent.click(screen.getByRole('button', { name: 'Keep' }));
		expect(onConfirm).not.toHaveBeenCalled();
		expect(onKeepConflict).toHaveBeenCalledWith(
			expect.objectContaining({ key: 'avoid_vendor_calls', trust: 'stated' })
		);
		expect(screen.queryByRole('status')).not.toBeInTheDocument();
	});
});
