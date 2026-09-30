import { fireEvent, render, screen, within } from '@testing-library/svelte';
import { afterEach, describe, expect, it, vi } from 'vitest';
import TodayNewspaperLedger from './TodayNewspaperLedger.svelte';
import type { LlmPulse } from './pulseQueries';
import type { Task } from '$lib/stores/taskStore';
import type { AgentSummary } from '$lib/stores/agentStore';

describe('TodayNewspaperLedger', () => {
	const mockLlm: LlmPulse = {
		spendToday: 0.42,
		spendYesterday: 0.35,
		callsToday: 142,
		callsYesterday: 110,
		hourlySpend: [
			0, 0, 0, 0, 0, 0,
			0.02, 0.05, 0.08, 0.04, 0.06, 0.03,
			0.01, 0.04, 0.02, 0.03, 0.02, 0.01,
			0.01, 0, 0, 0, 0, 0
		],
		topProvider: {
			name: 'Anthropic',
			model: 'claude-3-5-sonnet',
			sharePct: 68
		}
	};

	const mockTasks: Task[] = [
		{ id: 't1', title: 'Task 1', status: 'completed', tags: [] } as unknown as Task,
		{ id: 't2', title: 'Task 2', status: 'completed', tags: [] } as unknown as Task,
		{ id: 't3', title: 'Task 3', status: 'completed', tags: [] } as unknown as Task,
		{ id: 't4', title: 'Task 4', status: 'failed', tags: [] } as unknown as Task,
		{ id: 't5', title: 'Task 5', status: 'running', tags: [] } as unknown as Task,
		{ id: 't6', title: 'Task 6', status: 'ready', tags: [] } as unknown as Task
	];

	const mockAgents: AgentSummary[] = [
		{ agent_id: 'a1', name: 'Agent 1', status: 'idle', disabled: false } as AgentSummary,
		{ agent_id: 'a2', name: 'Agent 2', status: 'running', disabled: false } as AgentSummary,
		{ agent_id: 'a3', name: 'Agent 3', status: 'disabled', disabled: true } as AgentSummary
	];

	it('renders daily LLM cost, delta vs yesterday, and calls count', () => {
		render(TodayNewspaperLedger, {
			props: {
				customLlm: mockLlm,
				customTasks: mockTasks,
				customAgents: mockAgents
			}
		});

		// Heading is the current slide's title
		expect(screen.getByRole('heading', { level: 2, name: 'Economics of Operations' })).toBeInTheDocument();
		expect(screen.getByText('Continuous real-time accounting')).toBeInTheDocument();

		// LLM Spend ($0.42)
		expect(screen.getAllByText('0').some((el) => el.classList.contains('np-spend-dollars'))).toBe(true);
		expect(screen.getByText('.42')).toBeInTheDocument();
		// Stat grid beside the chart.
		expect(screen.getByText('Avg / call')).toBeInTheDocument();
		expect(screen.getByText('Peak hour')).toBeInTheDocument();
		expect(screen.getByText('Coding runs')).toBeInTheDocument();
		expect(screen.getByText(/142/)).toBeInTheDocument();
		expect(screen.getByText(/model calls today/)).toBeInTheDocument();

		// Top provider with 2-decimal restricted share percentage
		expect(screen.getByText('Anthropic')).toBeInTheDocument();
		expect(screen.getByText(/claude-3-5-sonnet/)).toBeInTheDocument();
		expect(screen.getByText(/68%/)).toBeInTheDocument();
	});

	it('moves fleet yield, agent counts and Tasks / Agent Crew links to the State of Operations slide', async () => {
		const { container } = render(TodayNewspaperLedger, {
			props: { customLlm: mockLlm, customTasks: mockTasks, customAgents: mockAgents }
		});

		// Slide 2 is hidden from assistive tech until it is current.
		expect(screen.queryByRole('link', { name: /Agent Crew/ })).not.toBeInTheDocument();

		await fireEvent.click(screen.getByRole('button', { name: 'Slide 2 of 3, State of Operations' }));

		expect(screen.getByRole('link', { name: 'Tasks' })).toHaveAttribute('href', '/tasks');
		expect(screen.getByRole('link', { name: 'Agent Crew' })).toHaveAttribute('href', '/crew');

		// Agent counts: Enabled (2), Active (1), Total (3)
		const metricsText = container.querySelector('.fleet-metrics-list')?.textContent?.replace(/\s+/g, ' ');
		expect(metricsText).toContain('Enabled: 2');
		expect(metricsText).toContain('Active: 1');
		expect(metricsText).toContain('Total: 3');

		// Pie legend buckets: Active (running) 1, Succeeded 3, Failed 1
		const legend = screen.getByRole('list', { name: 'Task outcomes' }).textContent?.replace(/\s+/g, ' ');
		expect(legend).toContain('Active 1(20%)');
		expect(legend).toContain('Succeeded 3(60%)');
		expect(legend).toContain('Failed 1(20%)');

		expect(screen.queryByText('AGENT FLEET')).not.toBeInTheDocument();
		expect(screen.queryByText(/attempted/)).not.toBeInTheDocument();
	});

	it('formats top provider percentage with at most 2 decimal points', () => {
		const unroundedLlm: LlmPulse = {
			...mockLlm,
			topProvider: {
				name: 'openai',
				model: 'gpt-6-luna',
				sharePct: 49.53051643192488
			}
		};

		render(TodayNewspaperLedger, {
			props: {
				customLlm: unroundedLlm,
				customTasks: mockTasks,
				customAgents: mockAgents
			}
		});

		expect(screen.getByText('openai')).toBeInTheDocument();
		expect(screen.getByText(/gpt-6-luna/)).toBeInTheDocument();
		expect(screen.getByText(/49.53%/)).toBeInTheDocument();
	});

	it('shows cost and calls in hourly graph tooltips and on hover', async () => {
		const llmWithHourlyCalls: LlmPulse = {
			...mockLlm,
			hourlySpend: [0, 0, 0, 0, 0, 0, 0, 0, 0.42, ...new Array(15).fill(0)],
			hourlyCalls: [0, 0, 0, 0, 0, 0, 0, 0, 18, ...new Array(15).fill(0)]
		};

		const { container } = render(TodayNewspaperLedger, {
			props: {
				customLlm: llmWithHourlyCalls,
				customTasks: mockTasks,
				customAgents: mockAgents
			}
		});

		// Check SVG title contains cost + calls
		const titles = container.querySelectorAll('.np-bar-hit title');
		expect(titles.length).toBe(24);
		expect(titles[8]?.textContent).toContain('$0.42 · 18 calls');
	});

	describe('Operations carousel', () => {
		afterEach(() => {
			vi.useRealTimers();
		});

		const NOW = Date.parse('2026-09-28T12:00:00Z');
		const minutesAgo = (m: number) => new Date(NOW - m * 60_000).toISOString();
		const datedTasks: Task[] = Array.from({ length: 23 }, (_, i) => ({
			id: `task-${i}`,
			title: i === 22 ? 'A very long task title that should be ellipsised on one line in the ledger' : `Dated task ${i}`,
			status: i % 3 === 0 ? 'failed' : i % 3 === 1 ? 'completed' : 'running',
			tags: [],
			createdAt: minutesAgo(3000),
			updatedAt: minutesAgo(i === 22 ? 4 : 10 + i * 60)
		})) as unknown as Task[];

		// Titles cross-fade in one cell; only the current one is exposed to assistive tech.
		const currentTitle = () => {
			const heading = screen.getByRole('heading', { level: 2 });
			const visible = heading.querySelector('.is-current')?.textContent?.trim();
			expect(heading).toHaveAccessibleName(visible);
			return visible;
		};
		const carousel = (container: HTMLElement) => container.querySelector('[aria-roledescription="carousel"]') as HTMLElement;

		it('renders one dot per slide with accessible labels and marks the current one', () => {
			const { container } = render(TodayNewspaperLedger, {
				props: { customLlm: mockLlm, customTasks: mockTasks, customAgents: mockAgents }
			});
			expect(carousel(container)).toHaveAttribute('aria-label', 'Operations');
			const dots = [
				screen.getByRole('button', { name: 'Slide 1 of 3, Economics of Operations' }),
				screen.getByRole('button', { name: 'Slide 2 of 3, State of Operations' })
			];
			expect(dots[0]).toHaveAttribute('aria-current', 'true');
			expect(dots[1]).not.toHaveAttribute('aria-current');
			expect(container.querySelectorAll('[aria-roledescription="slide"]')).toHaveLength(3);
			expect(screen.getByRole('button', { name: 'Slide 3 of 3, State of the Crew' })).toBeInTheDocument();
		});

		it('changes the header title and announces the slide when a dot is clicked', async () => {
			const { container } = render(TodayNewspaperLedger, {
				props: { customLlm: mockLlm, customTasks: mockTasks, customAgents: mockAgents }
			});
			expect(currentTitle()).toBe('Economics of Operations');

			await fireEvent.click(screen.getByRole('button', { name: 'Slide 2 of 3, State of Operations' }));

			expect(currentTitle()).toBe('State of Operations');
			const live = container.querySelector('[aria-live]');
			expect(live).toHaveAttribute('aria-live', 'polite');
			expect(live?.textContent?.trim()).toBe('Slide 2 of 3, State of Operations');
			expect(screen.getByRole('button', { name: 'Slide 2 of 3, State of Operations' })).toHaveAttribute('aria-current', 'true');
		});

		it('lists the 20 most recently updated tasks as one-line rows linking to the task', async () => {
			vi.useFakeTimers({ now: NOW });
			render(TodayNewspaperLedger, {
				props: { customLlm: mockLlm, customTasks: datedTasks, customAgents: mockAgents }
			});
			await fireEvent.click(screen.getByRole('button', { name: 'Slide 2 of 3, State of Operations' }));

			const list = screen.getByRole('list', { name: 'Recently updated tasks' });
			const links = within(list).getAllByRole('link');
			expect(links).toHaveLength(20);

			// Most recently updated first: the long-titled task (4m ago), then task-0 (10m ago).
			expect(links[0]).toHaveAttribute('href', '/tasks?selected=task-22');
			expect(links[0].querySelector('.np-ops-task__title')?.textContent).toContain('A very long task title');
			expect(links[0].querySelector('.np-ops-task__age')?.textContent).toBe('4m');
			expect(links[0].querySelector('.np-ops-task__dot')).toHaveClass('is-success');
			expect(links[1]).toHaveAttribute('href', '/tasks?selected=task-0');
			expect(links[1].querySelector('.np-ops-task__dot')).toHaveClass('is-danger');
			expect(links[2].querySelector('.np-ops-task__age')?.textContent).toBe('1h');
		});

		it('shows the empty state when there are no tasks', async () => {
			render(TodayNewspaperLedger, {
				props: { customLlm: mockLlm, customTasks: [], customAgents: mockAgents }
			});
			await fireEvent.click(screen.getByRole('button', { name: 'Slide 2 of 3, State of Operations' }));
			expect(screen.getByText('No tasks yet today.')).toBeInTheDocument();
			expect(screen.getByText('IDLE')).toBeInTheDocument();
		});

		it('auto-advances every 8 seconds and wraps to the first slide', async () => {
			vi.useFakeTimers({ now: NOW });
			render(TodayNewspaperLedger, {
				props: { customLlm: mockLlm, customTasks: mockTasks, customAgents: mockAgents }
			});
			expect(currentTitle()).toBe('Economics of Operations');

			await vi.advanceTimersByTimeAsync(7_900);
			expect(currentTitle()).toBe('Economics of Operations');
			await vi.advanceTimersByTimeAsync(200);
			expect(currentTitle()).toBe('State of Operations');
			await vi.advanceTimersByTimeAsync(8_000);
			expect(currentTitle()).toBe('State of the Crew');
			await vi.advanceTimersByTimeAsync(8_000);
			expect(currentTitle()).toBe('Economics of Operations');
		});

		it('pauses auto-advance while hovered and resumes after the pointer leaves', async () => {
			vi.useFakeTimers({ now: NOW });
			const { container } = render(TodayNewspaperLedger, {
				props: { customLlm: mockLlm, customTasks: mockTasks, customAgents: mockAgents }
			});
			await fireEvent.mouseEnter(carousel(container));
			await vi.advanceTimersByTimeAsync(30_000);
			expect(currentTitle()).toBe('Economics of Operations');

			await fireEvent.mouseLeave(carousel(container));
			await vi.advanceTimersByTimeAsync(8_100);
			expect(currentTitle()).toBe('State of Operations');
		});

		it('holds auto-advance for 15 seconds after manual navigation', async () => {
			vi.useFakeTimers({ now: NOW });
			render(TodayNewspaperLedger, {
				props: { customLlm: mockLlm, customTasks: mockTasks, customAgents: mockAgents }
			});
			await fireEvent.click(screen.getByRole('button', { name: 'Slide 2 of 3, State of Operations' }));
			// The click focuses nothing in jsdom, but blur explicitly to rule out focus-within holding it.
			(document.activeElement as HTMLElement | null)?.blur?.();

			await vi.advanceTimersByTimeAsync(14_000);
			expect(currentTitle()).toBe('State of Operations');
			await vi.advanceTimersByTimeAsync(1_200);
			expect(currentTitle()).toBe('State of the Crew');
		});

		it('moves between slides with the arrow keys', async () => {
			const { container } = render(TodayNewspaperLedger, {
				props: { customLlm: mockLlm, customTasks: mockTasks, customAgents: mockAgents }
			});
			await fireEvent.keyDown(carousel(container), { key: 'ArrowRight' });
			expect(currentTitle()).toBe('State of Operations');
			await fireEvent.keyDown(carousel(container), { key: 'ArrowRight' });
			expect(currentTitle()).toBe('State of the Crew');
			await fireEvent.keyDown(carousel(container), { key: 'ArrowRight' });
			expect(currentTitle()).toBe('Economics of Operations');
			await fireEvent.keyDown(carousel(container), { key: 'ArrowLeft' });
			expect(currentTitle()).toBe('State of the Crew');
		});

		it('does not auto-advance under prefers-reduced-motion', async () => {
			vi.useFakeTimers({ now: NOW });
			const original = Object.getOwnPropertyDescriptor(window, 'matchMedia');
			const reduced = ((query: string) => ({
				matches: query.includes('prefers-reduced-motion'),
				media: query,
				onchange: null,
				addEventListener: () => {},
				removeEventListener: () => {},
				addListener: () => {},
				removeListener: () => {},
				dispatchEvent: () => true
			})) as unknown as typeof window.matchMedia;
			Object.defineProperty(window, 'matchMedia', { configurable: true, writable: true, value: reduced });
			try {
				render(TodayNewspaperLedger, {
					props: { customLlm: mockLlm, customTasks: mockTasks, customAgents: mockAgents }
				});
				await vi.advanceTimersByTimeAsync(30_000);
				expect(currentTitle()).toBe('Economics of Operations');
			} finally {
				if (original) Object.defineProperty(window, 'matchMedia', original);
			}
		});
	});

	const currentTitleOf = () => screen.getByRole('heading', { level: 2 }).querySelector('.is-current')?.textContent?.trim();

	describe('State of the Crew slide', () => {
		afterEach(() => {
			vi.useRealTimers();
		});

		const NOW = Date.parse('2026-09-28T12:00:00Z');
		const hoursAgo = (h: number) => new Date(NOW - h * 3_600_000).toISOString();
		const crewAgents: AgentSummary[] = [
			{ agent_id: 'pilot', name: 'Pilot', status: 'idle', disabled: false } as AgentSummary,
			{ agent_id: 'scout', name: 'Scout', status: 'running', disabled: false } as AgentSummary,
			{ agent_id: 'quiet', name: 'Quiet', status: 'idle', disabled: false } as AgentSummary
		];
		const crewTasks = [
			{ id: 'c1', title: 'Done', status: 'completed', agentId: 'pilot', updatedAt: hoursAgo(2), createdAt: hoursAgo(3), tags: [] },
			{ id: 'c2', title: 'Done 2', status: 'completed', agentId: 'pilot', updatedAt: hoursAgo(5), createdAt: hoursAgo(6), tags: [] },
			{ id: 'c3', title: 'Broke', status: 'failed', agentId: 'pilot', updatedAt: hoursAgo(1), createdAt: hoursAgo(2), tags: [] },
			{ id: 'c4', title: 'Old', status: 'completed', agentId: 'quiet', updatedAt: hoursAgo(30), createdAt: hoursAgo(31), tags: [] }
		] as unknown as Task[];

		it('lists active agents first with cost, calls, tasks, success and reliability', async () => {
			vi.useFakeTimers({ now: NOW });
			render(TodayNewspaperLedger, {
				props: {
					customLlm: mockLlm,
					customTasks: crewTasks,
					customAgents: crewAgents,
					customCrewLlm: [
						{ agentId: 'pilot', calls: 22, costUsd: 0.063, okCalls: 22, avgLatencyMs: 900 },
						{ agentId: 'scout', calls: 4, costUsd: 0.004, okCalls: 3, avgLatencyMs: 500 }
					]
				}
			});
			await fireEvent.click(screen.getByRole('button', { name: 'Slide 3 of 3, State of the Crew' }));
			expect(currentTitleOf()).toBe('State of the Crew');

			const totals = screen.getByRole('group', { name: 'Slide 3 of 3, State of the Crew' });
			const totalsText = within(totals).getByLabelText('Crew totals, last 24 hours').textContent?.replace(/\s+/g, ' ');
			expect(totalsText).toContain('Active 1/3');
			expect(totalsText).toContain('$0.07');
			expect(totalsText).toContain('Tasks 24h 2');
			expect(totalsText).toContain('96%');

			const rows = within(screen.getByRole('list', { name: 'Crew activity, last 24 hours' })).getAllByRole('link');
			// Scout is running (active first), then Pilot by cost; Quiet only has an out-of-window task.
			expect(rows).toHaveLength(2);
			expect(rows[0]).toHaveAttribute('href', '/crew/scout');
			expect(rows[0].textContent).toContain('Scout');
			expect(rows[0].textContent).toContain('Active');
			expect(rows[0].textContent).toContain('$0.0040');
			expect(rows[0].textContent).toContain('✓ —');
			expect(rows[0].textContent).toContain('⚡ 75%');
			expect(rows[1]).toHaveAttribute('href', '/crew/pilot');
			expect(rows[1].textContent).toContain('$0.06');
			expect(rows[1].textContent).toContain('22 calls');
			expect(rows[1].textContent).toContain('2 tasks');
			expect(rows[1].textContent).toContain('✓ 67%');
			expect(rows[1].textContent).toContain('⚡ 100%');
			expect(screen.getAllByRole('link', { name: 'Agent Crew' }).some((a) => a.getAttribute('href') === '/crew')).toBe(true);
		});

		it('says the crew is resting when nothing happened in the window', async () => {
			vi.useFakeTimers({ now: NOW });
			render(TodayNewspaperLedger, {
				props: {
					customLlm: mockLlm,
					customTasks: [],
					customAgents: [{ agent_id: 'pilot', name: 'Pilot', status: 'idle' } as AgentSummary]
				}
			});
			await fireEvent.click(screen.getByRole('button', { name: 'Slide 3 of 3, State of the Crew' }));
			expect(screen.getByText('The crew is resting — no activity in the last 24 hours.')).toBeInTheDocument();
		});
	});
});
