import { browser } from '$app/environment';
import { writable, type Readable } from 'svelte/store';

import {
	AudioNoteRequestError,
	uploadAudioNote,
	type AudioNoteReceipt,
	type AudioNoteUpload
} from './audioNotes';

const DATABASE_NAME = 'magician-audio-notes';
const DATABASE_VERSION = 1;
const STORE_NAME = 'outbox';
const MAX_RECORDS = 50;
const MAX_TOTAL_BYTES = 200 * 1024 * 1024;
const MAX_RECORD_BYTES = 24 * 1024 * 1024;
const MAX_ATTEMPTS = 5;
const RETRY_DELAYS_MS = [2_000, 10_000, 30_000, 120_000, 300_000];
const MAX_TRANSCRIPT_WAIT_MS = 10 * 60 * 1000;
const UPLOAD_LEASE_MS = 2 * 60 * 1000;

export type AudioNoteOutboxState = 'waiting_for_transcript' | 'pending' | 'retry_wait' | 'failed';

export interface AudioNoteOutboxRecord extends AudioNoteUpload {
	createdAt: number;
	updatedAt: number;
	state: AudioNoteOutboxState;
	attempts: number;
	nextAttemptAt: number;
	lastError: string | null;
	transcriptDeadlineAt?: number;
	leaseOwner?: string | null;
	leaseUntil?: number;
}

export interface AudioNoteOutboxStatus {
	id: string;
	capturedAt: string;
	transcript: string | null;
	durationMs: number;
	bytes: number;
	state: 'Waiting for transcript' | 'Pending' | 'Uploading' | 'Retrying' | 'Needs attention';
	detail: string | null;
	canRetry: boolean;
	canDiscard: boolean;
}

export interface AudioNoteOutboxStorage {
	list(): Promise<AudioNoteOutboxRecord[]>;
	get(noteId: string): Promise<AudioNoteOutboxRecord | null>;
	put(record: AudioNoteOutboxRecord): Promise<void>;
	delete(noteId: string): Promise<void>;
	claim(noteId: string, owner: string, now: number, leaseMs: number): Promise<AudioNoteOutboxRecord | null>;
}

export type AudioNoteUploader = (note: AudioNoteUpload) => Promise<AudioNoteReceipt>;

function requestResult<T>(request: IDBRequest<T>): Promise<T> {
	return new Promise((resolve, reject) => {
		request.onsuccess = () => resolve(request.result);
		request.onerror = () => reject(request.error ?? new Error('IndexedDB request failed'));
	});
}

function transactionComplete(transaction: IDBTransaction): Promise<void> {
	return new Promise((resolve, reject) => {
		transaction.oncomplete = () => resolve();
		transaction.onabort = () => reject(transaction.error ?? new Error('IndexedDB transaction aborted'));
		transaction.onerror = () => reject(transaction.error ?? new Error('IndexedDB transaction failed'));
	});
}

class IndexedDbAudioNoteOutboxStorage implements AudioNoteOutboxStorage {
	private databasePromise: Promise<IDBDatabase> | null = null;

	private database(): Promise<IDBDatabase> {
		if (!browser || typeof indexedDB === 'undefined') {
			return Promise.reject(new Error('Durable browser storage is unavailable'));
		}
		if (!this.databasePromise) {
			this.databasePromise = new Promise((resolve, reject) => {
				const request = indexedDB.open(DATABASE_NAME, DATABASE_VERSION);
				request.onupgradeneeded = () => {
					const database = request.result;
					if (!database.objectStoreNames.contains(STORE_NAME)) {
						const store = database.createObjectStore(STORE_NAME, { keyPath: 'noteId' });
						store.createIndex('createdAt', 'createdAt');
					}
				};
				request.onsuccess = () => resolve(request.result);
				request.onerror = () => {
					this.databasePromise = null;
					reject(request.error ?? new Error('Could not open Audio Notes storage'));
				};
				request.onblocked = () => {
					this.databasePromise = null;
					reject(new Error('Audio Notes storage upgrade is blocked by another tab'));
				};
			});
		}
		return this.databasePromise;
	}

