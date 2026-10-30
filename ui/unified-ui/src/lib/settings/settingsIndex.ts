/**
 * The web Settings page's categories, cards and search keywords.
 *
 * The page renders every card (grouped under these categories) and filters
 * them live: a card stays visible when every word of the query appears in its
 * title, its keywords or its rendered text. Keywords cover words people search
 * for that a card's text may not contain ("budget" for Spending).
 */

export interface SettingsCard {
	/** `data-settings-card` value on the card's wrapper. */
	id: string;
	title: string;
	keywords: string[];
	/** Ids inside the card that other pages link to (`/settings#workspace`). */
	anchors?: string[];
}

export interface SettingsCategory {
	/** Section id and URL hash (`/settings#voice`). */
	id: string;
	label: string;
	cards: SettingsCard[];
}

/** A category as the Settings nav shows it. */
export interface SettingsNavCategory {
	id: string;
	label: string;
	/** Cards shown in the category (matching ones while searching). */
	count: number;
}

export const SETTINGS_CATEGORIES: SettingsCategory[] = [
	{
		id: 'account',
		label: 'Account & workspaces',
		cards: [
			{
				id: 'sign-in',
				title: 'Sign-in and workspace',
				keywords: ['account', 'login', 'log in', 'sign out', 'bearer', 'token', 'username', 'password'],
				anchors: ['workspace']
			},
			{
				id: 'workspaces',
				title: 'Workspaces',
				keywords: ['workspace', 'switch workspace', 'create workspace', 'members']
			},
			{
				id: 'service-accounts',
				title: 'Service accounts',
				keywords: ['oauth', 'sign in', 'connect', 'swiggy', 'zepto', 'metabase', 'higgsfield', 'login']
			}
		]
	},
	{
		id: 'agents',
		label: 'Agents & safety',
		cards: [
			{
				id: 'god-mode',
				title: 'Agent powers',
				keywords: ['god mode', 'auto-approve', 'auto approve', 'super god mode', 'permissions', 'approval', 'python', 'sandbox', 'powers']
			},
			{
				id: 'trust-policy',
				title: 'Rules (trust policy)',
				keywords: ['trust', 'policy', 'rules', 'yaml', 'allow', 'deny', 'safety']
			},
			{
				id: 'preferences',
				title: 'Your preferences',
				keywords: ['preferences', 'rules', 'goals', 'tastes', 'alignment', 'checks', 'before it sends', 'buys']
			},
			{
				id: 'preference-checks',
				title: 'Preference checks',
				keywords: ['preference checks', 'check policy', 'how firmly', 'alignment', 'enforce', 'notes from recent checks', 'before it books', 'orders']
			},
			{
				id: 'outward-actions',
				title: 'Outward actions',
				keywords: ['outward', 'sending', 'capture', 'email', 'whatsapp', 'imessage', 'opc', 'envoy', 'autonomous', 'form submissions']
			},
			{
				id: 'budgets',
				title: 'Spending',
				keywords: ['budget', 'budgets', 'spend', 'spending', 'cost', 'money', 'dollars', 'limit', 'per run', 'per day', 'usd', 'model spend', 'action limits', 'emails per day', 'rupees', 'ledger', 'resource authority'],
				anchors: ['budgets', 'model-spend', 'action-limits']
			},
			{
				id: 'harness-auth',
				title: 'Harness credentials',
				keywords: ['coding cli', 'claude', 'codex', 'gemini', 'api key', 'subscription', 'harness', 'account']
			},
			{
				id: 'terminal-grants',
				title: 'Terminal grants',
				keywords: ['terminal', 'shell', 'grant', 'plane', 'command']
			}
		]
	},
	{
		id: 'models',
		label: 'Models & engines',
		cards: [
			{
				id: 'engines',
				title: 'Engines',
				keywords: ['engine', 'model', 'provider', 'who thinks', 'llm', 'openai', 'anthropic', 'agentic flows', 'background runs', 'thinking']
			},
			{
				id: 'local-generation',
				title: 'Local models',
				keywords: ['local', 'on-device', 'on device', 'ollama', 'offline', 'generation', 'model', 'cloud', 'processing', 'privacy', 'locality']
			},
			{
				id: 'backend-config',
				title: 'Backend config and model routing',
				keywords: ['config', 'reload', 'model routing', 'routes', 'profiles', 'operation mappings', 'restart']
			}
		]
	},
	{
		id: 'voice',
		label: 'Voice & calls',
		cards: [
			{
				id: 'audio-surfaces',
				title: 'Voice and audio profiles',
				keywords: ['audio', 'voice', 'microphone', 'speaker', 'surface', 'profile']
			},
			{
				id: 'voice-preferences',
				title: 'Voice preferences',
				keywords: ['voice', 'speech', 'speak', 'language', 'preferences']
			},
			{
				id: 'tts-voices',
				title: 'Text-to-speech voices',
				keywords: ['tts', 'text to speech', 'voice', 'speaking voice', 'persona']
			},
			{
				id: 'live-call-voices',
				title: 'Live call voices',
				keywords: ['live', 'realtime', 'call', 'voice', 'engine']
			},
			{
				id: 'whatsapp-calls',
				title: 'WhatsApp calls',
				keywords: ['whatsapp', 'call', 'phone', 'ring']
			},
			{
				id: 'meet-calls',
				title: 'Google Meet calls',
				keywords: ['meet', 'google meet', 'meeting', 'call', 'video']
			}
		]
	},
	{
		id: 'notifications',
		label: 'Notifications',
		cards: [
			{
				id: 'browser-notifications',
				title: 'Browser notifications',
				keywords: ['notification', 'push', 'alert', 'questions', 'browser'],
				anchors: ['browser-notifications-panel']
			},
			{
				id: 'owner-identities',
				title: 'Owner identities',
				keywords: ['owner', 'identity', 'claim', 'whatsapp', 'telegram', 'phone number', 'chat id', 'kapso', 'envoy', 'guest']
			},
			{
				id: 'critical-delivery',
				title: 'Critical alerts',
				keywords: ['critical', 'alert', 'credential request', 'delivery', 'phone', 'urgent']
			}
		]
	},
	{
		id: 'devices',
		label: 'Devices & connection',
		cards: [
			{
				id: 'machines',
				title: 'Self-hosted connection',
				keywords: ['machine', 'self-hosted', 'connection', 'server', 'remote', 'host']
			},
			{
				id: 'device-pairing',
				title: 'Connected devices',
				keywords: ['device', 'pair', 'pairing', 'phone', 'ios', 'android', 'qr', 'connected']
			}
		]
	},
	{
		id: 'data',
		label: 'Data & storage',
		cards: [
			{
				id: 'workspace-storage',
				title: 'Workspace storage',
				keywords: ['storage', 'disk', 'runtime', 'data folder', 'workspace']
			},
			{
				id: 'storage-governance',
				title: 'Storage governance',
				keywords: ['storage', 'retention', 'compaction', 'cleanup', 'maintenance', 'local data']
			},
			{
				id: 'notes-folder',
				title: 'Notes folder',
				keywords: ['notes', 'folder', 'silverbullet', 'markdown', 'publish', 'captures']
			},
			{
				id: 'api-mining',
				title: 'API mining',
				keywords: ['api mining', 'privacy', 'learn', 'capture', 'replay', 'automation']
			}
		]
	},
	{
		id: 'about',
		label: 'About',
		cards: [
			{
				id: 'versions',
				title: 'Versions',
				keywords: ['version', 'about', 'build', 'release', 'web app', 'backend', 'api', 'magicutor', 'desktop', 'update']
			}
		]
	}
];

export function normalizeSettingsQuery(query: string): string {
	return query.trim().toLowerCase().replace(/\s+/g, ' ');
}

/**
 * True when every word of the (normalized) query appears in the card's title,
 * keywords or rendered text. An empty query matches every card.
 */
export function cardMatches(card: SettingsCard, query: string, renderedText = ''): boolean {
	const normalized = normalizeSettingsQuery(query);
	if (!normalized) return true;
	const haystack = [card.title, ...card.keywords, renderedText].join(' ').toLowerCase();
	return normalized.split(' ').every((word) => haystack.includes(word));
}

/** The category a URL hash points at: a category id, card id or card anchor. */
export function categoryForHash(hash: string): string | null {
	const id = decodeURIComponent(hash.replace(/^#/, '')).trim();
	if (!id) return null;
	for (const category of SETTINGS_CATEGORIES) {
		if (category.id === id) return category.id;
		if (category.cards.some((card) => card.id === id || card.anchors?.includes(id))) {
			return category.id;
		}
	}
	return null;
}
