/**
 * Shared filesystem-path affordance builder.
 *
 * Every place in the UI that wants to expose a filesystem path with the
 * standard "Open in default app" + "Reveal in Finder" actions calls
 * `buildPathLink` (clickable text + icons) or `attachPathActions`
 * (icons-only, appended after an existing element). This keeps the
 * visual language and click semantics identical across:
 *
 *   - bare-prose absolute paths inside chat assistant text
 *   - paths wrapped in inline ``code`` blocks
 *   - `[label](file:///abs/path)` markdown links
 *   - any future surface that surfaces a path
 *
 * The OS actions themselves are caller-supplied (`PathActionHandlers`)
 * because each surface has its own scope/session context that the
 * backend uses to authorize the open-file/open-folder POST.
 */

const SVG_NS = 'http://www.w3.org/2000/svg';

export type PathSize = 'sm' | 'md' | 'lg';

const SIZE_PX: Record<PathSize, number> = {
    sm: 11,
    md: 13,
    lg: 16,
};

export interface PathActionHandlers {
    /** Called when the user clicks the open-file affordance. */
    onOpenFile: (absolutePath: string) => void | Promise<void>;
    /** Called when the user clicks the reveal-folder affordance. */
    onRevealFolder: (absolutePath: string) => void | Promise<void>;
}

function buildSvgIcon(
    sizePx: number,
    paths: Array<{ tag: 'path' | 'polyline'; d?: string; points?: string }>,
): SVGSVGElement {
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('xmlns', SVG_NS);
    svg.setAttribute('width', String(sizePx));
    svg.setAttribute('height', String(sizePx));
    svg.setAttribute('viewBox', '0 0 24 24');
    svg.setAttribute('fill', 'none');
    svg.setAttribute('stroke', 'currentColor');
    svg.setAttribute('stroke-width', '2.2');
    svg.setAttribute('stroke-linecap', 'round');
    svg.setAttribute('stroke-linejoin', 'round');
    svg.setAttribute('aria-hidden', 'true');
    for (const spec of paths) {
        const child = document.createElementNS(SVG_NS, spec.tag);
        if (spec.d) child.setAttribute('d', spec.d);
        if (spec.points) child.setAttribute('points', spec.points);
        svg.appendChild(child);
    }
    return svg;
}

function buildFolderIcon(sizePx: number): SVGSVGElement {
    return buildSvgIcon(sizePx, [
        {
            tag: 'path',
            d: 'M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z',
        },
    ]);
}

function buildFileIcon(sizePx: number): SVGSVGElement {
    return buildSvgIcon(sizePx, [
        { tag: 'path', d: 'M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z' },
        { tag: 'polyline', points: '14 3 14 8 19 8' },
    ]);
}

function buildIconButton(
    iconSvg: SVGSVGElement,
    cls: string,
    titleText: string,
    ariaLabel: string,
    onClick: (event: MouseEvent) => void,
): HTMLButtonElement {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = `path-action ${cls}`;
    btn.title = titleText;
    btn.setAttribute('aria-label', ariaLabel);
    btn.appendChild(iconSvg);
    btn.addEventListener('click', (event) => {
        event.preventDefault();
        event.stopPropagation();
        onClick(event);
    });
    return btn;
}

/**
 * Builds the icon pair (reveal-folder + open-file) for an absolute path.
 * Returns a `<span>` that callers can drop next to existing text/links.
 */
export function buildPathActionsBlock(
    absolutePath: string,
    size: PathSize,
    handlers: PathActionHandlers,
): HTMLSpanElement {
    const wrap = document.createElement('span');
    wrap.className = `path-actions path-actions--${size}`;
    wrap.setAttribute('data-path-actions', '1');

    const folderBtn = buildIconButton(
        buildFolderIcon(SIZE_PX[size]),
        'path-action--folder',
        `Reveal the folder containing ${absolutePath} in your OS file manager`,
        `Reveal folder containing ${absolutePath}`,
        () => void handlers.onRevealFolder(absolutePath),
    );
    const fileBtn = buildIconButton(
        buildFileIcon(SIZE_PX[size]),
        'path-action--file',
        `Open ${absolutePath} in its default application`,
        `Open ${absolutePath}`,
        () => void handlers.onOpenFile(absolutePath),
    );
    wrap.appendChild(folderBtn);
    wrap.appendChild(fileBtn);
    return wrap;
}

/**
 * Appends the action icons immediately after the given element. Used
 * when the caller already rendered the path text (e.g. inside `<code>`
 * or `<a>`) and just needs the affordance icons next to it.
 *
 * Idempotent — checks for an existing `[data-path-actions]` sibling
 * adjacent to the target before adding a fresh one, so a re-run of the
 * caller's MutationObserver doesn't double-wrap.
 */
export function attachPathActions(
    after: HTMLElement,
    absolutePath: string,
    size: PathSize,
    handlers: PathActionHandlers,
): HTMLSpanElement | null {
    const existing = after.nextElementSibling;
    if (existing instanceof HTMLElement && existing.dataset.pathActions === '1') {
        return null;
    }
    const block = buildPathActionsBlock(absolutePath, size, handlers);
    after.insertAdjacentElement('afterend', block);
    return block;
}

/**
 * Builds a complete clickable path reference: the path text (or a
 * custom display label) as a click target for open-file, followed by
 * the reveal-folder + open-file icon pair. Used for bare-prose paths
 * that aren't already wrapped in `<code>` or `<a>`.
 */
export function buildPathLink(
    absolutePath: string,
    displayText: string,
    size: PathSize,
    handlers: PathActionHandlers,
): HTMLSpanElement {
    const wrap = document.createElement('span');
    wrap.className = `path-ref path-ref--${size}`;

    const textBtn = document.createElement('button');
    textBtn.type = 'button';
    textBtn.className = 'path-ref__text';
    textBtn.textContent = displayText;
    textBtn.title = `Open ${absolutePath} in its default application`;
    textBtn.setAttribute('aria-label', `Open ${absolutePath}`);
    textBtn.addEventListener('click', (event) => {
        event.preventDefault();
        event.stopPropagation();
        void handlers.onOpenFile(absolutePath);
    });
    wrap.appendChild(textBtn);
    wrap.appendChild(buildPathActionsBlock(absolutePath, size, handlers));
    return wrap;
}