	async list(): Promise<AudioNoteOutboxRecord[]> {
		const database = await this.database();
		const transaction = database.transaction(STORE_NAME, 'readonly');
		const completed = transactionComplete(transaction);
		const [records] = await Promise.all([
			requestResult(
				transaction.objectStore(STORE_NAME).getAll() as IDBRequest<AudioNoteOutboxRecord[]>
			),
			completed
		]);
		return records.sort((left, right) => right.createdAt - left.createdAt);
	}

	async get(noteId: string): Promise<AudioNoteOutboxRecord | null> {
		const database = await this.database();
		const transaction = database.transaction(STORE_NAME, 'readonly');
		const completed = transactionComplete(transaction);
		const [value] = await Promise.all([
			requestResult(
				transaction.objectStore(STORE_NAME).get(noteId) as IDBRequest<AudioNoteOutboxRecord | undefined>
			),
			completed
		]);
		return value ?? null;
	}

	async put(record: AudioNoteOutboxRecord): Promise<void> {
		const database = await this.database();
		const transaction = database.transaction(STORE_NAME, 'readwrite');
		const completed = transactionComplete(transaction);
		transaction.objectStore(STORE_NAME).put(record);
		await completed;
	}

	async delete(noteId: string): Promise<void> {
		const database = await this.database();
		const transaction = database.transaction(STORE_NAME, 'readwrite');
		const completed = transactionComplete(transaction);
		transaction.objectStore(STORE_NAME).delete(noteId);
		await completed;
	}

	async claim(
		noteId: string,
		owner: string,
		now: number,
		leaseMs: number
	): Promise<AudioNoteOutboxRecord | null> {
		const database = await this.database();
		const transaction = database.transaction(STORE_NAME, 'readwrite');
		const completed = transactionComplete(transaction);
		const store = transaction.objectStore(STORE_NAME);
		let record: AudioNoteOutboxRecord | undefined;
		try {
			record = await requestResult(
				store.get(noteId) as IDBRequest<AudioNoteOutboxRecord | undefined>
			);
		} catch (error) {
			await completed.catch(() => undefined);
			throw error;
		}
		if (!record || ((record.leaseUntil ?? 0) > now && record.leaseOwner !== owner)) {
			await completed;
			return null;
		}
		const claimed = {
			...record,
			leaseOwner: owner,
			leaseUntil: now + leaseMs,
			updatedAt: now
		};
		store.put(claimed);
		await completed;
		return claimed;
	}
}

function newNoteId(): string {
	if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
		return crypto.randomUUID();
	}
	throw new Error('This browser cannot generate a private Audio Note identifier');
}

function newWorkerId(): string {
	if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
		return `worker-${crypto.randomUUID()}`;
	}
	return `worker-${Date.now()}-${Math.random().toString(36).slice(2)}`;
}

function retryDelay(attempts: number): number {
	return RETRY_DELAYS_MS[Math.min(RETRY_DELAYS_MS.length - 1, Math.max(0, attempts - 1))];
}

function hasActiveLease(record: AudioNoteOutboxRecord, now: number): boolean {
	return (record.leaseUntil ?? 0) > now;
}

function statusFor(
	record: AudioNoteOutboxRecord,
	uploadingId: string | null,
	now: number
): AudioNoteOutboxStatus {
	let state: AudioNoteOutboxStatus['state'];
	if (record.noteId === uploadingId || hasActiveLease(record, now)) state = 'Uploading';
	else if (record.state === 'waiting_for_transcript') state = 'Waiting for transcript';
	else if (record.state === 'retry_wait') state = 'Retrying';
	else if (record.state === 'failed') state = 'Needs attention';
	else state = 'Pending';
	return {
		id: record.noteId,
		capturedAt: record.capturedAt,
		transcript: record.transcript,
		durationMs: record.durationMs,
		bytes: record.audio.size,
		state,
		detail: record.lastError,
		canRetry: record.state === 'failed' && !hasActiveLease(record, now),
		canDiscard: !hasActiveLease(record, now)
	};
}

export class AudioNoteOutboxQueue implements Readable<AudioNoteOutboxStatus[]> {
	private readonly stateStore = writable<AudioNoteOutboxStatus[]>([]);
	private started = false;
	private flushing = false;
	private uploadingId: string | null = null;
	private retryTimer: ReturnType<typeof setTimeout> | null = null;
	private onlineHandler: (() => void) | null = null;
	private visibilityHandler: (() => void) | null = null;
	private readonly workerId: string;
	private lifecycleGeneration = 0;
	private crossTabChannel: BroadcastChannel | null = null;

