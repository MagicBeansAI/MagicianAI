/** Pi 0.87.1 Plane bridge. Loaded explicitly; all native tools are disabled. */
import { readFile, writeFile } from "node:fs/promises";

export default async function magicianPlane(pi) {
  const configPath = process.env.MAGICIAN_PI_PLANE_CONFIG;
  const readyPath = process.env.MAGICIAN_PI_PLANE_READY;
  if (!configPath || !readyPath) throw new Error("Pi Plane bridge is not configured");
  const { url, grant } = JSON.parse(await readFile(configPath, "utf8"));
  if (!url || !grant) throw new Error("Pi Plane bridge config is incomplete");
  let sessionId;
  let nextId = 0;

  async function rpc(method, params, signal) {
    const headers = {
      "content-type": "application/json",
      accept: "application/json, text/event-stream",
      authorization: `Bearer ${grant}`,
    };
    if (sessionId) headers["mcp-session-id"] = sessionId;
    const response = await fetch(url, {
      method: "POST", headers, signal,
      body: JSON.stringify({ jsonrpc: "2.0", id: ++nextId, method, params }),
    });
    if (!response.ok) throw new Error(`Magician Plane ${method} returned HTTP ${response.status}`);
    const newSession = response.headers.get("mcp-session-id");
    if (newSession) sessionId = newSession;
    const body = await response.json();
    if (body.error) throw new Error(`Magician Plane ${method}: ${body.error.message}`);
    return body.result;
  }

  await rpc("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "magician-pi", version: "0.87.1" },
  });
  const listed = await rpc("tools/list", {});
  if (!Array.isArray(listed?.tools)) throw new Error("Magician Plane tools/list failed");

  async function call(name, args, signal) {
    const result = await rpc("tools/call", { name, arguments: args || {} }, signal);
    if (result?.isError) {
      throw new Error(result.content?.map((item) => item.text || "").join("\n") || `${name} failed`);
    }
    return { content: result?.content || [{ type: "text", text: JSON.stringify(result) }], details: result?.structuredContent };
  }

  for (const tool of listed.tools) {
    if (!tool.name || !tool.inputSchema) continue;
    pi.registerTool({
      name: tool.name,
      label: tool.title || tool.name,
      description: tool.description || tool.name,
      parameters: tool.inputSchema,
      async execute(_id, args, signal) { return call(tool.name, args, signal); },
    });
  }
  // tool_search may authorize a deferred tool after registration. This
  // explicit dispatcher reaches that newly loaded name through the same
  // MCP authority check without requiring an extension reload mid-turn.
  pi.registerTool({
    name: "magician_plane_call",
    label: "Call a loaded Magician tool",
    description: "Call a Magician Plane tool that tool_search just loaded. Use its exact tool name and arguments. The Plane grant checks authorization.",
    parameters: {
      type: "object", required: ["name"],
      properties: {
        name: { type: "string" },
        arguments: { type: "object", additionalProperties: true },
      },
    },
    async execute(_id, args, signal) { return call(args.name, args.arguments, signal); },
  });
  await writeFile(readyPath, "ready", { mode: 0o600, flag: "wx" });
}
