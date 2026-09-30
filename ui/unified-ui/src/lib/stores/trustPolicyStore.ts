/**
 * Trust policy store for P3B-09 settings UI.
 * Handles load/update/restore for system/trust_policies.yaml.
 */

import { derived, get, writable } from 'svelte/store';
import { timedFetch } from '$lib/shared/fetch';

export interface TrustPolicyDocument {
	path: string;
	template_path: string;
	default_path: string;
	content: string;
	is_valid: boolean;
	validation_error?: string;
	validation_line?: number;
	validation_column?: number;
}

export interface TrustPolicyStoreState {
	isLoading: boolean;
	isSaving: boolean;
	isRestoring: boolean;
	error: string | null;
	validationError: string | null;
	validationLine: number | null;
	validationColumn: number | null;
	lastLoadedAt: number | null;
}

interface TrustPolicyMetaState {
	isLoading: boolean;
	isSaving: boolean;
	isRestoring: boolean;
	error: string | null;
	validationError: string | null;
	validationLine: number | null;
	validationColumn: number | null;
	lastLoadedAt: number | null;
}

interface ParsedApiError {
	message: string;
	code?: string;
	validationError?: string;
	validationLine?: number;
	validationColumn?: number;
}

const defaultMetaState: TrustPolicyMetaState = {
	isLoading: false,
	isSaving: false,
	isRestoring: false,
	error: null,
	validationError: null,
	validationLine: null,
	validationColumn: null,
	lastLoadedAt: null
};

const trustPolicyDocumentStore = writable<TrustPolicyDocument | null>(null);
const trustPolicyMeta = writable<TrustPolicyMetaState>(defaultMetaState);
export const trustPolicyDraft = writable<string>('');

function asRecord(value: unknown): Record<string, unknown> | null {
	return typeof value === 'object' && value !== null && !Array.isArray(value)
		? (value as Record<string, unknown>)
		: null;
}

function readString(payload: Record<string, unknown>, field: string): string | undefined {
	const value = payload[field];
	return typeof value === 'string' ? value : undefined;
}

function readNumber(payload: Record<string, unknown>, field: string): number | undefined {
	const value = payload[field];
	return typeof value === 'number' && Number.isFinite(value) ? value : undefined;
}

function parseTrustPolicyDocument(raw: unknown): TrustPolicyDocument | null {
	const root = asRecord(raw);
	if (!root) return null;

	const path = readString(root, 'path');
	const templatePath = readString(root, 'template_path');
	const defaultPath = readString(root, 'default_path');
	const content = readString(root, 'content');
	if (!path || !templatePath || !defaultPath || content === undefined) {
		return null;
	}

	return {
		path,
		template_path: templatePath,
		default_path: defaultPath,
		content,
		is_valid: root.is_valid === true,
		validation_error: readString(root, 'validation_error'),
		validation_line: readNumber(root, 'validation_line'),
		validation_column: readNumber(root, 'validation_column')
	};
}

function setValidationState(
	validationError: string | null,
	validationLine?: number,
	validationColumn?: number
): void {
	trustPolicyMeta.update((state) => ({
		...state,
		validationError,
		validationLine: validationLine ?? null,
		validationColumn: validationColumn ?? null
	}));
}

function syncValidationFromDocument(document: TrustPolicyDocument | null): void {
	if (!document || document.is_valid) {
		setValidationState(null);
		return;
	}
	setValidationState(
		document.validation_error || 'Trust policy file is invalid',
		document.validation_line,
		document.validation_column
	);
}

function applyDocument(document: TrustPolicyDocument): void {
	trustPolicyDocumentStore.set(document);
	trustPolicyDraft.set(document.content);
	syncValidationFromDocument(document);
}

async function readApiError(response: Response): Promise<ParsedApiError> {
	let message = `Request failed (${response.status})`;
	let code: string | undefined;
	let validationError: string | undefined;
	let validationLine: number | undefined;
	let validationColumn: number | undefined;

	try {
		const text = await response.text();
		if (!text) {
			return { message };
		}

		try {
			const parsed = JSON.parse(text) as unknown;
			const root = asRecord(parsed);
			const details = root ? asRecord(root.details) : null;
			const errorText = root ? readString(root, 'error') : undefined;
			const messageText = root ? readString(root, 'message') : undefined;
			code = root ? readString(root, 'code') : undefined;
			validationError = details ? readString(details, 'validation_error') : undefined;
			validationLine = details ? readNumber(details, 'line') : undefined;
			validationColumn = details ? readNumber(details, 'column') : undefined;
			message = `Request failed (${response.status}): ${errorText || messageText || text}`;
		} catch {
			message = `Request failed (${response.status}): ${text}`;
		}
	} catch {
		// Best effort only.
	}

	return {
		message,
		code,
		validationError,
		validationLine,
		validationColumn
	};
}

function beginOperation(key: 'isLoading' | 'isSaving' | 'isRestoring'): void {
	trustPolicyMeta.update((state) => ({
		...state,
		[key]: true,
		error: null
	}));
}

