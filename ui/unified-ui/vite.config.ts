/// <reference types="vitest/config" />
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig, type Plugin } from 'vite';
import pkg from './package.json';
import { existsSync } from 'fs';
import { join } from 'path';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { createConnection } from 'node:net';

const SOTA_ATTACHMENT_ROUTE = '/tests/sota-download-assets/case-b-attachment-report.csv';
const MAGICIAN_BACKEND_HOST = '127.0.0.1';
const MAGICIAN_BACKEND_PORT = 3002;
const MAGICIAN_BACKEND_ORIGIN = `http://${MAGICIAN_BACKEND_HOST}:${MAGICIAN_BACKEND_PORT}`;
const MAGICIAN_BACKEND_HEALTH_URL = `${MAGICIAN_BACKEND_ORIGIN}/health`;
const MAGICIAN_BACKEND_READY_PREFIXES = [
	'/api/magician',
	'/api/llm',
	'/api/local-resource-governor',
	'/health'
];
const MAGICIAN_BACKEND_PROBE_TIMEOUT_MS = 350;
const MAGICIAN_BACKEND_READY_TTL_MS = 2_000;
const MAGICIAN_BACKEND_DOWN_TTL_MS = 750;
// Cloudflare Tunnel zone for the dev-UI allow-list. The named tunnel
// (scripts/ensure-magician-tunnel.sh) exposes the dev server at
// `https://ui.<zone>/`; vite's DNS-rebinding protection blocks unknown Host
// headers, so we widen the allow-list to that zone. Override to match a custom
// MAGICIAN_TUNNEL_ZONE.
const MAGICIAN_TUNNEL_ZONE = process.env.MAGICIAN_TUNNEL_ZONE || 'magican.ai';
const SOTA_ATTACHMENT_BODY = [
	'report_id,vendor,status,amount_usd',
	'case-b-attachment-report,Northwind Parts,approved,18450',
	'case-b-attachment-report,Metro Freight,approved,9210',
	'case-b-attachment-report,Prairie Plastics,scheduled,6440'
].join('\n');

function handleSotaAttachmentRequest(req: { url?: string; method?: string }, res: { statusCode: number; setHeader: (name: string, value: string | number) => void; end: (body?: string) => void; }) {
	const url = req.url || '';
	const pathname = url.split('?')[0];
	if (pathname !== SOTA_ATTACHMENT_ROUTE) return false;
	if (req.method && req.method !== 'GET' && req.method !== 'HEAD') return false;

	res.statusCode = 200;
	res.setHeader('Content-Type', 'text/csv; charset=utf-8');
	res.setHeader('Content-Disposition', 'attachment; filename="case-b-attachment-report.csv"');
	res.setHeader('Cache-Control', 'no-store');
	res.setHeader('X-Sota-Download-Case', 'navigation-attachment');
	res.setHeader('Content-Length', Buffer.byteLength(SOTA_ATTACHMENT_BODY));

	if (req.method === 'HEAD') {
		res.end();
		return true;
	}

	res.end(SOTA_ATTACHMENT_BODY);
	return true;
}

function sotaAttachmentRoute(): Plugin {
	return {
		name: 'sota-attachment-route',
		configureServer(server) {
			server.middlewares.use((req, res, next) => {
				if (handleSotaAttachmentRequest(req, res)) return;
				next();
			});
		},
		configurePreviewServer(server) {
			server.middlewares.use((req, res, next) => {
				if (handleSotaAttachmentRequest(req, res)) return;
				next();
			});
		}
	};
}

type BackendReadinessState = {
	healthy: boolean;
	checkedAt: number;
	pending: Promise<boolean> | null;
	lastLoggedAt: number;
};

export function isMagicianBackendRoute(url?: string): boolean {
	if (!url) return false;
	const pathname = url.split('?')[0] || '';
	return MAGICIAN_BACKEND_READY_PREFIXES.some((prefix) => {
		return pathname === prefix || pathname.startsWith(`${prefix}/`);
	});
}

