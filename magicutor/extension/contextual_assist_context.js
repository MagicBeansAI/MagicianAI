(function () {
    const TEXT_INPUT_TYPES = new Set([
        '',
        'email',
        'number',
        'search',
        'tel',
        'text',
        'url'
    ]);
    const TEXTBOX_ROLES = new Set(['combobox', 'searchbox', 'textbox']);
    const SECURE_AUTOCOMPLETE_RE = /\b(current-password|new-password|one-time-code|password)\b/i;
    const SENSITIVE_FIELD_RE = /\b(password|passcode|passphrase|secret|token|api[-_\s]*key|apikey|access[-_\s]*key|private[-_\s]*key|ssh[-_\s]*key|client[-_\s]*secret|credential|bearer|oauth|refresh[-_\s]*token|recovery[-_\s]*code|mnemonic)\b/i;
    const SENSITIVE_CONTAINER_SELECTOR = [
        '[data-contextual-assist-exclude]',
        '[data-contextual-assist-private]',
        '[data-sensitive]',
        '[data-secret]',
        '[data-private]'
    ].join(',');
    const MAX_CONTEXT_TEXT_CHARS = 50000;

    function isHtmlInput(element) {
        return typeof HTMLInputElement !== 'undefined' && element instanceof HTMLInputElement;
    }

    function isHtmlTextArea(element) {
        return typeof HTMLTextAreaElement !== 'undefined' && element instanceof HTMLTextAreaElement;
    }

    function deepActiveElement(root = document) {
        let active = root.activeElement || null;
        while (active?.shadowRoot?.activeElement) {
            active = active.shadowRoot.activeElement;
        }
        return active;
    }

    function inputType(element) {
        return (element.getAttribute('type') || 'text').trim().toLowerCase();
    }

    function isDisabledOrReadonly(element) {
        return element.disabled === true
            || element.readOnly === true
            || element.getAttribute('aria-disabled') === 'true'
            || element.getAttribute('aria-readonly') === 'true';
    }

    function isSecureElement(element) {
        if (!element) return false;
        if (element.closest?.(SENSITIVE_CONTAINER_SELECTOR)) return true;
        if (isHtmlInput(element) && inputType(element) === 'password') return true;
        if (SECURE_AUTOCOMPLETE_RE.test(element.getAttribute?.('autocomplete') || '')) return true;
        return SENSITIVE_FIELD_RE.test(sensitiveFieldDescriptor(element));
    }

    function sensitiveFieldDescriptor(element) {
        const parts = [
            element.getAttribute?.('type'),
            element.getAttribute?.('name'),
            element.id,
            element.getAttribute?.('aria-label'),
            element.getAttribute?.('placeholder'),
            element.getAttribute?.('autocomplete'),
            element.getAttribute?.('role'),
            element.getAttribute?.('data-field'),
            element.getAttribute?.('data-name'),
            element.getAttribute?.('data-testid')
        ];

        if (isHtmlInput(element) || isHtmlTextArea(element)) {
            for (const label of Array.from(element.labels || [])) {
                parts.push(label.textContent || '');
            }
        }

        const labelledBy = element.getAttribute?.('aria-labelledby');
        if (labelledBy) {
            for (const id of labelledBy.split(/\s+/)) {
                const label = document.getElementById(id);
                if (label) parts.push(label.textContent || '');
            }
        }

        return parts.filter(Boolean).join(' ');
    }

    function isWritableElement(element) {
        if (!element || isDisabledOrReadonly(element) || isSecureElement(element)) {
            return false;
        }
        if (isHtmlTextArea(element)) return true;
        if (isHtmlInput(element)) return TEXT_INPUT_TYPES.has(inputType(element));
        if (element.isContentEditable) return true;

        const role = (element.getAttribute?.('role') || '').trim().toLowerCase();
        return TEXTBOX_ROLES.has(role);
    }

    function controlSelectionText(element) {
        if (!isHtmlInput(element) && !isHtmlTextArea(element)) return '';
        if (isSecureElement(element)) return '';
        try {
            if (
                typeof element.selectionStart === 'number'
                && typeof element.selectionEnd === 'number'
                && element.selectionEnd > element.selectionStart
            ) {
                return String(element.value || '').slice(element.selectionStart, element.selectionEnd);
            }
        } catch (_) {
            return '';
        }
        return '';
    }

    function pageSelectionText() {
        try {
            const selection = window.getSelection?.();
            if (!selection || selection.rangeCount === 0 || selection.isCollapsed) return '';
            return selection.toString() || '';
        } catch (_) {
            return '';
        }
    }

    function pageSelectionIsSecure() {
        try {
            const selection = window.getSelection?.();
            if (!selection || selection.rangeCount === 0 || selection.isCollapsed) return false;
            for (let index = 0; index < selection.rangeCount; index += 1) {
                const range = selection.getRangeAt(index);
                if (selectionNodeIsSecure(range.startContainer) || selectionNodeIsSecure(range.endContainer)) {
                    return true;
                }
            }
        } catch (_) {
            return false;
        }
        return false;
    }

    function selectionNodeIsSecure(node) {
        const element = node instanceof HTMLElement ? node : node?.parentElement;
        return Boolean(element?.closest?.(SENSITIVE_CONTAINER_SELECTOR));
    }

    function elementValueLength(element) {
        if (!element) return 0;
        if (isSecureElement(element)) return 0;
        if (isHtmlInput(element) || isHtmlTextArea(element)) {
            return String(element.value || '').trim().length;
        }
        if (element.isContentEditable) {
            return String(element.innerText || element.textContent || '').trim().length;
        }
        return String(element.textContent || '').trim().length;
    }

    function elementContextText(element) {
        if (!element || isSecureElement(element) || !isWritableElement(element)) return '';
        if (isHtmlInput(element) || isHtmlTextArea(element)) {
            return String(element.value || '');
        }
        if (element.isContentEditable) {
            return String(element.innerText || element.textContent || '');
        }
        return String(element.textContent || '');
    }

    function trimContextText(text) {
        const trimmed = String(text || '').trim();
        if (!trimmed) return '';
        return trimmed.slice(0, MAX_CONTEXT_TEXT_CHARS);
    }

    function elementKind(element) {
        if (!element?.tagName) return null;
        if (isHtmlInput(element)) return `input:${inputType(element) || 'text'}`;
        if (isHtmlTextArea(element)) return 'textarea';
        if (element.isContentEditable) return 'contenteditable';
        const role = element.getAttribute?.('role');
        return role ? `role:${role}` : element.tagName.toLowerCase();
    }

    function pageUrl() {
        try {
            if (location.protocol !== 'http:' && location.protocol !== 'https:') return null;
            return location.href;
        } catch (_) {
            return null;
        }
    }

    function probeContextualAssist() {
        const active = deepActiveElement();
        const secure = isSecureElement(active) || pageSelectionIsSecure();
        const isWritable = isWritableElement(active);
        const selectedInControl = secure ? '' : controlSelectionText(active);
        const selectedInPage = secure || selectedInControl ? '' : pageSelectionText();
        const selectedTextLength = (selectedInControl || selectedInPage).trim().length;
        const hasSelection = selectedTextLength > 0;
        const valueLength = elementValueLength(active);

        let eligible = false;
        let reason = 'none';
        let state = 'none';

        if (secure) {
            reason = 'secure_field';
            state = 'secure-field';
        } else if (hasSelection && isWritable) {
            eligible = true;
            reason = selectedInControl ? 'input_selection' : 'page_selection_in_field';
            state = 'selection-field';
        } else if (hasSelection) {
            eligible = true;
            reason = 'page_selection';
            state = 'selection';
        } else if (isWritable) {
            eligible = true;
            reason = 'focused_writable';
            state = valueLength > 0 ? 'draft' : 'empty-context';
        }

        return {
            ok: true,
            eligible,
            reason,
            state,
            isWritable,
            hasSelection,
            secure,
            valueLength,
            selectedTextLength,
            activeElement: elementKind(active),
            url: pageUrl(),
            title: document.title || null,
            frameUrl: pageUrl(),
            topFrame: window.top === window,
            timestamp: Date.now()
        };
    }

    function contextualAssistContext() {
        const active = deepActiveElement();
        const secure = isSecureElement(active) || pageSelectionIsSecure();
        const isWritable = isWritableElement(active);
        if (secure) {
            return {
                ok: true,
                contextText: null,
                contextKind: 'secure-field',
                isWritable,
                hasSelection: false,
                secure,
                selectedTextLength: 0,
                valueLength: 0,
                activeElement: elementKind(active),
                url: pageUrl(),
                title: document.title || null,
                frameUrl: pageUrl(),
                topFrame: window.top === window,
                timestamp: Date.now()
            };
        }

        const selectedText = trimContextText(controlSelectionText(active) || pageSelectionText());
        if (selectedText) {
            return {
                ok: true,
                contextText: selectedText,
                contextKind: isWritable ? 'selection-field' : 'selection',
                isWritable,
                hasSelection: true,
                secure,
                selectedTextLength: selectedText.length,
                valueLength: elementValueLength(active),
                activeElement: elementKind(active),
                url: pageUrl(),
                title: document.title || null,
                frameUrl: pageUrl(),
                topFrame: window.top === window,
                timestamp: Date.now()
            };
        }

        if (!isWritable || secure) {
            return {
                ok: true,
                contextText: null,
                contextKind: secure ? 'secure-field' : 'none',
                isWritable,
                hasSelection: false,
                secure,
                selectedTextLength: 0,
                valueLength: elementValueLength(active),
                activeElement: elementKind(active),
                url: pageUrl(),
                title: document.title || null,
                frameUrl: pageUrl(),
                topFrame: window.top === window,
                timestamp: Date.now()
            };
        }

        const contextText = trimContextText(elementContextText(active));
        return {
            ok: true,
            contextText: contextText || null,
            contextKind: contextText ? 'draft' : 'empty-context',
            isWritable,
            hasSelection: false,
            secure,
            selectedTextLength: 0,
            valueLength: elementValueLength(active),
            activeElement: elementKind(active),
            url: pageUrl(),
            title: document.title || null,
            frameUrl: pageUrl(),
            topFrame: window.top === window,
            timestamp: Date.now()
        };
    }

    chrome.runtime.onMessage.addListener((message, _sender, sendResponse) => {
        if (message?.type === 'contextual_assist_probe') {
            sendResponse(probeContextualAssist());
            return true;
        }
        if (message?.type === 'contextual_assist_context') {
            sendResponse(contextualAssistContext());
            return true;
        }
        return undefined;
    });
})();
