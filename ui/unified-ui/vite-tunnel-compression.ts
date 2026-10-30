/**
 * Dev-server responses compressed for requests that arrive through the
 * Cloudflare tunnel (phones and other machines on `ui-<machine>`).
 *
 * Vite's dev server sends everything uncompressed. Cloudflare compresses at
 * its edge, but the bytes still cross the tunnel from this machine first —
 * the dev-mode Settings document alone was ~1 MB of inlined CSS, which took a
 * phone ~2.8 s to receive before any script could start (2026-10-25). Local
 * requests are left alone, and so are WebSocket upgrades (HMR), the
 * backend-proxied paths and streamed responses.
 */
import type { IncomingMessage, ServerResponse } from 'node:http';
import { brotliCompressSync, constants, gzipSync } from 'node:zlib';

const PROXIED = /^\/(?:api|health|ws|startup)(?:[/?]|$)/;
const COMPRESSIBLE = /^(?:text\/|application\/(?:javascript|json|xml|manifest\+json)|image\/svg\+xml)/i;
const MIN_BYTES = 1024;

type Next = (error?: unknown) => void;
type HeaderValue = number | string | readonly string[];

export function viaTunnel(req: IncomingMessage): boolean {
	return Boolean(req.headers['cf-ray'] || req.headers['cf-connecting-ip']);
}

export function chooseEncoding(acceptEncoding: string | string[] | undefined): 'br' | 'gzip' | null {
	const accepted = String(acceptEncoding ?? '').toLowerCase();
	if (/\bbr\b/.test(accepted)) return 'br';
	if (/\bgzip\b/.test(accepted)) return 'gzip';
	return null;
}

export function compress(body: Buffer, encoding: 'br' | 'gzip'): Buffer {
	return encoding === 'br'
		? brotliCompressSync(body, { params: { [constants.BROTLI_PARAM_QUALITY]: 4 } })
		: gzipSync(body, { level: 6 });
}

export function tunnelCompression(req: IncomingMessage, res: ServerResponse, next: Next): void {
	const encoding = chooseEncoding(req.headers['accept-encoding']);
	if (!encoding || !viaTunnel(req) || req.headers.upgrade || PROXIED.test(req.url ?? '') || req.method === 'HEAD') {
		next();
		return;
	}

	const originalWriteHead = res.writeHead.bind(res) as (...args: unknown[]) => ServerResponse;
	const originalWrite = res.write.bind(res) as (...args: unknown[]) => boolean;
	const originalEnd = res.end.bind(res) as (...args: unknown[]) => ServerResponse;
	const chunks: Buffer[] = [];
	let decided: 'buffer' | 'pass' | null = null;
	let pendingHead: unknown[] | null = null;

	const toBuffer = (chunk: unknown, enc?: unknown): Buffer =>
		Buffer.isBuffer(chunk)
			? chunk
			: typeof chunk === 'string'
				? Buffer.from(chunk, typeof enc === 'string' ? (enc as BufferEncoding) : undefined)
				: Buffer.from(chunk as Uint8Array);

	// writeHead fixes headers the moment it runs, so hold it until the body
	// shows whether it will be compressed; its headers move onto the response.
	res.writeHead = ((...args: unknown[]) => {
		const [status, second, third] = args;
		res.statusCode = Number(status);
		const headers = (typeof second === 'object' && second) || (typeof third === 'object' && third) || null;
		if (headers) {
			for (const [name, value] of Object.entries(headers as Record<string, HeaderValue>)) {
				if (value !== undefined) res.setHeader(name, value);
			}
		}
		if (typeof second === 'string') res.statusMessage = second;
		pendingHead = [];
		return res;
	}) as typeof res.writeHead;

	// Node sends headers implicitly through `this.writeHead`, so the original
	// must be back in place before any byte is handed to it.
	const flushHead = () => {
		res.writeHead = originalWriteHead as typeof res.writeHead;
		if (pendingHead) {
			pendingHead = null;
			originalWriteHead(res.statusCode);
		}
	};

	const decide = (): 'buffer' | 'pass' => {
		if (decided) return decided;
		const type = String(res.getHeader('content-type') ?? '');
		const already = res.getHeader('content-encoding');
		const streaming = /event-stream/i.test(type);
		// A byte range names offsets in the uncompressed body.
		const ranged = Boolean(req.headers.range) || res.statusCode === 206 || res.getHeader('content-range') !== undefined;
		decided = !already && !streaming && !ranged && COMPRESSIBLE.test(type) && res.statusCode !== 204 && res.statusCode !== 304
			? 'buffer'
			: 'pass';
		if (decided === 'pass') flushHead();
		return decided;
	};

	res.write = ((chunk: unknown, ...rest: unknown[]) => {
		if (decide() === 'pass') return originalWrite(chunk, ...rest);
		chunks.push(toBuffer(chunk, rest[0]));
		const callback = rest.find((arg) => typeof arg === 'function') as (() => void) | undefined;
		callback?.();
		return true;
	}) as typeof res.write;

	res.end = ((chunk?: unknown, ...rest: unknown[]) => {
		const callback = [chunk, ...rest].find((arg) => typeof arg === 'function') as (() => void) | undefined;
		const data = typeof chunk === 'function' ? undefined : chunk;
		if (decide() === 'pass') {
			return data === undefined ? originalEnd(callback) : originalEnd(data, ...rest);
		}
		if (data !== undefined && data !== null) chunks.push(toBuffer(data, rest[0]));
		const body = Buffer.concat(chunks);
		const vary = String(res.getHeader('vary') ?? '');
		if (!/(^|,)\s*(accept-encoding|\*)\s*(,|$)/i.test(vary)) {
			res.setHeader('Vary', vary ? `${vary}, Accept-Encoding` : 'Accept-Encoding');
		}
		if (body.length < MIN_BYTES) {
			res.setHeader('Content-Length', body.length);
			flushHead();
			return originalEnd(body, callback);
		}
		const packed = compress(body, encoding);
		res.removeHeader('Accept-Ranges');
		res.setHeader('Content-Encoding', encoding);
		res.setHeader('Content-Length', packed.length);
		// A strong validator would now name different bytes.
		const etag = res.getHeader('etag');
		if (typeof etag === 'string' && !etag.startsWith('W/')) res.setHeader('ETag', `W/${etag}`);
		flushHead();
		return originalEnd(packed, callback);
	}) as typeof res.end;

	next();
}
