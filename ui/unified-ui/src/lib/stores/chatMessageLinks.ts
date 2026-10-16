/** Durable destinations; old projections resolve by exact turn + saved timestamp. */
export interface ChatMessageOrigin {
    ui_thread_id: string;
    session_id: string;
    request_id: string;
    message_id?: string;
    result_created_at?: number;
}
export interface ChatMessageTarget {
    messageId?: string;
    turnId?: string;
    createdAt?: number;
}
export function matchesMessageTarget(
    message: { id: string; chat_turn_id?: string; created_at: number; direction: string },
    target: ChatMessageTarget
): boolean {
    if (target.messageId) return message.id === target.messageId;
    return !!target.turnId && target.createdAt !== undefined &&
        message.chat_turn_id === target.turnId && message.created_at === target.createdAt &&
        message.direction !== 'user';
}
export function originalAnswerHref(message: {
    context_origin?: ChatMessageOrigin; session_id: string; direction: string;
    chat_turn_id?: string; created_at: number;
}): string | null {
    const origin = message.context_origin;
    if (!origin || message.direction === 'user' || origin.session_id === message.session_id) return null;
    const query = new URLSearchParams({ session: origin.session_id });
    if (origin.message_id) query.set('message', origin.message_id);
    else if (message.chat_turn_id && Number.isFinite(message.created_at)) {
        query.set('source_turn', message.chat_turn_id);
        query.set('source_at', String(message.created_at));
    } else return null;
    return `/t/${encodeURIComponent(origin.ui_thread_id)}/chat?${query}`;
}
export function messageTargetFromQuery(query: URLSearchParams): ChatMessageTarget | undefined {
    const messageId = query.get('message')?.trim();
    if (messageId) return { messageId };
    const turnId = query.get('source_turn')?.trim();
    const at = query.get('source_at');
    if (turnId && at && Number.isFinite(Number(at))) return { turnId, createdAt: Number(at) };
    return undefined;
}
