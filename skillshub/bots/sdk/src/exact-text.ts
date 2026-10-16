/** Split a prepared payload without dropping whitespace or splitting a Unicode
 * surrogate pair. Joining the chunks reconstructs the exact prepared text. */
export function chunkExactText(text: string, maxLength: number): string[] {
  if (!Number.isSafeInteger(maxLength) || maxLength < 2) {
    throw new Error("maxLength must be an integer of at least 2");
  }
  const chunks: string[] = [];
  let start = 0;
  while (start < text.length) {
    let end = Math.min(start + maxLength, text.length);
    if (end < text.length) {
      const split = Math.max(text.lastIndexOf("\n", end - 1), text.lastIndexOf(" ", end - 1));
      if (split >= start) end = split + 1;
      const before = text.charCodeAt(end - 1);
      const after = text.charCodeAt(end);
      if (before >= 0xd800 && before <= 0xdbff && after >= 0xdc00 && after <= 0xdfff) end -= 1;
    }
    chunks.push(text.slice(start, end));
    start = end;
  }
  return chunks;
}
