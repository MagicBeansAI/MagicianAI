/**
 * Magician Citizen — the M6 "Pi as OS citizen" extension.
 *
 * Loaded by Magician at spawn (`pi --mode rpc --extension <this file>`), it
 * registers Magician-native capabilities as model-callable tools. Each tool's
 * async handler calls back into the Magician host over loopback HTTP (the
 * "Citizen API"), authenticated by a per-run scope token, so a tool runs in the
 * exact scope (principal/workspace/project) of the run that spawned Pi.
 *
 * This is the moat: anyone can `npx pi`, but only Magician hands it the live
 * preview, the LSP, the Work-Evidence Graph, and a scoped secret broker as
 * first-class tools wired into the user's own running system.
 *
 * Tools shipped so far: `magician_preview_url` (Phase 0, R3 dev-server),
 * `magician_secret` (Phase 1, scoped secret broker), and
 * `magician_code_knowledge` (M3 P3, hybrid recall of the run agent's distilled
 * code facts). `magician_lsp` is the remaining planned tool (needs an LSP
 * service). See docs/plans/2026-06-14-m6-magician-citizen.md and
 * docs/plans/2026-06-15-m3-code-knowledge.md.
 *
 * Config (injected as env by `run_coding_task` at spawn):
 *   MAGICIAN_CITIZEN_URL    loopback base url of the Citizen API (e.g. http://127.0.0.1:3002/api/magician/v2/vibedev)
 *   MAGICIAN_CITIZEN_TOKEN  per-run bearer token (scope-qualified, turn lifetime)
 *
 * Intentionally dependency-free (no typebox / SDK runtime imports) so it loads
 * from an arbitrary path regardless of the host's module resolution.
 */

function citizenConfig() {
	return {
		baseUrl: process.env.MAGICIAN_CITIZEN_URL,
		token: process.env.MAGICIAN_CITIZEN_TOKEN,
	};
}

/** POST a capability call to the Citizen API; return the text body or throw. */
async function callCitizen(capability, params, signal) {
	const { baseUrl, token } = citizenConfig();
	if (!baseUrl || !token) {
		throw new Error(
			"Magician Citizen API is not configured (missing MAGICIAN_CITIZEN_URL / MAGICIAN_CITIZEN_TOKEN).",
		);
	}
	const res = await fetch(`${baseUrl}/citizen/${capability}`, {
		method: "POST",
		headers: {
			"content-type": "application/json",
			authorization: `Bearer ${token}`,
		},
		body: JSON.stringify(params || {}),
		signal,
	});
	const body = await res.text();
	if (!res.ok) {
		throw new Error(`Citizen ${capability} failed (HTTP ${res.status}): ${body}`);
	}
	return body;
}

export default function magicianCitizen(pi) {
	const allowedRaw = process.env.MAGICIAN_CITIZEN_TOOLS;
	const allowedTools =
		allowedRaw && allowedRaw.trim()
			? new Set(allowedRaw.split(",").map((s) => s.trim()).filter(Boolean))
			: null;
	// null == env absent == unscoped (legacy / build run) == allow all; a populated set permits only
	// those tool names. The server-side authorize_citizen check in vibedev_api is the authoritative gate.
	const allow = (name) => allowedTools === null || allowedTools.has(name);
	// Register a tool only when its name is in the per-run allowlist. The server-side
	// authorize_citizen check is the authoritative gate; this just trims Pi's tool surface.
	const register = (def) => {
		if (allow(def.name)) pi.registerTool(def);
	};
	register({
		name: "magician_preview_url",
		label: "Magician Preview URL",
		description:
			"Get the live local preview URL of this VibeDev project's running dev server (served by the Magician host). Use it before screenshotting or describing the running app. Returns {ok, preview_url, project_id} or {ok:false, reason} when no preview is running.",
		parameters: {
			type: "object",
			properties: {
				project_id: {
					type: "string",
					description:
						"VibeDev project id. Omit to use the active project of the current run.",
				},
			},
		},
		async execute(_toolCallId, params, signal) {
			try {
				const body = await callCitizen("preview_url", params, signal);
				return {
					content: [{ type: "text", text: body }],
					details: { source: "magician_citizen", capability: "preview_url" },
				};
			} catch (err) {
				return {
					content: [{ type: "text", text: String(err && err.message ? err.message : err) }],
					isError: true,
				};
			}
		},
	});

	register({
		name: "magician_code_knowledge",
		label: "Magician Code Knowledge",
		description:
			"Recall durable knowledge distilled from this project's prior engineering work — where things live, the conventions/patterns used, build/test commands, design decisions, and 'what we tried and why it broke'. Use it BEFORE writing or changing code to ground yourself in what already exists instead of guessing or re-reading the whole tree. Results are scoped to THIS project (facts the system tagged to another project are excluded; each fact carries its `project_id`). Pass a natural-language `query`; optionally cap results with `k` (default 6). Returns ranked facts {key, text, project_id, source, score}; empty if nothing relevant has been learned yet.",
		parameters: {
			type: "object",
			properties: {
				query: {
					type: "string",
					description: "Natural-language query over this project's distilled code knowledge.",
				},
				k: {
					type: "number",
					description: "Max number of facts to return (default 6).",
				},
			},
			required: ["query"],
		},
		async execute(_toolCallId, params, signal) {
			try {
				const body = await callCitizen("code_knowledge", params, signal);
				return {
					content: [{ type: "text", text: body }],
					details: { source: "magician_citizen", capability: "code_knowledge" },
				};
			} catch (err) {
				return {
					content: [{ type: "text", text: String(err && err.message ? err.message : err) }],
					isError: true,
				};
			}
		},
	});

	register({
		name: "magician_secret",
		label: "Magician Secret",
		description:
			"Make a scoped secret (e.g. DATABASE_URL, a 3rd-party API key) available to THIS project's dev server. Magician resolves it and injects the value into the running app's environment — you NEVER see the value. Read it in code via the env var (e.g. process.env.<env_var>). Use this instead of hardcoding secrets or asking the user to paste them. Returns {ok, env_var, requires_restart}.",
		parameters: {
			type: "object",
			properties: {
				secret_id: {
					type: "string",
					description: "The provisioned secret's id/name in this workspace's vault.",
				},
				env_var: {
					type: "string",
					description: "Env var to expose it under in the dev server (defaults to secret_id).",
				},
				project_id: {
					type: "string",
					description: "VibeDev project id; omit to use the current run's project.",
				},
			},
			required: ["secret_id"],
		},
		async execute(_toolCallId, params, signal) {
			try {
				const body = await callCitizen("secret", params, signal);
				return {
					content: [{ type: "text", text: body }],
					details: { source: "magician_citizen", capability: "secret" },
				};
			} catch (err) {
				return {
					content: [{ type: "text", text: String(err && err.message ? err.message : err) }],
					isError: true,
				};
			}
		},
	});
}
