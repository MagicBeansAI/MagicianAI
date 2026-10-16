import { parseEmailMailbox } from "@magician/bot-sdk";

/** Prepare a recipient-bound, exact-body Gmail reply without sending it.
 * The CLI's +reply helper quotes the original email, so tracked messages use
 * the raw MIME endpoint and retain only threading headers from the source. */
export async function prepareEnvoyReply(
  exec: (args: string[]) => Promise<string>, messageId: string, text: string,
): Promise<{ channelAddress: string; send(): Promise<{ accepted: boolean }> }> {
  if (!messageId) throw new Error("Gmail reply requires a message id");
  const original = JSON.parse(await exec(["gmail", "users", "messages", "get", "--params",
    JSON.stringify({ userId: "me", id: messageId, format: "metadata" })]));
  if (original.id !== messageId || typeof original.threadId !== "string" || !original.threadId) {
    throw new Error("Gmail reply metadata does not match the selected message");
  }
  const headers: Array<{ name: string; value: string }> = original.payload?.headers;
  if (!Array.isArray(headers)) throw new Error("Gmail reply headers unavailable");
  const header = (name: string): string => {
    const matches = headers.filter((h) => typeof h.name === "string" && h.name.toLowerCase() === name);
    if (matches.length > 1) throw new Error(`Ambiguous Gmail ${name} header`);
    const value = matches[0]?.value ?? "";
    if (typeof value !== "string" || /[\r\n\0]/.test(value)) throw new Error(`Invalid Gmail ${name} header`);
    return value;
  };
  const channelAddress = mailbox(header("reply-to") || header("from"));
  const profile = JSON.parse(await exec(["gmail", "users", "getProfile", "--params", '{"userId":"me"}']));
  const sender = mailbox(profile.emailAddress);
  const inReplyTo = header("message-id");
  if (!/^<[^<>\s]+>$/.test(inReplyTo)) throw new Error("Gmail reply lacks a safe Message-ID");
  const references = header("references");
  if (references && !/^(<[^<>\s]+>\s*)+$/.test(references)) throw new Error("Invalid Gmail References header");
  const subject = header("subject");
  // Keep the source subject for Gmail's thread match, encoded without allowing
  // source header text to introduce another MIME header or recipient.
  const encodedSubject = /^[\x20-\x7e]*$/.test(subject) ? subject : Array.from(subject).reduce<string[]>((chunks, char) => {
    const last = chunks.length - 1;
    if (last < 0 || Buffer.byteLength(chunks[last]! + char) > 42) chunks.push(char);
    else chunks[last] += char;
    return chunks;
  }, []).map((chunk) => `=?UTF-8?B?${Buffer.from(chunk).toString("base64")}?=`).join("\r\n ");
  const mime = [
    `From: ${sender}`, `To: ${channelAddress}`, `Subject: ${encodedSubject}`,
    `In-Reply-To: ${inReplyTo}`, `References: ${references ? references + " " : ""}${inReplyTo}`,
    "MIME-Version: 1.0", 'Content-Type: text/plain; charset="UTF-8"', "Content-Transfer-Encoding: base64", "",
    Buffer.from(text, "utf8").toString("base64").match(/.{1,76}/g)?.join("\r\n") ?? "",
  ].join("\r\n");
  const body = JSON.stringify({ threadId: original.threadId, raw: Buffer.from(mime).toString("base64url") });
  return {
    channelAddress,
    async send() {
      const receipt = JSON.parse(await exec(["gmail", "users", "messages", "send", "--params", '{"userId":"me"}', "--json", body]));
      return { accepted: typeof receipt.id === "string" && receipt.id.length > 0 };
    },
  };
}

function mailbox(value: unknown): string {
  const email = parseEmailMailbox(value);
  if (!email) throw new Error("Gmail requires one unambiguous supported mailbox");
  return email;
}
