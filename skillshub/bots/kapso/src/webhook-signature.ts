import { createHmac, timingSafeEqual } from "node:crypto";

const SHA256_HEX_LENGTH = 64;

/**
 * Verify Kapso's HMAC-SHA256 signature over the exact webhook request bytes.
 *
 * Kapso sends the lowercase hexadecimal digest in `X-Webhook-Signature`. The
 * regular-expression check both rejects malformed input and guarantees equal
 * buffer lengths before `timingSafeEqual` is called.
 */
export function verifyKapsoWebhookSignature(
  rawBody: Buffer,
  signatureHeader: string | undefined,
  webhookSecret: string,
): boolean {
  if (!webhookSecret || !signatureHeader) return false;

  const signature = signatureHeader.trim();
  if (
    signature.length !== SHA256_HEX_LENGTH ||
    !/^[0-9a-fA-F]+$/.test(signature)
  ) {
    return false;
  }

  const expected = createHmac("sha256", webhookSecret).update(rawBody).digest();
  const supplied = Buffer.from(signature, "hex");
  return (
    supplied.length === expected.length && timingSafeEqual(supplied, expected)
  );
}
