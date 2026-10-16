/** Parse one supported mailbox, never an address hidden inside its display
 * name. Ambiguous lists, groups and unsupported syntax carry no identity. */
export function parseEmailMailbox(value: unknown): string | undefined {
  if (typeof value !== "string" || value.length > 4096 || /[\x00-\x1f\x7f]/.test(value)) return undefined;
  const text = value.trim();
  // A quoted display name can contain angle brackets and commas; they are
  // decoration, not the mailbox. Anchor the complete header before selecting.
  const named = /^(?:"(?:[^"\\]|\\.)*"|[^"<>(),;:\\]+)?\s*<([^<>]+)>$/.exec(text);
  const address = (named?.[1] ?? text).trim();
  if (address.length > 254) return undefined;
  const parts = address.split("@");
  if (parts.length !== 2) return undefined;
  const [local, domain] = parts as [string, string];
  if (!local || local.length > 64 || !domain) return undefined;
  // Support ordinary dot-atom mailboxes and DNS/IDNA domains. Do not guess at
  // comments, quoted local parts or address literals used as a principal.
  if (!local.split(".").every(part => /^[A-Za-z0-9!#$%&'*+/=?^_`{|}~-]+$/.test(part))) return undefined;
  if (!domain.split(".").every(label => /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?$/.test(label))) return undefined;
  return address.toLowerCase();
}