// A local Vite shell has a different origin from the mobile/public endpoint.
// Pin only its reviewed HTML frames to this proxy's own origin. Requests through
// a tunnel or from another machine retain the backend's configured policy.
export function localScriptedSurfaceCsp(
	csp: string,
	request: { url?: string; host?: string; remoteAddress?: string }
): string {
	if (!/^\/api\/magician\/v2\/apps\/installations\/[^/]+\/custom-surface-v1\/assets\//.test(request.url ?? '')) return csp;
	if (!/^(?:localhost|127\.0\.0\.1|\[::1\])(?::\d{1,5})?$/i.test(request.host ?? '')) return csp;
	if (!['127.0.0.1', '::1', '::ffff:127.0.0.1'].includes(request.remoteAddress ?? '')) return csp;
	// Preserve every other directive, and never override an explicit 'none'.
	return csp.replace(
		/((?:^|;)\s*)frame-ancestors\s+https?:\/\/[^;\s]+(?=\s*(?:;|$))/,
		"$1frame-ancestors 'self'"
	);
}

function probeMagicianBackend(): Promise<boolean> {
	return new Promise((resolve) => {
		const socket = createConnection({
			host: MAGICIAN_BACKEND_HOST,
			port: MAGICIAN_BACKEND_PORT
		});
		let settled = false;
		const timer = setTimeout(() => finish(false), MAGICIAN_BACKEND_PROBE_TIMEOUT_MS);
		const finish = (healthy: boolean) => {
			if (settled) return;
			settled = true;
			clearTimeout(timer);
			socket.destroy();
			resolve(healthy);
		};

		socket.once('connect', () => finish(true));
		socket.once('error', () => finish(false));
		socket.once('close', () => {
			if (!settled) {
				finish(false);
			}
		});
	});
}

async function isMagicianBackendReady(state: BackendReadinessState): Promise<boolean> {
	const now = Date.now();
	const ttl = state.healthy ? MAGICIAN_BACKEND_READY_TTL_MS : MAGICIAN_BACKEND_DOWN_TTL_MS;
	if (now - state.checkedAt < ttl) return state.healthy;

	if (!state.pending) {
		state.pending = probeMagicianBackend()
			.then((healthy) => {
				state.healthy = healthy;
				state.checkedAt = Date.now();
				return healthy;
			})
			.catch(() => {
				state.healthy = false;
				state.checkedAt = Date.now();
				return false;
			})
			.finally(() => {
				state.pending = null;
			});
	}

	return state.pending;
}

function sendMagicianBackendStarting(req: IncomingMessage, res: ServerResponse) {
	const body = JSON.stringify({
		error: 'magician_backend_starting',
		message: 'Magician backend is still starting; retry shortly.',
		path: req.url || null
	});
	res.statusCode = 503;
	res.setHeader('Content-Type', 'application/json; charset=utf-8');
	res.setHeader('Cache-Control', 'no-store');
	res.setHeader('Retry-After', '1');
	res.setHeader('X-Magician-Backend-Starting', '1');
	res.setHeader('Content-Length', Buffer.byteLength(body));
	if (req.method === 'HEAD') {
		res.end();
		return;
	}
	res.end(body);
}

function magicianBackendReadinessGuard(): Plugin {
	return {
		name: 'magician-backend-readiness-guard',
		apply: 'serve',
		configureServer(server) {
			const state: BackendReadinessState = {
				healthy: false,
				checkedAt: 0,
				pending: null,
				lastLoggedAt: 0
			};

			server.middlewares.use(async (req, res, next) => {
				if (!isMagicianBackendRoute(req.url)) {
					next();
					return;
				}

				let backendReady = false;
				try {
					backendReady = await isMagicianBackendReady(state);
				} catch {
					state.healthy = false;
					state.checkedAt = Date.now();
				}

				if (backendReady) {
					next();
					return;
				}

				const now = Date.now();
				if (now - state.lastLoggedAt > 10_000) {
					state.lastLoggedAt = now;
					server.config.logger.info(
						`[magician] Backend not ready yet; returning 503 for proxied API calls until ${MAGICIAN_BACKEND_HEALTH_URL} responds.`
					);
				}
				sendMagicianBackendStarting(req, res);
			});
		}
	};
}

// Plugin to serve index.html for directory paths in static folder
function staticDirectoryIndex(): Plugin {
	return {
		name: 'static-directory-index',
		configureServer(server) {
			server.middlewares.use((req, res, next) => {
				const url = req.url || '';
				// Check if URL looks like a directory (no file extension)
				if (url && !url.includes('.') && !url.startsWith('/api') && !url.startsWith('/dashboard')) {
					const cleanPath = url.split('?')[0].replace(/\/$/, '');
					const indexPath = join(process.cwd(), 'static', cleanPath, 'index.html');
					if (existsSync(indexPath)) {
						// If no trailing slash, redirect to add it (so relative links work correctly)
						if (!url.endsWith('/')) {
							res.writeHead(302, { Location: cleanPath + '/' });
							res.end();
							return;
						}
						// Serve the index.html
						req.url = cleanPath + '/index.html';
					}
				}
				next();
			});
		}
	};
}

export default defineConfig({
	plugins: [
		sotaAttachmentRoute(),
		magicianBackendReadinessGuard(),
		staticDirectoryIndex(),
		sveltekit()
	],
	build: {
		// DiffStrip intentionally supports Shiki's full bundled language
		// registry through per-language lazy chunks, and browser wake-word
		// support loads Vosk as a separate lazy worker/wasm chunk. Keep
		// the threshold above those known lazy assets while still catching
		// accidental multi-megabyte eager bundles.
		chunkSizeWarningLimit: 6200
	},
	define: {
		// Inject version from package.json at build time
		__APP_VERSION__: JSON.stringify(pkg.version)
	},
	server: {
		host: true,
		port: 5173,
		// Allow any host under the Cloudflare Tunnel zone (e.g.
		// https://ui.example.com/) so dev access through the named tunnel isn't
		// blocked by vite's DNS-rebinding protection. The leading dot makes it
		// a subdomain wildcard. Localhost + LAN IP access is already permitted
		// by default; this only widens the allow-list for the tunnel HTTPS
		// hostnames used during mobile testing.
		allowedHosts: [`.${MAGICIAN_TUNNEL_ZONE}`],
		proxy: {
			// Magician API calls to the Magician backend (port 3002)
			'/api/magician': {
				target: MAGICIAN_BACKEND_ORIGIN,
				// The backend pins custom-page frame-ancestors to the UI origin
				// when public_origin is unset. Preserve the browser's Host here.
				changeOrigin: false,
				secure: false,
				ws: true,
				configure(proxy) {
					proxy.on('proxyRes', (response, request) => {
						const csp = response.headers['content-security-policy'];
						if (response.statusCode !== 200 || typeof csp !== 'string'
							|| !response.headers['content-type']?.startsWith('text/html')) return;
						const localCsp = localScriptedSurfaceCsp(csp, {
							url: request.url,
							host: request.headers.host,
							remoteAddress: request.socket.remoteAddress
						});
						if (localCsp !== csp) {
							response.headers['content-security-policy'] = localCsp;
							response.headers['cache-control'] = 'private, no-store';
						}
					});
				}
			},
			// LLM dispatch queue viewer API (registered backend-side
			// under /api/llm/queue/* by `llm_queue_api::configure`).
			// Without this entry the UI's queue page hits the Vite
			// dev server and 404s instead of reaching the backend.
			'/api/llm': {
				target: MAGICIAN_BACKEND_ORIGIN,
				changeOrigin: true,
				secure: false
			},
			// Observe-only local resource governor. This API predates the
			// /api/magician/v2 namespace, so it needs its own dev proxy entry.
			'/api/local-resource-governor': {
				target: MAGICIAN_BACKEND_ORIGIN,
				changeOrigin: true,
				secure: false
			},
			// Resource Authority API (served under /api/magician/v2/resource-authority,
			// covered by the /api/magician proxy above)
			// Health check
			'/health': {
				target: MAGICIAN_BACKEND_ORIGIN,
				changeOrigin: true,
				secure: false
			}
		}
	}
});