	constructor(
		private readonly storage: AudioNoteOutboxStorage,
		private readonly uploader: AudioNoteUploader = uploadAudioNote,
		private readonly now: () => number = Date.now,
		workerId?: string
	) {
		this.workerId = workerId ?? newWorkerId();
	}

	subscribe = this.stateStore.subscribe;

	async start(): Promise<void> {
		if (this.started) return;
		this.started = true;
		const generation = ++this.lifecycleGeneration;
		try {
			await this.recoverInterruptedCapture();
			await this.refresh();
			if (!this.started || generation !== this.lifecycleGeneration) return;
			if (browser) {
				if (typeof BroadcastChannel !== 'undefined') {
					this.crossTabChannel = new BroadcastChannel(DATABASE_NAME);
					this.crossTabChannel.onmessage = () => {
						void this.refresh();
						void this.flush();
					};
				}
				this.onlineHandler = () => void this.flush();
				this.visibilityHandler = () => {
					if (document.visibilityState === 'visible') void this.flush();
				};
				window.addEventListener('online', this.onlineHandler);
				document.addEventListener('visibilitychange', this.visibilityHandler);
			}
			void this.flush();
		} catch (error) {
			if (generation === this.lifecycleGeneration) this.started = false;
			throw error;
		}
	}

	stop(): void {
		if (this.onlineHandler && browser) window.removeEventListener('online', this.onlineHandler);
		if (this.visibilityHandler && browser) {
			document.removeEventListener('visibilitychange', this.visibilityHandler);
		}
		this.onlineHandler = null;
		this.visibilityHandler = null;
		this.crossTabChannel?.close();
		this.crossTabChannel = null;
		if (this.retryTimer) clearTimeout(this.retryTimer);
		this.retryTimer = null;
		this.started = false;
		this.lifecycleGeneration += 1;
	}

	async stage(input: {
		audio: Blob;
		originalFilename: string;
		mimeType: string;
		durationMs: number;
		capturedAt?: string;
		sourceSurface?: string;
	}): Promise<string> {
		await this.start();
		if (input.audio.size > MAX_RECORD_BYTES) {
			throw new Error('This recording is larger than the 24 MB Audio Notes limit.');
		}
		const existing = await this.storage.list();
		const totalBytes = existing.reduce((total, record) => total + record.audio.size, 0);
		if (existing.length >= MAX_RECORDS || totalBytes + input.audio.size > MAX_TOTAL_BYTES) {
			throw new Error('The Audio Notes outbox is full. Open Audio Notes to retry or discard pending recordings.');
		}
		const now = this.now();
		const noteId = newNoteId();
		await this.storage.put({
			noteId,
			capturedAt: input.capturedAt ?? new Date(now).toISOString(),
			sourceSurface: input.sourceSurface ?? 'web_chat_dictation',
			transcript: null,
			originalFilename: input.originalFilename,
			mimeType: input.mimeType,
			durationMs: input.durationMs,
			audio: input.audio,
			createdAt: now,
			updatedAt: now,
			state: 'waiting_for_transcript',
			attempts: 0,
			nextAttemptAt: 0,
			lastError: null,
			transcriptDeadlineAt: now + MAX_TRANSCRIPT_WAIT_MS,
			leaseOwner: null,
			leaseUntil: 0
		});
		await this.refresh();
		this.announceMutation();
		return noteId;
	}

	async finalize(noteId: string, transcript: string | null): Promise<void> {
		const record = await this.storage.get(noteId);
		if (!record) return;
		await this.storage.put({
			...record,
			transcript: transcript?.trim() || null,
			state: 'pending',
			updatedAt: this.now(),
			nextAttemptAt: 0,
			lastError: null,
			leaseOwner: null,
			leaseUntil: 0
		});
		await this.refresh();
		this.announceMutation();
		void this.flush();
	}

	async retry(noteId: string): Promise<void> {
		const record = await this.storage.get(noteId);
		if (!record || hasActiveLease(record, this.now())) return;
		await this.storage.put({
			...record,
			state: 'pending',
			attempts: 0,
			nextAttemptAt: 0,
			lastError: null,
			updatedAt: this.now(),
			leaseOwner: null,
			leaseUntil: 0
		});
		await this.refresh();
		this.announceMutation();
		void this.flush();
	}

