import { invoke } from "@tauri-apps/api/core";

interface ContextualAssistWebviewContext {
  route?: string;
  url?: string;
  title?: string;
  selectedText?: string;
  fieldText?: string;
  editable: boolean;
  hasSelection: boolean;
  secure: boolean;
}

const TEXT_INPUT_TYPES = new Set([
  "",
  "email",
  "number",
  "search",
  "tel",
  "text",
  "url",
]);
const TEXTBOX_ROLES = new Set(["combobox", "searchbox", "textbox"]);
const SECURE_AUTOCOMPLETE_RE = /\b(current-password|new-password|one-time-code|password)\b/i;
const SENSITIVE_FIELD_RE =
  /\b(password|passcode|passphrase|secret|token|api[-_\s]*key|apikey|access[-_\s]*key|private[-_\s]*key|ssh[-_\s]*key|client[-_\s]*secret|credential|bearer|oauth|refresh[-_\s]*token|recovery[-_\s]*code|mnemonic)\b/i;
const SENSITIVE_CONTAINER_SELECTOR = [
  "[data-contextual-assist-exclude]",
  "[data-contextual-assist-private]",
  "[data-sensitive]",
  "[data-secret]",
  "[data-private]",
].join(",");
const MAX_CONTEXT_TEXT_CHARS = 50_000;

export function startContextualAssistWebviewTracker(): () => void {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
    return () => {};
  }

  let disposed = false;
  let sendTimer: number | null = null;
  let lastPayload = "";

  const schedule = () => {
    if (disposed) return;
    if (sendTimer !== null) window.clearTimeout(sendTimer);
    sendTimer = window.setTimeout(() => {
      sendTimer = null;
      void sendCurrentContext();
    }, 40);
  };

  async function sendCurrentContext() {
    if (disposed) return;
    const context = currentContext();
    const serialized = JSON.stringify(context);
    if (serialized === lastPayload) return;
    lastPayload = serialized;
    try {
      await invoke("update_contextual_assist_webview_context", { context });
    } catch {
      // This tracker also runs in browser dev builds; missing Tauri is harmless.
    }
  }

  const events: Array<[EventTarget, string, EventListenerOrEventListenerObject]> = [
    [document, "selectionchange", schedule],
    [document, "focusin", schedule],
    [document, "focusout", schedule],
    [document, "input", schedule],
    [document, "keyup", schedule],
    [document, "pointerup", schedule],
    [window, "blur", schedule],
  ];
  for (const [target, eventName, listener] of events) {
    target.addEventListener(eventName, listener, true);
  }
  schedule();

  return () => {
    disposed = true;
    if (sendTimer !== null) window.clearTimeout(sendTimer);
    for (const [target, eventName, listener] of events) {
      target.removeEventListener(eventName, listener, true);
    }
  };
}

function currentContext(): ContextualAssistWebviewContext {
  const active = deepActiveElement();
  const writable = writableElement(active);
  const secure =
    isSecureElement(writable) ||
    (active instanceof HTMLElement && isSecureElement(active)) ||
    pageSelectionIsSecure();
  const editable = Boolean(writable && !secure);
  const selectedText = secure ? "" : trimContextText(controlSelectionText(writable) || pageSelectionText());
  const fieldText = editable ? trimContextText(elementContextText(writable)) : "";

  return {
    route: `${location.pathname}${location.search}`,
    url: location.href,
    title: document.title,
    selectedText: selectedText || undefined,
    fieldText: fieldText || undefined,
    editable,
    hasSelection: Boolean(selectedText),
    secure,
  };
}

function deepActiveElement(root: Document | ShadowRoot = document): Element | null {
  let active = root.activeElement;
  while (active?.shadowRoot?.activeElement) {
    active = active.shadowRoot.activeElement;
  }
  return active;
}

function writableElement(element: Element | null): HTMLElement | null {
  if (!(element instanceof HTMLElement)) return null;
  if (isSecureElement(element)) return element;
  if (isWritableElement(element)) return element;
  const editableAncestor = element.closest<HTMLElement>("[contenteditable=''], [contenteditable='true']");
  if (editableAncestor && (isSecureElement(editableAncestor) || isWritableElement(editableAncestor))) {
    return editableAncestor;
  }
  return null;
}

