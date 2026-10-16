/**
 * Shadow-DOM-aware querySelector/querySelectorAll code snippet for injection into
 * Runtime.evaluate scripts. Tries document.querySelector first, then traverses all
 * shadow roots. Include this at the start of any IIFE that needs to find elements
 * inside shadow DOM.
 *
 * Provides two functions:
 *   deepQuerySelector(sel)    — returns first match (or null)
 *   deepQuerySelectorAll(sel) — returns Array of all matches
 */
const DEEP_QS = `
    function deepQuerySelector(sel) {
        let found = document.querySelector(sel);
        if (found) return found;
        const walk = (root) => {
            for (const host of root.querySelectorAll('*')) {
                if (host.shadowRoot) {
                    found = host.shadowRoot.querySelector(sel);
                    if (found) return found;
                    const nested = walk(host.shadowRoot);
                    if (nested) return nested;
                }
            }
            return null;
        };
        return walk(document);
    }
    function deepQuerySelectorAll(sel) {
        const results = Array.from(document.querySelectorAll(sel));
        const walk = (root) => {
            for (const host of root.querySelectorAll('*')) {
                if (host.shadowRoot) {
                    results.push(...host.shadowRoot.querySelectorAll(sel));
                    walk(host.shadowRoot);
                }
            }
        };
        walk(document);
        return results;
    }
`;

export {
    DEEP_QS
};