	async discard(noteId: string): Promise<void> {
		const record = await this.storage.get(noteId);
		if (!record || hasActiveLease(record, this.now())) return;
		await this.storage.delete(noteId);
		await this.refresh();
		this.announceMutation();
	}

	async recording(noteId: string): Promise<Blob | null> {
		return (await this.storage.get(noteId))?.audio ?? null;
	}

	async refresh(): Promise<void> {
		const records = await this.storage.list();
		const now = this.now();
		this.stateStore.set(records.map((record) => statusFor(record, this.uploadingId, now)));
	}

	async flush(): Promise<void> {
		if (this.flushing) return;
		this.flushing = true;
		try {
			while (true) {
				await this.recoverInterruptedCapture();
				const records = await this.storage.list();
				const now = this.now();
				const next = records
					.filter((record) =>
						(record.state === 'pending'
							|| (record.state === 'retry_wait' && record.nextAttemptAt <= now))
						&& ((record.leaseUntil ?? 0) <= now || record.leaseOwner === this.workerId)
					)
					.sort((left, right) => left.createdAt - right.createdAt)[0];
				if (!next) break;
				const claimed = await this.storage.claim(
					next.noteId,
					this.workerId,
					now,
					UPLOAD_LEASE_MS
				);
				if (!claimed) continue;
				this.uploadingId = claimed.noteId;
				await this.refresh();
				try {
					await this.uploader(claimed);
					await this.storage.delete(claimed.noteId);
					this.announceMutation();
				} catch (error) {
					await this.recordFailure(claimed, error);
					this.announceMutation();
				}
				this.uploadingId = null;
				await this.refresh();
			}
		} finally {
			this.uploadingId = null;
			this.flushing = false;
			await this.refresh().catch(() => undefined);
			if (this.started) await this.scheduleRetry().catch(() => undefined);
		}
	}

	private async recoverInterruptedCapture(): Promise<void> {
		const records = await this.storage.list();
		const now = this.now();
		for (const record of records) {
			if (
				record.state !== 'waiting_for_transcript'
				|| (record.transcriptDeadlineAt ?? 0) > now
			) continue;
			await this.storage.put({
				...record,
				state: 'pending',
				lastError: 'Recovered after the page closed before transcription finished; saving audio without a transcript.',
				updatedAt: now,
				leaseOwner: null,
				leaseUntil: 0
			});
		}
	}

	private async recordFailure(record: AudioNoteOutboxRecord, error: unknown): Promise<void> {
		const requestError = error instanceof AudioNoteRequestError ? error : null;
		const attempts = record.attempts + 1;
		const retryable = requestError?.retryable ?? true;
		const exhausted = attempts >= MAX_ATTEMPTS;
		const message = error instanceof Error ? error.message : 'Audio Notes upload failed';
		await this.storage.put({
			...record,
			state: retryable && !exhausted ? 'retry_wait' : 'failed',
			attempts,
			nextAttemptAt: retryable && !exhausted ? this.now() + retryDelay(attempts) : 0,
			lastError: message,
			updatedAt: this.now(),
			leaseOwner: null,
			leaseUntil: 0
		});
	}

	private async scheduleRetry(): Promise<void> {
		if (this.retryTimer) clearTimeout(this.retryTimer);
		this.retryTimer = null;
		const now = this.now();
		const nextAt = (await this.storage.list()).flatMap((record) => {
			if (record.state === 'retry_wait') return [record.nextAttemptAt];
			if (record.state === 'waiting_for_transcript') {
				return [record.transcriptDeadlineAt ?? now];
			}
			if ((record.leaseUntil ?? 0) > now) return [record.leaseUntil ?? now];
			return [];
		}).sort((left, right) => left - right)[0];
		if (nextAt === undefined) return;
		const delay = Math.max(0, Math.min(300_000, nextAt - now));
		this.retryTimer = setTimeout(() => void this.flush(), delay);
	}

	private announceMutation(): void {
		this.crossTabChannel?.postMessage({ kind: 'outbox_changed' });
	}
}

export const audioNoteOutbox = new AudioNoteOutboxQueue(new IndexedDbAudioNoteOutboxStorage());
