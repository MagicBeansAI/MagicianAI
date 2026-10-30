import { scopedRequestHeaders } from '$lib/stores/scopeIdentityStore';

/** A getter keeps each mounted chat's media actions independent of other chats. */
export const MEDIA_CHAT_SESSION = Symbol('media-chat-session');
export type MediaChatSession = () => string | null;
export function mediaChatHeaders(chatSessionId?: string | null, extra: Record<string, string> = {}): Headers {
    return scopedRequestHeaders({ ...extra, ...(chatSessionId ? { 'X-Chat-Session-Id': chatSessionId } : {}) });
}
