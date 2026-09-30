/**
 * Classify a link-shaped string into a typed internal resource reference.
 *
 * The chat surface routinely receives free-form prose from the LLM that
 * mentions internal resources (task IDs, briefing slugs, output paths,
 * execution IDs, …). Markdown renders those as `<a>` tags whose hrefs
 * are filesystem paths or bare IDs — the browser can't navigate to
 * them, so they either fail silently or pop "outside workspace" errors.
 *
 * `classifyRef` is the single source of truth for "what URL/path means
 * what". `resolveHref` maps a classified ref to an in-app URL that
 * SvelteKit's `goto()` can navigate. Together they let the chat
 * markdown post-processor (and any other surface) rewrite raw LLM
 * mentions into actionable links.
 *
 * Add new resource kinds here when the runtime starts emitting them
 * in chat text; every consumer (link rewriter, action menus, code
 * generators, …) picks up the new behavior automatically.
 */

/* eslint-disable @typescript-eslint/no-explicit-any */

export type ResourceRef =
    /** A magician task. Optionally scoped by ui_thread for thread-task view. */
    | { kind: 'task'; id: string; threadId?: string }
    /** A magician execution (one run of a task). */
    | { kind: 'execution'; id: string }
    /** A published briefing surface (route slug or id). */
    | { kind: 'briefing'; slug: string | null }
    /** A file artifact owned by a task — served via the artifact API. */
    | { kind: 'taskOutput'; taskId: string; relativePath: string; absolutePath?: string }
    /** A dashboard surface route (`/dashboard/...`). */
    | { kind: 'dashboard'; route: string }
    /** A skill detail page. */
    | { kind: 'skill'; name: string }
    /** An agent detail page. */
    | { kind: 'agent'; id: string }
    /** A SvelteKit in-app route (already starts with `/`). */
    | { kind: 'internalRoute'; route: string }
    /** An absolute filesystem path — opened via the scope-validated
     *  chat-session OS-action endpoints. Covers chat-session outputs,
     *  /tmp pack outputs, anything the agent dropped on disk. The
     *  backend re-validates against the scope root, so an unrelated
     *  /etc/passwd href is rejected server-side. */
    | { kind: 'filePath'; absolutePath: string }
    /** A normal external URL (https/mailto/…). */
    | { kind: 'external'; url: string }
    /** Could not classify; fall back to plain text. */
    | { kind: 'unknown'; raw: string };

const TASK_ID_RE = /^task_[0-9a-f]{32}$/i;
const EXECUTION_ID_RE = /^exec_[0-9a-f]{32}$/i;

/**
 * Classify the given href / path / bare id.
 *
 * `href` should be the literal string from `<a href="...">` or a bare
 * token (e.g. a task_id mentioned in prose). Leading/trailing
 * whitespace is trimmed; case is preserved.
 */
