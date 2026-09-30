const DEFAULT_TEXT_CHUNK_LIMIT = 4_000;

export function chunkTelegramText(
  text: string,
  maxLength = DEFAULT_TEXT_CHUNK_LIMIT,
): string[] {
  if (maxLength < 1) {
    throw new Error("maxLength must be at least 1");
  }

  if (text.length <= maxLength) {
    return [text];
  }

  const chunks: string[] = [];
  let start = 0;

  while (start < text.length) {
    let end = Math.min(start + maxLength, text.length);
    if (end < text.length) {
      const newlineSplit = text.lastIndexOf("\n", end - 1);
      const whitespaceSplit = text.lastIndexOf(" ", end - 1);
      const splitIndex = Math.max(newlineSplit, whitespaceSplit);

      if (splitIndex >= start) {
        end = splitIndex + 1;
      }
    }

    chunks.push(text.slice(start, end));
    start = end;
  }

  return chunks;
}

/**
 * Does this failure mean Telegram rejected the inline button's URL, rather
 * than rejecting the message? Telegram validates a button URL and refuses a
 * non-public one, which failed the ENTIRE critical alert — the owner was told
 * nothing because a decoration could not be drawn. The card's text already
 * carries the same link, so this is the one case where dropping the button and
 * keeping the alert is right. Matched on Telegram's own wording.
 */
export function isRejectedButtonUrl(error: unknown): boolean {
  const message = (error instanceof Error ? error.message : String(error)).toLowerCase();
  return message.includes("wrong http url")
    || (message.includes("button url") && message.includes("invalid"));
}