function isWritableElement(element: HTMLElement | null): boolean {
  if (!element || isDisabledOrReadonly(element)) return false;
  if (element instanceof HTMLTextAreaElement) return true;
  if (element instanceof HTMLInputElement) return TEXT_INPUT_TYPES.has(inputType(element));
  if (element.isContentEditable) return true;

  const role = (element.getAttribute("role") || "").trim().toLowerCase();
  return TEXTBOX_ROLES.has(role);
}

function inputType(element: HTMLInputElement): string {
  return (element.getAttribute("type") || "text").trim().toLowerCase();
}

function isDisabledOrReadonly(element: HTMLElement): boolean {
  if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) {
    return element.disabled || element.readOnly;
  }
  return element.getAttribute("aria-disabled") === "true" || element.getAttribute("aria-readonly") === "true";
}

function isSecureElement(element: HTMLElement | null): boolean {
  if (!element) return false;
  if (element.closest(SENSITIVE_CONTAINER_SELECTOR)) return true;
  if (element instanceof HTMLInputElement && inputType(element) === "password") return true;
  if (SECURE_AUTOCOMPLETE_RE.test(element.getAttribute("autocomplete") || "")) return true;
  return SENSITIVE_FIELD_RE.test(sensitiveFieldDescriptor(element));
}

function sensitiveFieldDescriptor(element: HTMLElement): string {
  const parts = [
    element.getAttribute("type"),
    element.getAttribute("name"),
    element.id,
    element.getAttribute("aria-label"),
    element.getAttribute("placeholder"),
    element.getAttribute("autocomplete"),
    element.getAttribute("role"),
    element.getAttribute("data-field"),
    element.getAttribute("data-name"),
    element.getAttribute("data-testid"),
  ];

  if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) {
    for (const label of Array.from(element.labels || [])) {
      parts.push(label.textContent || "");
    }
  }

  const labelledBy = element.getAttribute("aria-labelledby");
  if (labelledBy) {
    for (const id of labelledBy.split(/\s+/)) {
      const label = document.getElementById(id);
      if (label) parts.push(label.textContent || "");
    }
  }

  return parts.filter(Boolean).join(" ");
}

function controlSelectionText(element: HTMLElement | null): string {
  if (!(element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement)) return "";
  if (isSecureElement(element)) return "";
  try {
    if (
      typeof element.selectionStart === "number" &&
      typeof element.selectionEnd === "number" &&
      element.selectionEnd > element.selectionStart
    ) {
      return String(element.value || "").slice(element.selectionStart, element.selectionEnd);
    }
  } catch {
    return "";
  }
  return "";
}

function pageSelectionText(): string {
  try {
    const selection = window.getSelection();
    if (!selection || selection.rangeCount === 0 || selection.isCollapsed) return "";
    return selection.toString() || "";
  } catch {
    return "";
  }
}

function pageSelectionIsSecure(): boolean {
  try {
    const selection = window.getSelection();
    if (!selection || selection.rangeCount === 0 || selection.isCollapsed) return false;
    for (let index = 0; index < selection.rangeCount; index += 1) {
      const range = selection.getRangeAt(index);
      if (selectionNodeIsSecure(range.startContainer) || selectionNodeIsSecure(range.endContainer)) {
        return true;
      }
    }
  } catch {
    return false;
  }
  return false;
}

function selectionNodeIsSecure(node: Node | null): boolean {
  const element = node instanceof HTMLElement ? node : node?.parentElement;
  return Boolean(element?.closest(SENSITIVE_CONTAINER_SELECTOR));
}

function elementContextText(element: HTMLElement | null): string {
  if (!element || isSecureElement(element)) return "";
  if (element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement) {
    return String(element.value || "");
  }
  if (element.isContentEditable) {
    return String(element.innerText || element.textContent || "");
  }
  return String(element.textContent || "");
}

function trimContextText(text: string): string {
  return String(text || "").trim().slice(0, MAX_CONTEXT_TEXT_CHARS);
}
