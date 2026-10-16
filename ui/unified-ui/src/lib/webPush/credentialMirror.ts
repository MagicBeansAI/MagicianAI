/**
 * The session bearer, mirrored where the service worker can read it.
 *
 * The web app authenticates every `/api/magician/**` call with a bearer kept
 * in `localStorage` (`scopeIdentityStore`), injected by `installScopedApiFetch`.
 * There is no cookie session, and a service worker cannot read
 * `localStorage`, so while web push is enabled the page copies the bearer
 * into this same-origin IndexedDB record (same exposure as `localStorage`)
 * and deletes it on sign-out or when push is turned off. The worker reads it
 * only to fetch an ask and post an answer. Usable from page and worker: no
 * DOM, no `$app/*`.
 */

const DATABASE_NAME = 'magician-web-push';
const STORE_NAME = 'credentials';
const BEARER_KEY = 'scoped-bearer';

function factory(): IDBFactory | null {
	return typeof indexedDB === 'undefined' ? null : indexedDB;
}

function openDatabase(idb: IDBFactory): Promise<IDBDatabase> {
	return new Promise((resolve, reject) => {
		const request = idb.open(DATABASE_NAME, 1);
		request.onupgradeneeded = () => {
			if (!request.result.objectStoreNames.contains(STORE_NAME)) {
				request.result.createObjectStore(STORE_NAME);
			}
		};
		request.onsuccess = () => resolve(request.result);
		request.onerror = () => reject(request.error);
		request.onblocked = () => reject(new Error('web push credential store is blocked'));
	});
}

async function run<T>(
	mode: IDBTransactionMode,
	operation: (store: IDBObjectStore) => IDBRequest<T>
): Promise<T | undefined> {
	const idb = factory();
	if (!idb) return undefined;
	const database = await openDatabase(idb);
	try {
		return await new Promise<T>((resolve, reject) => {
			const transaction = database.transaction(STORE_NAME, mode);
			const request = operation(transaction.objectStore(STORE_NAME));
			transaction.oncomplete = () => resolve(request.result);
			transaction.onerror = () => reject(transaction.error);
			transaction.onabort = () => reject(transaction.error);
		});
	} finally {
		database.close();
	}
}

/** The mirrored bearer, or `''` when none is stored (or storage is unavailable). */
export async function readMirroredBearer(): Promise<string> {
	try {
		const value = await run('readonly', (store) => store.get(BEARER_KEY));
		return typeof value === 'string' ? value.trim() : '';
	} catch {
		return '';
	}
}

export async function writeMirroredBearer(token: string): Promise<void> {
	const trimmed = token.trim();
	if (!trimmed) return clearMirroredBearer();
	try {
		await run('readwrite', (store) => store.put(trimmed, BEARER_KEY));
	} catch {
		// Without the mirror the worker still shows generic notifications.
	}
}

export async function clearMirroredBearer(): Promise<void> {
	try {
		await run('readwrite', (store) => store.delete(BEARER_KEY));
	} catch {
		// Nothing stored or storage unavailable — nothing to clear.
	}
}
