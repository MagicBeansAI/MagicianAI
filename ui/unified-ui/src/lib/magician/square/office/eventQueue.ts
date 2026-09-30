/**
 * Staggers proven floor events so a poll of five changes plays as a sequence,
 * not a burst. Caps the queue: if a poll lands forty events, the oldest
 * low-priority ones are dropped and logged rather than animating for minutes.
 */

import type { OfficeEvent } from './officeEvents';

const PRIORITY: Record<OfficeEvent['type'], number> = {
	blocked: 50,
	handoff: 40,
	'task-routed': 30,
	'social-talk': 25,
	'delivery-landed': 20,
	unblocked: 10,
	'work-started': 10,
	'work-ended': 10
};

export interface EventQueueOptions {
	intervalMs?: number;
	cap?: number;
	onRelease: (event: OfficeEvent) => void;
	onDrop?: (event: OfficeEvent) => void;
	log?: (message: string, event: OfficeEvent) => void;
}

export interface EventQueue {
	enqueue(events: readonly OfficeEvent[]): void;
	stop(): void;
	size(): number;
}

export function createEventQueue(options: EventQueueOptions): EventQueue {
	const intervalMs = options.intervalMs ?? 1_600;
	const cap = options.cap ?? 12;
	const onRelease = options.onRelease;
	const onDrop = options.onDrop;
	const log =
		options.log ??
		((message: string, event: OfficeEvent) => {
			console.info(message, event);
		});

	type Slot = { event: OfficeEvent; seq: number };
	const queue: Slot[] = [];
	let seq = 0;
	let timer: ReturnType<typeof setInterval> | null = null;

	const stopTimer = (): void => {
		if (timer == null) return;
		clearInterval(timer);
		timer = null;
	};

	const releaseNext = (): void => {
		const next = queue.shift();
		if (!next) {
			stopTimer();
			return;
		}
		onRelease(next.event);
		if (queue.length === 0) stopTimer();
	};

	const startTimer = (): void => {
		if (timer != null || queue.length === 0) return;
		timer = setInterval(releaseNext, intervalMs);
	};

	const trim = (): void => {
		while (queue.length > cap) {
			let dropAt = 0;
			let worst = PRIORITY[queue[0].event.type];
			for (let i = 1; i < queue.length; i++) {
				const priority = PRIORITY[queue[i].event.type];
				if (priority < worst || (priority === worst && queue[i].seq < queue[dropAt].seq)) {
					worst = priority;
					dropAt = i;
				}
			}
			const [dropped] = queue.splice(dropAt, 1);
			onDrop?.(dropped.event);
			log('[office] dropped queued floor event', dropped.event);
		}
	};

	const sort = (): void => {
		queue.sort((a, b) => {
			const delta = PRIORITY[b.event.type] - PRIORITY[a.event.type];
			return delta !== 0 ? delta : a.seq - b.seq;
		});
	};

	return {
		enqueue(events) {
			for (const event of events) {
				queue.push({ event, seq: seq++ });
			}
			trim();
			sort();
			startTimer();
		},
		stop() {
			queue.length = 0;
			stopTimer();
		},
		size() {
			return queue.length;
		}
	};
}
