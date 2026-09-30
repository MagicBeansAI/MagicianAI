/**
 * Provider factory — picks the right `RealtimeFrontendProvider` impl
 * for a given session descriptor. The single branching point that
 * the orchestrator-shaped client `realtimeVoiceClient.ts` uses.
 *
 * Adding a new provider (Gemini Live, ElevenLabs Realtime, etc.) is:
 *   1. One new file under this directory implementing
 *      `RealtimeFrontendProvider` for the right topology.
 *   2. One match arm here against `descriptor.provider`.
 *
 * Nothing else in the client changes.
 */

import { BackendProxiedFrontendProvider } from './backend_proxied';
import { OpenAiRealtimeFrontendProvider } from './openai';
import type { ProviderDescriptor, RealtimeFrontendProvider } from './types';

export function pickRealtimeProvider(
	descriptor: ProviderDescriptor
): RealtimeFrontendProvider {
	// Topology comes first — `BackendProxied` providers all share the
	// same PCM-over-WS plumbing; only the upstream wire format differs
	// (and that lives backend-side). `DirectPeerToPeer` providers
	// each get their own impl because the data-channel + SDP shape
	// is vendor-specific.
	if (descriptor.topology === 'backend_proxied') {
		return new BackendProxiedFrontendProvider();
	}
	switch (descriptor.provider) {
		case 'openai':
		case 'open_ai':
			return new OpenAiRealtimeFrontendProvider();
		default:
			throw new Error(
				`Unsupported realtime voice provider: "${descriptor.provider}". ` +
					'Add an impl under lib/media/voice/providers/ and a match arm in pickRealtimeProvider.'
			);
	}
}

export type {
	ProviderDescriptor,
	ProviderResumeContext,
	ProviderResumeTurn,
	ProviderSessionConfig,
	RealtimeFrontendProvider,
	RealtimeProviderCallbacks,
	VoiceToolDefinition
} from './types';

export { OpenAiRealtimeFrontendProvider } from './openai';
