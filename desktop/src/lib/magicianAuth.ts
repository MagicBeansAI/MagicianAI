import { invoke } from "@tauri-apps/api/core";

import { isSelectedMagicianUrl } from "./magicianAuthPolicy.js";

interface ConnectionAuth { origin: string; token: string | null; revision: number }

export async function magicianConnectionAuth(): Promise<ConnectionAuth> {
  // Read origin and credential atomically. Do not retain a process-wide promise
  // that could outlive an engine switch, logout or another window's login.
  return invoke<ConnectionAuth>("get_magician_connection_auth");
}

export async function setMagicianBearerToken(token: string | null, expectedOrigin: string, expectedRevision: number): Promise<void> {
  await invoke("set_magician_bearer_token", { token: token?.trim() || null, expectedOrigin, expectedRevision });
}

export async function signInMagician(username: string, password: string): Promise<void> {
  await invoke("sign_in_magician", { username: username.trim(), password });
}

export async function signOutMagician(): Promise<void> {
  await invoke("logout_magician_session");
}

export async function magicianFetch(
  input: RequestInfo | URL,
  init: RequestInit = {},
): Promise<Response> {
  const raw = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
  const auth = await magicianConnectionAuth();
  if (!isSelectedMagicianUrl(raw, auth.origin)) {
    throw new Error("Refusing to attach the Magician bearer to an untrusted URL.");
  }
  const headers = new Headers(init.headers ?? (input instanceof Request ? input.headers : undefined));
  headers.delete("X-Principal");
  headers.delete("X-Workspace");
  headers.delete("Authorization");
  if (init.signal?.aborted) throw new DOMException("The operation was aborted.", "AbortError");
  if (init.body != null && typeof init.body !== "string") {
    throw new Error("Desktop Magician requests support text and JSON bodies only.");
  }
  const result = await invoke<{
    status: number;
    headers: Array<[string, string]>;
    body: number[];
  }>("magician_http_request", {
    request: {
      url: raw,
      method: init.method ?? (input instanceof Request ? input.method : "GET"),
      headers: Object.fromEntries(headers.entries()),
      body: typeof init.body === "string" ? init.body : null,
    },
  });
  return new Response(new Uint8Array(result.body), {
    status: result.status,
    headers: result.headers,
  });
}

export const MAGICIAN_REALTIME_WEBSOCKET_PROTOCOL = "magician-events-v2";

export async function magicianWebSocketProtocols(url: string, protocols: string[] = []): Promise<string[]> {
  const applicationProtocols = protocols.filter(
    (protocol) => !protocol.trim().startsWith("magician-bearer."),
  );
  const auth = await magicianConnectionAuth();
  if (!isSelectedMagicianUrl(url, auth.origin)) {
    throw new Error("Refusing to attach the Magician bearer to an untrusted WebSocket URL.");
  }
  const token = auth.token;
  return token
    ? [...applicationProtocols, `magician-bearer.${token}`]
    : applicationProtocols;
}