function endOperation(
	key: 'isLoading' | 'isSaving' | 'isRestoring',
	updates?: Partial<TrustPolicyMetaState>
): void {
	trustPolicyMeta.update((state) => ({
		...state,
		[key]: false,
		...(updates || {})
	}));
}

export const trustPolicyDocument = derived(
	trustPolicyDocumentStore,
	($document) => $document
);

export const trustPolicyHasUnsavedChanges = derived(
	[trustPolicyDocumentStore, trustPolicyDraft],
	([$document, $draft]) => ($document?.content ?? '') !== $draft
);

export const trustPolicyStoreState = derived(
	trustPolicyMeta,
	($meta): TrustPolicyStoreState => ({
		isLoading: $meta.isLoading,
		isSaving: $meta.isSaving,
		isRestoring: $meta.isRestoring,
		error: $meta.error,
		validationError: $meta.validationError,
		validationLine: $meta.validationLine,
		validationColumn: $meta.validationColumn,
		lastLoadedAt: $meta.lastLoadedAt
	})
);

export function setTrustPolicyDraftContent(content: string): void {
	trustPolicyDraft.set(content);
	setValidationState(null);
}

export function resetTrustPolicyDraftToPersisted(): void {
	const persisted = get(trustPolicyDocumentStore);
	trustPolicyDraft.set(persisted?.content ?? '');
	syncValidationFromDocument(persisted);
}

export function clearTrustPolicyError(): void {
	trustPolicyMeta.update((state) => ({
		...state,
		error: null
	}));
}

export async function loadTrustPolicy(): Promise<TrustPolicyDocument> {
	beginOperation('isLoading');
	setValidationState(null);
	try {
		const response = await timedFetch('/api/magician/v2/settings/trust-policy');
		if (!response.ok) {
			const apiError = await readApiError(response);
			endOperation('isLoading', {
				error: apiError.message
			});
			if (apiError.validationError) {
				setValidationState(
					apiError.validationError,
					apiError.validationLine,
					apiError.validationColumn
				);
			}
			throw new Error(apiError.message);
		}

		const payload = (await response.json()) as unknown;
		const document = parseTrustPolicyDocument(payload);
		if (!document) {
			throw new Error('Malformed trust policy payload');
		}

		applyDocument(document);
		endOperation('isLoading', {
			error: null,
			lastLoadedAt: Date.now()
		});
		return document;
	} catch (error) {
		const message = error instanceof Error ? error.message : 'Failed to load trust policy';
		endOperation('isLoading', { error: message });
		throw error;
	}
}

export async function saveTrustPolicy(content?: string): Promise<TrustPolicyDocument> {
	beginOperation('isSaving');
	setValidationState(null);
	try {
		const bodyContent = content ?? get(trustPolicyDraft);
		const response = await timedFetch('/api/magician/v2/settings/trust-policy', {
			method: 'PUT',
			headers: { 'Content-Type': 'application/json' },
			body: JSON.stringify({ content: bodyContent })
		});

		if (!response.ok) {
			const apiError = await readApiError(response);
			endOperation('isSaving', {
				error: apiError.message
			});
			if (apiError.validationError) {
				setValidationState(
					apiError.validationError,
					apiError.validationLine,
					apiError.validationColumn
				);
			}
			throw new Error(apiError.message);
		}

		const payload = (await response.json()) as unknown;
		const document = parseTrustPolicyDocument(payload);
		if (!document) {
			throw new Error('Malformed trust policy save response');
		}

		applyDocument(document);
		endOperation('isSaving', {
			error: null,
			lastLoadedAt: Date.now()
		});
		return document;
	} catch (error) {
		const message = error instanceof Error ? error.message : 'Failed to save trust policy';
		endOperation('isSaving', { error: message });
		throw error;
	}
}

export async function restoreTrustPolicyFromTemplate(): Promise<TrustPolicyDocument> {
	beginOperation('isRestoring');
	setValidationState(null);
	try {
		const response = await timedFetch('/api/magician/v2/settings/trust-policy/restore-template', {
			method: 'POST'
		});
		if (!response.ok) {
			const apiError = await readApiError(response);
			endOperation('isRestoring', {
				error: apiError.message
			});
			if (apiError.validationError) {
				setValidationState(
					apiError.validationError,
					apiError.validationLine,
					apiError.validationColumn
				);
			}
			throw new Error(apiError.message);
		}

		const payload = (await response.json()) as unknown;
		const document = parseTrustPolicyDocument(payload);
		if (!document) {
			throw new Error('Malformed trust policy restore response');
		}

		applyDocument(document);
		endOperation('isRestoring', {
			error: null,
			lastLoadedAt: Date.now()
		});
		return document;
	} catch (error) {
		const message = error instanceof Error ? error.message : 'Failed to restore trust policy';
		endOperation('isRestoring', { error: message });
		throw error;
	}
}
