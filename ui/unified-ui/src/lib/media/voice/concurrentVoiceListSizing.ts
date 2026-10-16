/**
 * How tall the expanded background-request list is: its content, up to four
 * rows plus a peek of the fifth (so it reads as scrollable). Rows are
 * measured, so one- and two-line rows each count as one row. The same rule
 * sizes the iOS and Android lists; there, the room above the list caps it at
 * 40%, and here the stylesheet's `dvh` cap does that job.
 */
export const VISIBLE_REQUEST_ROWS = 4;
export const REQUEST_ROW_PEEK_PX = 18;
export const AVAILABLE_FRACTION = 0.4;

/**
 * @param contentHeight the list's full content height, padding included.
 * @param rowBottoms each row's bottom edge from the content top, in order.
 * @param available the room above the list's bottom edge, when known.
 * @param bottomPadding the content's padding below the last row.
 */
export function requestListHeight(contentHeight: number, rowBottoms: readonly number[], available: number | null = null, bottomPadding = 0): number {
  if (contentHeight <= 0) return 0;
  let cap = contentHeight;
  if (rowBottoms.length > VISIBLE_REQUEST_ROWS) cap = rowBottoms[VISIBLE_REQUEST_ROWS - 1] + REQUEST_ROW_PEEK_PX;
  if (available != null && available > 0) {
    const floor = rowBottoms.length ? rowBottoms[0] + bottomPadding : 0;
    cap = Math.min(cap, Math.max(available * AVAILABLE_FRACTION, floor));
  }
  return Math.min(contentHeight, cap);
}

/**
 * Svelte action for the request list: publishes the four-row cap as
 * `--request-list-cap` and marks `more-below` while rows sit under the fold.
 * Rows are the list's `.request` children; a row growing (an opened result)
 * or the list changing re-measures.
 */
export function requestListSizing(node: HTMLElement) {
  const rows = () => Array.from(node.children).filter((el): el is HTMLElement => el instanceof HTMLElement && el.classList.contains('request'));
  const paddingBottom = () => parseFloat(getComputedStyle(node).paddingBottom) || 0;
  const markScroll = () => node.classList.toggle('more-below', node.scrollTop + node.clientHeight < node.scrollHeight - 1);
  const measure = () => {
    const bottoms = rows().map(row => row.offsetTop + row.offsetHeight);
    const cap = requestListHeight(node.scrollHeight, bottoms, null, paddingBottom());
    if (cap > 0) node.style.setProperty('--request-list-cap', `${Math.ceil(cap)}px`);
    markScroll();
  };
  const resize = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(measure);
  const observeRows = () => {
    if (!resize) return;
    resize.disconnect();
    resize.observe(node);
    for (const row of rows()) resize.observe(row);
  };
  const mutation = typeof MutationObserver === 'undefined' ? null : new MutationObserver(() => { observeRows(); measure(); });
  mutation?.observe(node, { childList: true });
  node.addEventListener('scroll', markScroll, { passive: true });
  observeRows();
  measure();
  return {
    destroy() {
      resize?.disconnect();
      mutation?.disconnect();
      node.removeEventListener('scroll', markScroll);
    },
  };
}
