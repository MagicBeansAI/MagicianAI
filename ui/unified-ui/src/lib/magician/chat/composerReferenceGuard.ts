export interface ComposerReferenceRequest {
	generation: number;
	sessionId: string | null;
	scopeKey: string;
}

/**
 * A session/scope tuple can repeat after A -> B -> A navigation, so identity
 * alone cannot reject the first A response. The monotonic generation makes
 * every refresh unique while the tuple still protects against missed scope or
 * session invalidation.
 */
export function isComposerReferenceRequestCurrent(
	request: ComposerReferenceRequest,
	currentGeneration: number,
	currentSessionId: string | null,
	currentScopeKey: string
): boolean {
	return request.generation === currentGeneration
		&& request.sessionId === currentSessionId
		&& request.scopeKey === currentScopeKey;
}
