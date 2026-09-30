import assert from "node:assert/strict";
import test from "node:test";

import {
  MagicianClient as ScopedMagicianClient,
  MagicianRealtimeConnection,
} from "../dist/index.js";

class MagicianClient extends ScopedMagicianClient {
  constructor(options) {
    super({ bearerToken: "mag_pat_test", ...options });
  }
}

function jsonResponse(body, init = {}) {
  return new Response(JSON.stringify(body), {
    status: init.status ?? 200,
    headers: {
      "content-type": "application/json",
    },
  });
}

class FakeSocket extends EventTarget {
  constructor() {
    super();
    this.readyState = 0;
    this.closed = false;
  }

  open() {
    this.readyState = 1;
    this.dispatchEvent(new Event("open"));
  }

  emitMessage(data) {
    this.dispatchEvent(new MessageEvent("message", { data }));
  }

  close() {
    this.closed = true;
    this.readyState = 3;
    this.dispatchEvent(new Event("close"));
  }
}

test("MagicianClient includes channel identity on chat requests", async () => {
  const requests = [];
  const client = new MagicianClient({
    baseUrl: "http://localhost:3002/",
    fetchImpl: async (url, init) => {
      requests.push({
        url: String(url),
        method: init?.method ?? "GET",
        headers: new Headers(init?.headers),
      });

      if (String(url).includes("/api/magician/v2/chat/active?")) {
        return jsonResponse({
          session: {
            id: "session-1",
            principal: "default",
            workspace: "default",
            agent_id: "personal-assistant",
            title: null,
            origin_channel: {
              channel_type: "telegram",
              address: "chat-42",
            },
            status: "active",
            created_at: 1,
            updated_at: 1,
          },
          messages: [],
        });
      }

      throw new Error(`Unexpected request: ${String(url)}`);
    },
  });

  await client.getActiveSession({
    principal: "default",
    channelType: "telegram",
    channelAddress: "chat-42",
  });

  assert.equal(requests.length, 1);
  assert.match(requests[0].url, /^http:\/\/localhost:3002\/api\/magician\/v2\/chat\/active\?/);
  assert.equal(new URL(requests[0].url).searchParams.has("principal"), false);
  assert.equal(requests[0].headers.get("Authorization"), "Bearer mag_pat_test");
  assert.match(requests[0].url, /channel=telegram/);
  assert.match(requests[0].url, /channel_address=chat-42/);
});

test("MagicianClient includes control intent when provided", async () => {
  const requests = [];
  const client = new MagicianClient({
    baseUrl: "http://localhost:3002/",
    fetchImpl: async (url, init) => {
      requests.push({
        url: String(url),
        method: init?.method ?? "GET",
      });

      if (String(url).includes("/api/magician/v2/chat/active?")) {
        return jsonResponse({
          session: {
            id: "session-1",
            principal: "default",
            workspace: "default",
            agent_id: "envoy",
            title: null,
            origin_channel: {
              channel_type: "kapso",
              address: "9199",
            },
            status: "active",
            created_at: 1,
            updated_at: 1,
          },
          messages: [],
        });
      }

      throw new Error(`Unexpected request: ${String(url)}`);
    },
  });

  await client.getActiveSession({
    principal: "default",
    channelType: "kapso",
    channelAddress: "9199",
    controlIntent: false,
  });

  assert.equal(requests.length, 1);
  assert.match(requests[0].url, /control_intent=false/);
});

test("MagicianClient parses realtime events with injected websocket factory", async () => {
  const socket = new FakeSocket();
  const events = [];
  const websocketUrls = [];
  const websocketProtocols = [];
  const client = new MagicianClient({
    baseUrl: "http://localhost:3002",
    websocketFactory: (url, protocols) => {
      websocketUrls.push(url);
      websocketProtocols.push(protocols);
      return socket;
    },
  });

  const connection = client.connectRealtime((event) => {
    events.push(event);
  });

  assert.ok(connection instanceof MagicianRealtimeConnection);
  assert.equal(
    websocketUrls[0],
    "ws://localhost:3002/api/magician/v2/realtime/ws",
  );
  // The runtime negotiates against its own list and echoes the protocol it
  // selected. Offering the bearer alone leaves it nothing it recognises, so it
  // selects none and omits `Sec-WebSocket-Protocol` from the 101 — which a
  // strict client must reject. The bearer still rides second because a
  // WebSocket cannot carry an Authorization header.
  assert.deepEqual(websocketProtocols[0], [
    "magician-events-v2",
    "magician-bearer.mag_pat_test",
  ]);
  const openPromise = connection.waitForOpen();
  socket.open();
  await openPromise;

  socket.emitMessage(
    JSON.stringify({
      event_type: "ChatMessageReceived",
      data: {
        session_id: "session-1",
        message: {
          id: "message-1",
          session_id: "session-1",
          direction: "system",
          content: {
            type: "task_status_update",
            task_id: "task-1",
            status: "running",
            summary: "Still working",
          },
          created_at: 10,
        },
      },
    }),
  );

  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(events.length, 1);
  assert.equal(events[0].event_type, "ChatMessageReceived");
  assert.equal(events[0].data.session_id, "session-1");

  connection.close();
  assert.equal(socket.closed, true);
});

test("MagicianClient keeps enrollment scope in the bearer", async () => {
  const requests = [];
  const client = new MagicianClient({
    baseUrl: "http://localhost:3002",
    fetchImpl: async (url, init) => {
      requests.push({
        url: String(url),
        method: init?.method ?? "GET",
        body: init?.body ? JSON.parse(String(init.body)) : null,
        headers: new Headers(init?.headers),
      });
      return jsonResponse({ enrolled: true, principal: "default" });
    },
  });

  await client.enroll({
    channelType: "telegram",
    channelAddress: "chat-42",
    displayName: "Alex",
  });

  assert.equal(requests.length, 1);
  assert.equal("workspace" in requests[0].body, false);
  assert.equal(requests[0].headers.get("Authorization"), "Bearer mag_pat_test");
});

test("MagicianClient resolves runtime workspace from the bearer session", async () => {
  const requests = [];
  const client = new MagicianClient({
    baseUrl: "http://localhost:3002",
    fetchImpl: async (url, init) => {
      requests.push({
        url: String(url),
        headers: new Headers(init?.headers),
      });
      return jsonResponse({ workspace: "project-alpha" });
    },
  });

  assert.deepEqual(await client.resolveBearerScope(), { workspace: "project-alpha" });
  assert.equal(requests[0].url, "http://localhost:3002/api/magician/v2/auth/session");
  assert.equal(requests[0].headers.get("Authorization"), "Bearer mag_pat_test");
  assert.equal(requests[0].headers.get("X-Principal"), null);
  assert.equal(requests[0].headers.get("X-Workspace"), null);
});
