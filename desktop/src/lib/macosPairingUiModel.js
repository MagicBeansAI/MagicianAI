const BUNDLE_ID_MAX_BYTES = 255;

/**
 * @typedef {object} MacosPairingApprovalStatus
 * @property {boolean} ownerApprovalRequired
 * @property {string | null | undefined} setupId
 * @property {number | null | undefined} generation
 * @property {string | null | undefined} scopeBindingRef
 * @property {unknown[] | null | undefined} requestedTargets
 * @property {string | null | undefined} reviewMaterialDigest
 */

/**
 * @param {ReadableStream<Uint8Array> | null | undefined} body
 * @param {number} maxBytes
 */
export async function readBoundedResponseText(body, maxBytes) {
  if (!body || !Number.isSafeInteger(maxBytes) || maxBytes <= 0) {
    throw new Error("missing or invalid bounded response body");
  }
  const reader = body.getReader();
  const chunks = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      if (!(value instanceof Uint8Array) || total + value.byteLength > maxBytes) {
        await reader.cancel("oversized pairing response");
        throw new Error("oversized response");
      }
      chunks.push(value);
      total += value.byteLength;
    }
  } finally {
    reader.releaseLock();
  }
  if (total === 0) throw new Error("invalid response");
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

/** @param {unknown} value */
export function validMacosBundleId(value) {
  return typeof value === "string"
    && value.length > 2
    && value.length <= BUNDLE_ID_MAX_BYTES
    && value.split(".").length >= 2
    && value.split(".").every((segment) => /^[A-Za-z0-9-]+$/.test(segment));
}

/**
 * @param {MacosPairingApprovalStatus | null | undefined} status
 * @param {string} confirmedDigest
 */
export function canApproveMacosPairing(status, confirmedDigest) {
  return Boolean(
    status?.ownerApprovalRequired
      && status.setupId
      && typeof status.generation === "number"
      && Number.isSafeInteger(status.generation)
      && status.generation > 0
      && status.scopeBindingRef
      && Array.isArray(status.requestedTargets)
      && status.requestedTargets.length > 0
      && status.requestedTargets.length <= 64
      && status.reviewMaterialDigest
      && status.reviewMaterialDigest === confirmedDigest,
  );
}

/** @param {unknown} phase */
export function macosPairingPhaseLabel(phase) {
  switch (phase) {
    case "pending_native_approval": return "Waiting for native review";
    case "approved_pending_finalize": return "Approved; finalization pending";
    case "active": return "Active";
    case "revoked": return "Revoked";
    case "unpaired": return "Not paired";
    default: return "Unavailable";
  }
}
