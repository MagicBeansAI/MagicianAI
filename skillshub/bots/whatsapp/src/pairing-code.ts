import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

/**
 * "Link with phone number" for one WhatsApp login.
 *
 * wu-cli's `createConnection` calls `requestPairingCode` on every QR refresh
 * while `pairingPhone` is set, and each request replaces the previous code.
 * The connection reads `pairingPhone` through {@link PairingCodeRequest.phone},
 * which hands out the phone only until a code was issued, so the person sees
 * one stable code; later refreshes fall back to the QR.
 */
export interface PairingCodeRequest {
  /** The phone for wu-cli, until a code was issued for this login. */
  phone(): string | undefined;
  /** Publish a code wu-cli received (never logged). */
  onCode(code: string): void;
}

/** Digits only: Baileys builds the companion JID from the raw number. */
export function normalizePairingPhone(raw: string | undefined): string | undefined {
  const digits = (raw ?? "").replace(/[^0-9]/g, "");
  return digits.length >= 6 && digits.length <= 15 ? digits : undefined;
}

export function writePairingCodeFile(file: string, code: string): void {
  mkdirSync(dirname(file), { recursive: true, mode: 0o700 });
  writeFileSync(file, `${code}\n`, { mode: 0o600 });
}

export function createPairingCodeRequest(options: {
  phone: string | undefined;
  codeFile: string | undefined;
  write?: (file: string, code: string) => void;
  log?: (message: string) => void;
}): PairingCodeRequest {
  const phone = normalizePairingPhone(options.phone);
  const write = options.write ?? writePairingCodeFile;
  const log = options.log ?? ((message: string) => console.info(message));
  let issued = false;
  return {
    phone: () => (issued ? undefined : phone),
    onCode: (code: string) => {
      issued = true;
      const codeFile = options.codeFile?.trim();
      if (!codeFile) {
        log("[whatsapp-bot] pairing code received but WHATSAPP_PAIRING_CODE_FILE is not set");
        return;
      }
      try {
        write(codeFile, code);
        log("[whatsapp-bot] pairing code ready — enter it in WhatsApp → Linked devices → Link with phone number");
      } catch (error) {
        console.error("[whatsapp-bot] failed to publish the pairing code", error);
      }
    },
  };
}
