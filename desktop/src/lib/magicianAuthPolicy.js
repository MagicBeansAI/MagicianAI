/** Compare exact origins, including loopback ports. WebSockets map to HTTP.
 * @param {string} value
 * @param {string} origin
 */
export function isSelectedMagicianUrl(value, origin) {
  try {
    const url = new URL(value);
    if (url.protocol === "ws:") url.protocol = "http:";
    if (url.protocol === "wss:") url.protocol = "https:";
    return ["http:", "https:"].includes(url.protocol)
      && !url.username && !url.password
      && url.origin === new URL(origin).origin
      && url.pathname.startsWith("/api/magician/");
  } catch {
    return false;
  }
}