export function classifyRef(href: string): ResourceRef {
    let raw = href?.trim() ?? '';
    if (!raw) return { kind: 'unknown', raw: '' };

    // External URL — keep as-is.
    if (/^(https?:|mailto:|tel:|ftp:)/i.test(raw)) {
        return { kind: 'external', url: raw };
    }

    // Strip `file://` prefix and re-classify the underlying path. The
    // browser can't navigate `file://` (sandbox), but the underlying
    // absolute path is fine for the OS-action endpoints — the backend
    // scope-validates it before opening. Handle both `file:///abs` and
    // `file://abs` shapes the LLM occasionally emits.
    if (/^file:/i.test(raw)) {
        raw = raw.replace(/^file:\/{0,3}/i, '/');
        if (!raw.startsWith('/')) {
            // Pathological — couldn't normalize to an absolute path.
            return { kind: 'unknown', raw: href };
        }
    }

    // Bare task / execution IDs (LLM mentioned them in prose).
    if (TASK_ID_RE.test(raw)) {
        return { kind: 'task', id: raw };
    }
    if (EXECUTION_ID_RE.test(raw)) {
        return { kind: 'execution', id: raw };
    }

    // Filesystem path containing a task_id + outputs/ — pull both out.
    // Matches:  magician_data_v3/.../tasks/task_xxx/.../outputs/foo.pdf
    //           any prefix, any suffix after /outputs/
    const taskOutputMatch = raw.match(
        /(?:^|\/)tasks\/(task_[0-9a-f]{32})\/(?:.*?\/)?outputs\/(.+)$/i,
    );
    if (taskOutputMatch) {
        const [, taskId, relativePath] = taskOutputMatch;
        return {
            kind: 'taskOutput',
            taskId,
            relativePath,
            absolutePath: raw.startsWith('/') ? raw : undefined,
        };
    }

    // /briefing or /briefing/<slug> (singular — matches the SvelteKit route).
    // Also accept /briefings (plural) since LLMs use both spellings.
    const briefingMatch = raw.match(/^\/briefings?(?:\/([^?#]+))?(?:[?#].*)?$/);
    if (briefingMatch) {
        return { kind: 'briefing', slug: briefingMatch[1] ?? null };
    }

    // /dashboard or /dashboard/...
    if (/^\/dashboard(?:\/|$)/.test(raw)) {
        return { kind: 'dashboard', route: raw };
    }

    // /skills/<name>
    const skillMatch = raw.match(/^\/skills\/([^/?#]+)/);
    if (skillMatch) {
        return { kind: 'skill', name: skillMatch[1] };
    }

    // /crew/<id> or /agents/<id>
    const agentMatch = raw.match(/^\/(?:crew|agents)\/([^/?#]+)/);
    if (agentMatch) {
        return { kind: 'agent', id: agentMatch[1] };
    }

    // Absolute filesystem path — anything that looks like a real OS
    // path (has a file extension on the last segment, or sits under a
    // known data-tree prefix). Routes to the OS-action endpoints so
    // the user can reveal/open it. Checked BEFORE the generic
    // `internalRoute` branch so paths to actual files don't get
    // misrouted as SvelteKit routes (`goto('/Users/...')` 404s).
    if (raw.startsWith('/') && !raw.startsWith('//') && !raw.includes('://')) {
        const looksLikeFilePath =
            /\/Users\//.test(raw) ||
            /\/tmp\//.test(raw) ||
            /\/private\/tmp\//.test(raw) ||
            /\/magician_data_v3\//.test(raw) ||
            /\.[A-Za-z0-9]{1,8}(?:$|[?#])/.test(raw);
        if (looksLikeFilePath) {
            return { kind: 'filePath', absolutePath: raw };
        }
    }

    // Generic in-app route (anything else starting with `/`).
    // Reject paths that contain `//` or `://` anywhere — both are
    // protocol-relative / absolute-URL escape vectors that would
    // otherwise reach `goto()` as a navigation to an unrelated origin.
    if (
        raw.startsWith('/') &&
        !raw.startsWith('//') &&
        !raw.includes('://') &&
        !raw.slice(1).includes('//')
    ) {
        return { kind: 'internalRoute', route: raw };
    }

    return { kind: 'unknown', raw };
}

/**
 * Resolve a classified ref into a target URL for SvelteKit `goto()`.
 *
 * Returns `null` for unknown / unrenderable refs — caller should
 * leave the original text as plain prose in that case.
 */
export function resolveHref(ref: ResourceRef): string | null {
    switch (ref.kind) {
        case 'external':
            return ref.url;
        case 'task':
            return ref.threadId
                ? `/t/${encodeURIComponent(ref.threadId)}?selected=${encodeURIComponent(ref.id)}`
                : `/tasks?filter=all&selected=${encodeURIComponent(ref.id)}`;
        case 'execution':
            // No dedicated /executions route yet; route to /debug with the
            // exec id which has an existing inspection surface.
            return `/debug?execution_id=${encodeURIComponent(ref.id)}`;
        case 'briefing':
            return ref.slug
                ? `/briefing/${encodeURIComponent(ref.slug)}`
                : `/briefing`;
        case 'taskOutput':
            // Artifact API serves the bytes; the chat page's existing
            // open-file handler is the better target for "open in OS",
            // but for in-app preview the artifact URL is right.
            return `/api/magician/v3/tasks/${encodeURIComponent(ref.taskId)}/outputs/${ref.relativePath
                .split('/')
                .map(encodeURIComponent)
                .join('/')}`;
        case 'dashboard':
            return ref.route;
        case 'skill':
            return `/skills?name=${encodeURIComponent(ref.name)}`;
        case 'agent':
            return `/crew/${encodeURIComponent(ref.id)}`;
        case 'internalRoute':
            return ref.route;
        case 'filePath':
            // `filePath` refs are not navigated via `goto()` or
            // `window.open` — the click handler in `ChatMarkdown` calls
            // the chat-session OS-action endpoint with `absolute_path`.
            // Return a placeholder so the anchor stays clickable; the
            // intercepting handler decides what actually happens.
            return ref.absolutePath;
        case 'unknown':
            return null;
    }
}

/**
 * Human-readable label for a classified ref — useful when the
 * rewriter inserts a clickable span around a bare ID in prose.
 */
export function refLabel(ref: ResourceRef): string {
    switch (ref.kind) {
        case 'task':
            return `Task ${ref.id.slice(5, 13)}…`;
        case 'execution':
            return `Run ${ref.id.slice(5, 13)}…`;
        case 'briefing':
            return ref.slug ? `Briefing: ${ref.slug}` : 'Briefing';
        case 'taskOutput':
            return ref.relativePath.split('/').pop() ?? ref.relativePath;
        case 'dashboard':
            return `Dashboard ${ref.route}`;
        case 'skill':
            return `Skill: ${ref.name}`;
        case 'agent':
            return `Agent: ${ref.id}`;
        case 'internalRoute':
            return ref.route;
        case 'filePath':
            return ref.absolutePath.split('/').pop() ?? ref.absolutePath;
        case 'external':
            return ref.url;
        case 'unknown':
            return ref.raw;
    }
}

/**
 * Hints whether the ref should be opened via SvelteKit `goto()`
 * (client-side nav) or via `window.open` (artifact downloads,
 * external URLs).
 */
export function refOpenMode(ref: ResourceRef): 'goto' | 'newTab' | 'download' | 'osAction' {
    switch (ref.kind) {
        case 'external':
            return 'newTab';
        case 'taskOutput':
            return 'newTab'; // image/PDF previews open cleanly in a new tab
        case 'filePath':
            // Intercept-only: the click handler POSTs to the chat
            // session's `outputs/open-file` endpoint with `absolute_path`.
            // No actual href navigation.
            return 'osAction';
        default:
            return 'goto';
    }
}
