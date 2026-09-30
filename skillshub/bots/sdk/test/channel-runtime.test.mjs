import assert from "node:assert/strict";
import test from "node:test";

import { ChannelRuntime, MagicianClient as ScopedMagicianClient } from "../dist/index.js";

class MagicianClient extends ScopedMagicianClient {
  constructor(options) {
    super({ bearerToken: "mag_pat_test", ...options });
  }

  async resolveBearerScope() {
    return { workspace: "default" };
  }

  // These conversation fixtures have no prepared Envoy act. Receipt behavior
  // is exercised with the real HTTP client in envoy-delivery.test.mjs.
  async reportEnvoyDelivery() { return { tracked: false, send: true, status: "not_envoy" }; }
}

function jsonResponse(body, init = {}) {
  return new Response(JSON.stringify(body), {
    status: init.status ?? 200,
    headers: {
      "content-type": "application/json",
    },
  });
}

class FakeAdapter {
  constructor() {
    this.handlers = null;
    this.sent = [];
  }

  async start(handlers) {
    this.handlers = handlers;
  }

  async sendText(target, text, options) {
    const message = { kind: "text", target, text };
    if (options !== undefined) {
      message.options = options;
    }
    this.sent.push(message);
  }
}

function queryParam(request, name) {
  return new URL(request.url).searchParams.get(name);
}

test("ChannelRuntime runs enroll -> active session -> send message flow", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    // Phase 3 drain loop calls GET /queue after every successful turn —
    // existing single-turn tests should respond with an empty queue so the
    // drain is a no-op.
    if (
      (init?.method ?? "GET") === "GET"
      && String(url).includes("/api/magician/v2/chat/sessions/session-1/queue?")
    ) {
      return jsonResponse({ queued: [], session_id: "session-1" });
    }

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        user_message: {
          id: "user-1",
          session_id: "session-1",
          direction: "user",
          content: {
            type: "text",
            text: "hello",
          },
          created_at: 2,
        },
        assistant_message: {
          id: "assistant-1",
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "text",
            text: "Hi there",
          },
          created_at: 3,
        },
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "hello",
    displayName: "Alex",
  });

  // 3 functional calls + 1 post-turn drain check (GET /queue, empty) = 4
  assert.equal(requests.length, 4);
  assert.equal("workspace" in requests[0].body, false);
  assert.match(requests[1].url, /channel=telegram/);
  assert.match(requests[1].url, /channel_address=chat-42/);
  assert.equal(new URL(requests[2].url).searchParams.has("principal"), false);
  assert.equal(requests[2].body.sender_display_name, "Alex");
  // Last request is the drain-loop's queue-empty check
  assert.match(requests[3].url, /\/queue\?/);
  assert.equal(adapter.sent[0].kind, "text");
  assert.equal(adapter.sent[0].text, "Hi there");
  assert.equal(adapter.sent.length, 1);
});

test("ChannelRuntime prefers structured presentation plain_text for text replies", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/queue?")) {
      return jsonResponse({ queued: [], session_id: "session-1" });
    }

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        user_message: {
          id: "user-1",
          session_id: "session-1",
          direction: "user",
          content: {
            type: "text",
            text: "hello",
          },
          created_at: 2,
        },
        assistant_message: {
          id: "assistant-1",
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "text",
            text: "Legacy reply",
          },
          presentation: {
            schema: "magician.structured_response",
            version: 1,
            plain_text: "Presented reply",
          },
          created_at: 3,
        },
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "hello",
    displayName: "Alex",
  });

  assert.equal(adapter.sent[0].kind, "text");
  assert.equal(adapter.sent[0].text, "Presented reply");
});

test("ChannelRuntime ignores invalid structured presentation and falls back to legacy chat text", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/queue?")) {
      return jsonResponse({ queued: [], session_id: "session-1" });
    }

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        user_message: {
          id: "user-1",
          session_id: "session-1",
          direction: "user",
          content: {
            type: "text",
            text: "hello",
          },
          created_at: 2,
        },
        assistant_message: {
          id: "assistant-1",
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "text",
            text: "Legacy reply",
          },
          presentation: {
            schema: "unknown.schema",
            version: 1,
            plain_text: "Presented reply",
          },
          created_at: 3,
        },
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "hello",
    displayName: "Alex",
  });

  assert.equal(adapter.sent[0].kind, "text");
  assert.equal(adapter.sent[0].text, "Legacy reply");
});

test("ChannelRuntime drops unsupported message content without sending a fallback", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/queue?")) {
      return jsonResponse({ queued: [], session_id: "session-1" });
    }

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        user_message: {
          id: "user-1",
          session_id: "session-1",
          direction: "user",
          content: {
            type: "text",
            text: "hello",
          },
          created_at: 2,
        },
        assistant_message: {
          id: "assistant-1",
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "future_card",
            text: "Legacy future card fallback",
          },
          created_at: 3,
        },
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "hello",
    displayName: "Alex",
  });

  assert.deepEqual(adapter.sent, []);
});

test("ChannelRuntime prefers escalation question over generic fallback text", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/queue?")) {
      return jsonResponse({ queued: [], session_id: "session-1" });
    }

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        user_message: {
          id: "user-1",
          session_id: "session-1",
          direction: "user",
          content: {
            type: "text",
            text: "hello",
          },
          created_at: 2,
        },
        assistant_message: {
          id: "assistant-1",
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "escalation",
            question: "Approve this request?",
          },
          created_at: 3,
        },
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "hello",
    displayName: "Alex",
  });

  assert.equal(adapter.sent[0].kind, "text");
  assert.equal(adapter.sent[0].text, "Approve this request?");
});

test("ChannelRuntime falls back to escalation summary when question is missing", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/queue?")) {
      return jsonResponse({ queued: [], session_id: "session-1" });
    }

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        user_message: {
          id: "user-1",
          session_id: "session-1",
          direction: "user",
          content: {
            type: "text",
            text: "hello",
          },
          created_at: 2,
        },
        assistant_message: {
          id: "assistant-1",
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "escalation",
            summary: "Escalation summary fallback",
          },
          created_at: 3,
        },
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "hello",
    displayName: "Alex",
  });

  assert.equal(adapter.sent[0].kind, "text");
  assert.equal(adapter.sent[0].text, "Escalation summary fallback");
});

test("ChannelRuntime sends pending-approval message without opening a session", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });
    return jsonResponse({ enrolled: false, code: "ABC123" });
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "discord",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "discord:user-1",
    channelAddress: "user-1",
    text: "hello",
  });

  assert.equal(requests.length, 1);
  assert.equal("workspace" in requests[0].body, false);
  assert.deepEqual(adapter.sent, [
    {
      kind: "text",
      target: "discord:user-1",
      text: "Pending approval. An admin will connect you shortly.",
    },
  ]);
});

test("ChannelRuntime control prefix strips tag and marks control intent", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

    if (String(url).includes("/api/magician/v2/chat/active?")) {
      return jsonResponse({
        session: {
          id: "session-1",
          principal: "default",
          workspace: "default",
          agent_id: "personal-assistant",
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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        user_message: {
          id: "user-1",
          session_id: "session-1",
          direction: "user",
          content: {
            type: "text",
            text: "hello",
          },
          created_at: 2,
        },
        assistant_message: {
          id: "assistant-1",
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "text",
            text: "Hi there",
          },
          created_at: 3,
        },
        pending_queue_depth: 0,
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "kapso",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
    controlPrefix: "@magic",
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "wa:9199",
    channelAddress: "9199",
    text: "@magic: hello",
  });

  assert.equal(requests.length, 3);
  assert.match(requests[1].url, /control_intent=true/);
  assert.equal(requests[2].body.text, "hello");
  assert.equal(adapter.sent[0].text, "Hi there");
});

test("ChannelRuntime requests per-address ordinary and magic control threads", async () => {
  const requests = [];
  const sentTexts = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

    if (String(url).includes("/api/magician/v2/chat/active?")) {
      return jsonResponse({
        session: {
          id: "session-1",
          principal: "default",
          workspace: "default",
          agent_id: "personal-assistant",
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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      const text = JSON.parse(String(init.body)).text;
      sentTexts.push(text);
      return jsonResponse({
        user_message: {
          id: `user-${sentTexts.length}`,
          session_id: "session-1",
          direction: "user",
          content: {
            type: "text",
            text,
          },
          created_at: 2,
        },
        assistant_message: {
          id: `assistant-${sentTexts.length}`,
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "text",
            text: `reply ${sentTexts.length}`,
          },
          created_at: 3,
        },
        pending_queue_depth: 0,
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "kapso",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
    controlPrefix: "/magic",
    controlPrefixes: ["@magic"],
    nonControlMessageBehavior: "dispatch",
    channelThreading: "per-address",
    nonControlMessageDispatch: {
      sourceSurface: "kapso-envoy-chat",
      allowOutsideWindowTemplate: false,
    },
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "wa:9199",
    channelAddress: "9199",
    text: "hello",
  });
  await adapter.handlers.onTextMessage({
    target: "wa:9199",
    channelAddress: "9199",
    text: "/magic run report",
  });

  const activeRequests = requests.filter((r) =>
    r.url.includes("/api/magician/v2/chat/active?"),
  );
  const messageRequests = requests.filter((r) =>
    r.url.includes("/api/magician/v2/chat/sessions/session-1/messages?"),
  );

  assert.deepEqual(
    activeRequests.map((r) => queryParam(r, "ui_thread_id")),
    ["ext:kapso:9199", "ext:kapso:9199:magic"],
  );
  assert.deepEqual(
    activeRequests.map((r) => queryParam(r, "control_intent")),
    ["false", "true"],
  );
  assert.deepEqual(messageRequests.map((r) => r.body.text), [
    "hello",
    "run report",
  ]);
});

test("ChannelRuntime keeps /new in the requested ordinary or magic thread", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

    if (
      (init?.method ?? "GET") === "POST"
      && String(url).includes("/api/magician/v2/chat/new?")
    ) {
      return jsonResponse({
        session: {
          id: `session-${requests.length}`,
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
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
    controlPrefix: "/magic",
    controlPrefixes: ["@magic"],
    nonControlMessageBehavior: "dispatch",
    channelThreading: "per-address",
    nonControlMessageDispatch: {
      sourceSurface: "telegram-envoy-chat",
      allowOutsideWindowTemplate: false,
    },
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "/new",
  });
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "/magic /new",
  });

  const newRequests = requests.filter((r) =>
    r.url.includes("/api/magician/v2/chat/new?"),
  );

  assert.deepEqual(
    newRequests.map((r) => queryParam(r, "ui_thread_id")),
    ["ext:telegram:chat-42", "ext:telegram:chat-42:magic"],
  );
  assert.deepEqual(
    newRequests.map((r) => queryParam(r, "control_intent")),
    ["false", "true"],
  );
  assert.deepEqual(adapter.sent.map((m) => m.text), [
    "New conversation started.",
    "New conversation started.",
  ]);
  assert.deepEqual(adapter.sent.map((m) => m.options), [
    { allowOutsideWindowTemplate: false },
    undefined,
  ]);
});

test("ChannelRuntime accepts slash control prefix and legacy aliases", async () => {
  const requests = [];
  const sentTexts = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      const text = JSON.parse(String(init.body)).text;
      sentTexts.push(text);
      return jsonResponse({
        user_message: {
          id: `user-${sentTexts.length}`,
          session_id: "session-1",
          direction: "user",
          content: {
            type: "text",
            text,
          },
          created_at: 2,
        },
        assistant_message: {
          id: `assistant-${sentTexts.length}`,
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "text",
            text: `reply ${sentTexts.length}`,
          },
          created_at: 3,
        },
        pending_queue_depth: 0,
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
    controlPrefix: "/magic",
    controlPrefixes: ["@magic"],
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "/magic@TestBot: do one thing",
  });
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "@magic do another thing",
  });

  const activeRequests = requests.filter((r) =>
    r.url.includes("/api/magician/v2/chat/active?"),
  );
  assert.equal(activeRequests.length, 2);
  assert.ok(activeRequests.every((r) => r.url.includes("control_intent=true")));
  assert.deepEqual(sentTexts, ["do one thing", "do another thing"]);
});

test("ChannelRuntime can ignore non-control messages before enrollment", async () => {
  const requests = [];
  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "whatsapp",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl: async (url, init) => {
        requests.push({
          url: String(url),
          method: init?.method ?? "GET",
        });
        throw new Error(`Unexpected request: ${String(url)}`);
      },
    }),
    controlPrefix: "@magic",
    nonControlMessageBehavior: "ignore",
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "self@s.whatsapp.net",
    channelAddress: "self@s.whatsapp.net",
    text: "remember this normally",
  });

  assert.equal(requests.length, 0);
  assert.deepEqual(adapter.sent, []);
});

test("ChannelRuntime handles SDK slash commands on prefix-gated public chat", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    if (
      (init?.method ?? "GET") === "DELETE"
      && String(url).includes("/api/magician/v2/chat/sessions/session-1/run?")
    ) {
      return jsonResponse({
        cancelled: true,
        session_id: "session-1",
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "kapso",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
    controlPrefix: "@magic",
    nonControlMessageBehavior: "dispatch",
    nonControlMessageDispatch: {
      sourceSurface: "kapso-envoy-chat",
      allowOutsideWindowTemplate: false,
    },
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "wa:9199",
    channelAddress: "9199",
    text: "/stop",
  });

  assert.equal(requests.length, 3);
  assert.match(requests[1].url, /control_intent=false/);
  assert.equal(requests[2].method, "DELETE");
  assert.match(requests[2].url, /\/run\?/);
  assert.equal(adapter.sent[0].text, "Stopped.");
  assert.deepEqual(adapter.sent[0].options, {
    allowOutsideWindowTemplate: false,
  });
});

test("ChannelRuntime applies non-control dispatch profile and delivery policy", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        user_message: {
          id: "user-1",
          session_id: "session-1",
          direction: "user",
          content: {
            type: "text",
            text: "hello",
          },
          created_at: 2,
        },
        assistant_message: {
          id: "assistant-1",
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "text",
            text: "Hi from envoy chat",
          },
          created_at: 3,
        },
        pending_queue_depth: 0,
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "kapso",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
    controlPrefix: "@magic",
    nonControlMessageBehavior: "dispatch",
    nonControlMessageDispatch: {
      profile: "chat-gpt6luna-responses-vision-toolsauto-fast",
      sourceSurface: "kapso-envoy-chat",
      allowOutsideWindowTemplate: false,
    },
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "wa:9199",
    channelAddress: "9199",
    text: "hello",
  });

  assert.match(requests[1].url, /control_intent=false/);
  assert.equal(
    requests[2].body.profile,
    "chat-gpt6luna-responses-vision-toolsauto-fast",
  );
  assert.equal(requests[2].body.source_surface, "kapso-envoy-chat");
  assert.equal(adapter.sent[0].text, "Hi from envoy chat");
  assert.deepEqual(adapter.sent[0].options, {
    allowOutsideWindowTemplate: false,
  });
});

test("ChannelRuntime allows templates when backend marks public-chat notice template_allowed", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        assistant_message: {
          id: "assistant-1",
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "text",
            text: "I got your message. I am a bit busy and will reply shortly.",
          },
          created_at: 3,
        },
        pending_queue_depth: 0,
        public_chat_notice: {
          kind: "queued",
          template_allowed: true,
          reason: "queued_after_capacity:position:1",
        },
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "kapso",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
    controlPrefix: "@magic",
    nonControlMessageBehavior: "dispatch",
    nonControlMessageDispatch: {
      sourceSurface: "kapso-envoy-chat",
      allowOutsideWindowTemplate: false,
    },
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "wa:9199",
    channelAddress: "9199",
    text: "hello",
  });

  assert.equal(
    adapter.sent[0].text,
    "I got your message. I am a bit busy and will reply shortly.",
  );
  assert.deepEqual(adapter.sent[0].options, {
    allowOutsideWindowTemplate: true,
  });
});

test("ChannelRuntime preserves template suppression when backend notice disallows templates", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

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

    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        assistant_message: {
          id: "assistant-1",
          session_id: "session-1",
          direction: "assistant",
          content: {
            type: "text",
            text: "I am switching to a lower-cost mode for today, but I can still chat.",
          },
          created_at: 3,
        },
        pending_queue_depth: 0,
        public_chat_notice: {
          kind: "fallback_notice",
          template_allowed: false,
          reason: "fallback_route",
        },
      });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "kapso",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
    controlPrefix: "@magic",
    nonControlMessageBehavior: "dispatch",
    nonControlMessageDispatch: {
      sourceSurface: "kapso-envoy-chat",
      allowOutsideWindowTemplate: false,
    },
  });

  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "wa:9199",
    channelAddress: "9199",
    text: "hello",
  });

  assert.equal(
    adapter.sent[0].text,
    "I am switching to a lower-cost mode for today, but I can still chat.",
  );
  assert.deepEqual(adapter.sent[0].options, {
    allowOutsideWindowTemplate: false,
  });
});

test("ChannelRuntime handles explicit connect requests without sending a chat message", async () => {
  const requests = [];
  const fetchImpl = async (url, init) => {
    requests.push({
      url: String(url),
      method: init?.method ?? "GET",
      body: init?.body ? JSON.parse(String(init.body)) : null,
    });

    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }

    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl,
    }),
  });

  await runtime.start();
  await adapter.handlers.onConnectRequest({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    displayName: "Alex",
  });

  assert.equal(requests.length, 1);
  assert.equal("workspace" in requests[0].body, false);
  assert.deepEqual(adapter.sent, [
    {
      kind: "text",
      target: "tg:chat-42",
      text: "Connected. You're chatting as default.",
    },
  ]);
});

// ─────────────────────────────────────────────────────────────────────
// Phase 3 — slash commands + drain loop
// ─────────────────────────────────────────────────────────────────────

/** Shared scaffolding: enroll + active-session + send-message responses
 *  the tests below all need, plus a routable fetch helper. */
function makePhase3Server() {
  const requests = [];
  const queueState = { items: [] };
  let cancelCount = 0;
  let lastSendResponse = null;

  const baseSession = {
    id: "session-1",
    principal: "default",
    workspace: "default",
    agent_id: "personal-assistant",
    title: null,
    origin_channel: { channel_type: "telegram", address: "chat-42" },
    status: "active",
    created_at: 1,
    updated_at: 1,
  };

  const fetchImpl = async (url, init) => {
    const u = String(url);
    const method = init?.method ?? "GET";
    requests.push({ url: u, method });

    if (u.endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }
    if (u.includes("/api/magician/v2/chat/active?")) {
      return jsonResponse({ session: baseSession, messages: [] });
    }
    if (
      method === "POST"
      && u.includes("/api/magician/v2/chat/sessions/session-1/messages?")
    ) {
      // Override hook for queued responses
      if (lastSendResponse) {
        const r = lastSendResponse;
        lastSendResponse = null;
        return jsonResponse(r);
      }
      return jsonResponse({
        user_message: { id: `u-${requests.length}`, session_id: "session-1", direction: "user", content: { type: "text", text: "x" }, created_at: 2 },
        assistant_message: { id: `a-${requests.length}`, session_id: "session-1", direction: "assistant", content: { type: "text", text: `reply-${requests.length}` }, created_at: 3 },
      });
    }
    if (method === "DELETE" && u.includes("/run?")) {
      cancelCount++;
      return jsonResponse({ cancelled: true, session_id: "session-1" });
    }
    if (method === "GET" && u.includes("/queue?")) {
      return jsonResponse({ queued: [...queueState.items], session_id: "session-1" });
    }
    if (
      method === "DELETE"
      && u.includes("/queue/")
      && !u.endsWith("/queue?")
    ) {
      const match = u.match(/\/queue\/([^?]+)/);
      const id = match ? decodeURIComponent(match[1]) : null;
      const before = queueState.items.length;
      queueState.items = queueState.items.filter((m) => m.id !== id);
      return jsonResponse({
        deleted: before !== queueState.items.length,
        session_id: "session-1",
        message_id: id,
      });
    }
    if (method === "DELETE" && u.includes("/queue?")) {
      const cleared = queueState.items.length;
      queueState.items = [];
      return jsonResponse({ cleared, session_id: "session-1" });
    }

    throw new Error(`Unexpected request: ${method} ${u}`);
  };

  return {
    fetchImpl,
    requests,
    queueState,
    counts: () => ({ cancel: cancelCount }),
    setNextSendResponse(r) { lastSendResponse = r; },
  };
}

function makeRuntime(server) {
  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({
      baseUrl: "http://localhost:3002",
      fetchImpl: server.fetchImpl,
    }),
  });
  return { adapter, runtime };
}

test("/stop slash command calls cancelChatRun and acknowledges", async () => {
  const server = makePhase3Server();
  const { adapter, runtime } = makeRuntime(server);
  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "/stop",
  });
  assert.equal(server.counts().cancel, 1, "cancelChatRun should be called exactly once");
  assert.ok(
    adapter.sent.some((m) => m.text === "Stopped."),
    `expected 'Stopped.' acknowledgement; got: ${JSON.stringify(adapter.sent)}`,
  );
  // No message dispatch should have happened
  assert.ok(
    !adapter.sent.some((m) => m.text?.startsWith("reply-")),
    "no agent reply should be sent on /stop",
  );
});

test("/clearqueue empties the backend queue and reports count", async () => {
  const server = makePhase3Server();
  server.queueState.items = [
    { id: "q1", session_id: "session-1", text: "a", queued_at: 1 },
    { id: "q2", session_id: "session-1", text: "b", queued_at: 2 },
  ];
  const { adapter, runtime } = makeRuntime(server);
  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "/clearqueue",
  });
  assert.equal(server.queueState.items.length, 0);
  assert.ok(
    adapter.sent.some((m) => m.text === "Cleared 2 queued messages."),
    `expected cleared-count message; got: ${JSON.stringify(adapter.sent)}`,
  );
});

test("/queue lists pending messages", async () => {
  const server = makePhase3Server();
  server.queueState.items = [
    { id: "q1", session_id: "session-1", text: "first thing", queued_at: 1 },
    { id: "q2", session_id: "session-1", text: "second thing", queued_at: 2 },
  ];
  const { adapter, runtime } = makeRuntime(server);
  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "/queue",
  });
  const listMsg = adapter.sent.find((m) => m.text?.startsWith("2 queued:"));
  assert.ok(listMsg, `expected queue list message; got: ${JSON.stringify(adapter.sent)}`);
  assert.match(listMsg.text, /1\. first thing/);
  assert.match(listMsg.text, /2\. second thing/);
});

test("normal message gets a queued hint when backend responds with queued receipt", async () => {
  const server = makePhase3Server();
  server.setNextSendResponse({
    user_message: null,
    assistant_message: null,
    tool_executed: [],
    session_title: null,
    queued: { id: "q-new", position: 2, dropped_oldest: null },
  });
  const { adapter, runtime } = makeRuntime(server);
  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "while busy",
  });
  const hint = adapter.sent.find((m) => m.text?.startsWith("Queued (#2)"));
  assert.ok(hint, `expected queued hint; got: ${JSON.stringify(adapter.sent)}`);
  assert.match(hint.text, /Type \/stop/);
});

test("drain loop replays queued messages after the in-flight turn settles", async () => {
  const server = makePhase3Server();
  // Pre-seed two queued messages — after the user's regular turn finishes,
  // the drain loop should replay both.
  server.queueState.items = [
    { id: "q1", session_id: "session-1", text: "queued-1", queued_at: 1 },
    { id: "q2", session_id: "session-1", text: "queued-2", queued_at: 2 },
  ];
  const { adapter, runtime } = makeRuntime(server);
  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42",
    channelAddress: "chat-42",
    text: "hello",
  });
  // Initial reply + two drained replies = 3 agent messages delivered
  const replies = adapter.sent.filter((m) => m.text?.startsWith("reply-"));
  assert.equal(replies.length, 3, `expected 3 replies (1 + 2 drained); got ${JSON.stringify(adapter.sent)}`);
  // Queue should be empty after drain
  assert.equal(server.queueState.items.length, 0);
});

// P3 Task 3.11: a channel never solicits a secret into its transcript.
test("ChannelRuntime relays a notice, not the question, for a secret ask", async () => {
  const { sensitiveAskNotice } = await import("../dist/index.js");
  assert.equal(sensitiveAskNotice({ type: "escalation", question: "Which quarter?" }), null);
  assert.equal(
    sensitiveAskNotice({ type: "escalation", question: "Which quarter?", input_type: "choice" }),
    null,
  );
  const code = sensitiveAskNotice({
    type: "escalation",
    question: "Enter the verification code we sent",
    input_type: "text",
    input_schema: { sensitive: { kind: "otp", one_time: true } },
  });
  assert.match(code, /verification code/);
  assert.match(code, /don't send it here/);
  assert.doesNotMatch(code, /Enter the verification code we sent/);
  assert.match(
    sensitiveAskNotice({ type: "escalation", question: "Password?", input_type: "password" }),
    /your password/,
  );
  assert.match(
    sensitiveAskNotice({
      type: "escalation",
      question: "Sign in",
      input_type: "form",
      input_schema: { sensitive: { fields: [{ id: "user", kind: "login_identifier" }, { id: "pw", kind: "password" }] } },
    }),
    /sign-in details/,
  );
  assert.match(
    sensitiveAskNotice({
      type: "escalation",
      question: "Password for example.com",
      input_type: "password",
      input_schema: { request_type: "secure_browser_input" },
    }),
    /your password/,
  );

  const fetchImpl = async (url, init) => {
    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }
    if (String(url).includes("/api/magician/v2/chat/active?")) {
      return jsonResponse({
        session: {
          id: "session-1", principal: "default", workspace: "default", agent_id: "personal-assistant",
          title: null, origin_channel: { channel_type: "telegram", address: "chat-42" },
          status: "active", created_at: 1, updated_at: 1,
        },
        messages: [],
      });
    }
    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/queue?")) {
      return jsonResponse({ queued: [], session_id: "session-1" });
    }
    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      return jsonResponse({
        user_message: {
          id: "user-1", session_id: "session-1", direction: "user",
          content: { type: "text", text: "log me in" }, created_at: 2,
        },
        assistant_message: {
          id: "assistant-1", session_id: "session-1", direction: "assistant",
          content: {
            type: "escalation",
            question: "Enter the verification code we sent to your phone",
            input_type: "text",
            input_schema: { sensitive: { kind: "otp", provenance: "heuristic", one_time: true } },
          },
          created_at: 3,
        },
      });
    }
    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({ baseUrl: "http://localhost:3002", fetchImpl }),
  });
  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42", channelAddress: "chat-42", text: "log me in", displayName: "Alex",
  });
  assert.equal(adapter.sent[0].kind, "text");
  assert.match(adapter.sent[0].text, /verification code/);
  assert.doesNotMatch(adapter.sent[0].text, /sent to your phone/);
});

// P3 Task 3.11: a reply on the channel is not accepted as the answer.
test("ChannelRuntime refuses a channel reply while a secret ask is open", async () => {
  const { SENSITIVE_REPLY_REFUSAL, sensitiveAskExpiry, SENSITIVE_ASK_REPLY_HOLD_MS } = await import("../dist/index.js");
  const now = 1_000_000;
  assert.equal(
    sensitiveAskExpiry({ type: "escalation", input_schema: { sensitive: { kind: "otp", collection_deadline_ms: now + 90_000 } } }, now),
    now + 90_000,
  );
  assert.equal(sensitiveAskExpiry({ type: "escalation", input_type: "password" }, now), now + SENSITIVE_ASK_REPLY_HOLD_MS);

  const forwarded = [];
  const deadline = Date.now() + 150;
  const fetchImpl = async (url, init) => {
    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }
    if (String(url).includes("/api/magician/v2/chat/active?")) {
      return jsonResponse({
        session: {
          id: "session-1", principal: "default", workspace: "default", agent_id: "personal-assistant",
          title: null, origin_channel: { channel_type: "telegram", address: "chat-42" },
          status: "active", created_at: 1, updated_at: 1,
        },
        messages: [],
      });
    }
    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/queue?")) {
      return jsonResponse({ queued: [], session_id: "session-1" });
    }
    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      const body = JSON.parse(init.body);
      forwarded.push(body.text);
      if (forwarded.length === 1) {
        return jsonResponse({
          user_message: {
            id: "user-1", session_id: "session-1", direction: "user",
            content: { type: "text", text: body.text }, created_at: 2,
          },
          assistant_message: {
            id: "assistant-1", session_id: "session-1", direction: "assistant",
            content: {
              type: "escalation",
              question: "Enter the verification code we sent to your phone",
              input_type: "text",
              input_schema: { sensitive: { kind: "otp", one_time: true, collection_deadline_ms: deadline } },
            },
            created_at: 3,
          },
        });
      }
      return jsonResponse({
        user_message: {
          id: `user-${forwarded.length}`, session_id: "session-1", direction: "user",
          content: { type: "text", text: body.text }, created_at: 4,
        },
        assistant_message: {
          id: `assistant-${forwarded.length}`, session_id: "session-1", direction: "assistant",
          content: { type: "text", text: "Noted." }, created_at: 5,
        },
      });
    }
    throw new Error(`Unexpected request: ${String(url)}`);
  };

  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({ baseUrl: "http://localhost:3002", fetchImpl }),
  });
  await runtime.start();
  const inbound = (text) => adapter.handlers.onTextMessage({
    target: "tg:chat-42", channelAddress: "chat-42", text, displayName: "Alex",
  });
  await inbound("log me in");
  assert.match(adapter.sent[0].text, /verification code/);

  // The code typed into the channel is refused, not forwarded.
  await inbound("482913");
  assert.deepEqual(forwarded, ["log me in"], "the reply must not reach the transcript");
  assert.equal(adapter.sent[1].text, SENSITIVE_REPLY_REFUSAL);

  // Once the ask's own window has closed the channel forwards again.
  await new Promise((resolve) => setTimeout(resolve, 200));
  await inbound("what now?");
  assert.deepEqual(forwarded, ["log me in", "what now?"]);
  assert.equal(adapter.sent[2].text, "Noted.");
});

test("ChannelRuntime recovers the sensitive-ask hold from the session after a restart", async () => {
  // The hold lived only in one process's memory, and the realtime feed is
  // live-only: a restart forgot every open ask while the pause was still open,
  // so the owner's password — sent to the channel they had just been told not
  // to use — was accepted as an ordinary message into the transcript.
  const { SENSITIVE_REPLY_REFUSAL } = await import("../dist/index.js");
  const forwarded = [];
  const escalation = {
    id: "assistant-1", session_id: "session-1", direction: "assistant",
    content: {
      type: "escalation",
      question: "Enter the verification code we sent to your phone",
      input_type: "text",
      input_schema: { sensitive: { kind: "otp", one_time: true, collection_deadline_ms: Date.now() + 120_000 } },
    },
    created_at: 3,
  };
  const fetchImpl = async (url, init = {}) => {
    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }
    if (String(url).includes("/api/magician/v2/chat/active?")) {
      return jsonResponse({
        session: {
          id: "session-1", principal: "default", workspace: "default", agent_id: "personal-assistant",
          title: null, origin_channel: { channel_type: "telegram", address: "chat-42" },
          status: "active", created_at: 1, updated_at: 1,
        },
        // The server's own record of the ask, which this process never saw.
        messages: [escalation],
      });
    }
    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/queue?")) {
      return jsonResponse({ queued: [], session_id: "session-1" });
    }
    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      forwarded.push(JSON.parse(init.body).text);
      return jsonResponse({
        user_message: { id: "user-1", session_id: "session-1", direction: "user", content: { type: "text", text: "x" }, created_at: 4 },
        assistant_message: { id: "assistant-2", session_id: "session-1", direction: "assistant", content: { type: "text", text: "Noted." }, created_at: 5 },
      });
    }
    throw new Error(`Unexpected request: ${String(url)}`);
  };
  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({ baseUrl: "http://localhost:3002", fetchImpl }),
  });
  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42", channelAddress: "chat-42", text: "482913", displayName: "Alex",
  });
  assert.deepEqual(forwarded, [], "a fresh process still refuses the value");
  assert.equal(adapter.sent[0].text, SENSITIVE_REPLY_REFUSAL);
});

test("ChannelRuntime does not recover a hold from an ask whose window has closed", async () => {
  // A recovered hold must expire WITH the ask. Arming it with the
  // just-delivered fallback restarted a fresh fifteen minutes from the same
  // stale row on every message — and no `escalation_resolved` row is written
  // for an agentic resolution, so the row stays newest after the owner answers
  // in the UI: the channel would be refused for ever.
  const forwarded = [];
  const closed = {
    id: "assistant-1", session_id: "session-1", direction: "assistant",
    content: {
      type: "escalation",
      question: "Enter the verification code we sent to your phone",
      input_type: "text",
      input_schema: { sensitive: { kind: "otp", one_time: true, collection_deadline_ms: Date.now() - 1_000 } },
    },
    created_at: 3,
  };
  const fetchImpl = async (url, init = {}) => {
    if (String(url).endsWith("/api/magician/v2/chat/enroll")) {
      return jsonResponse({ enrolled: true, principal: "default" });
    }
    if (String(url).includes("/api/magician/v2/chat/active?")) {
      return jsonResponse({
        session: {
          id: "session-1", principal: "default", workspace: "default", agent_id: "personal-assistant",
          title: null, origin_channel: { channel_type: "telegram", address: "chat-42" },
          status: "active", created_at: 1, updated_at: 1,
        },
        messages: [closed],
      });
    }
    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/queue?")) {
      return jsonResponse({ queued: [], session_id: "session-1" });
    }
    if (String(url).includes("/api/magician/v2/chat/sessions/session-1/messages?")) {
      forwarded.push(JSON.parse(init.body).text);
      return jsonResponse({
        user_message: { id: "user-1", session_id: "session-1", direction: "user", content: { type: "text", text: "x" }, created_at: 4 },
        assistant_message: { id: "assistant-2", session_id: "session-1", direction: "assistant", content: { type: "text", text: "Noted." }, created_at: 5 },
      });
    }
    throw new Error(`Unexpected request: ${String(url)}`);
  };
  const adapter = new FakeAdapter();
  const runtime = new ChannelRuntime({
    channelType: "telegram",
    adapter,
    magician: new MagicianClient({ baseUrl: "http://localhost:3002", fetchImpl }),
  });
  await runtime.start();
  await adapter.handlers.onTextMessage({
    target: "tg:chat-42", channelAddress: "chat-42", text: "what now?", displayName: "Alex",
  });
  assert.deepEqual(forwarded, ["what now?"], "the closed ask holds nothing");
  assert.equal(adapter.sent[0].text, "Noted.");
});


// P5: critical-request alerts are claimed, sent value-free, reported, retired.
class FakeRealtimeMagician {
  constructor(options = {}) {
    this.claims = [];
    this.reports = [];
    this.connections = [];
    this.claimResponse = options.claimResponse ?? ((deliveryId) => ({
      delivery_id: deliveryId,
      correlation_id: "req-1",
      kind: "request",
      channel_type: "telegram",
      address: "777001",
      alert: {
        kind: "request",
        service_alias: "accounts.example.test",
        reason: "a verification code",
        text: "Magician needs a verification code for accounts.example.test to continue. Open the secure request: https://m.example.test/attention?attention=1&attention_item=req-1 Don't reply with the value here.",
        open_url: "https://m.example.test/attention?attention=1&attention_item=req-1",
      },
    }));
  }
  async resolveBearerScope() { return { workspace: "default" }; }
  async enroll() { throw new Error("unused"); }
  async getActiveSession() { throw new Error("unused"); }
  async newSession() { throw new Error("unused"); }
  async sendMessage() { throw new Error("unused"); }
  async cancelChatRun() { throw new Error("unused"); }
  async listQueuedMessages() { return { queued: [], session_id: "" }; }
  async deleteQueuedMessage() { throw new Error("unused"); }
  async clearQueuedMessages() { throw new Error("unused"); }
  connectRealtime(listener) {
    const connection = {
      listener,
      closeListeners: [],
      closed: false,
      close() { this.closed = true; },
      onClose(fn) { this.closeListeners.push(fn); },
      drop() { this.closed = true; for (const fn of this.closeListeners) fn(); },
    };
    this.connections.push(connection);
    return connection;
  }
  async claimCriticalDelivery(deliveryId, channelType, connectionGeneration) {
    this.claims.push({ deliveryId, channelType, connectionGeneration });
    const response = this.claimResponse(deliveryId);
    if (response instanceof Error) throw response;
    return response;
  }
  async reportCriticalDelivery(deliveryId, report) {
    this.reports.push({ deliveryId, ...report });
  }
}

class AlertAdapter extends FakeAdapter {
  constructor(options = {}) {
    super();
    this.alerts = [];
    this.retired = [];
    this.failSend = options.failSend ?? false;
    this.resolveTo = options.resolveTo ?? ((address) => `tg:${address}`);
  }
  resolveRealtimeTarget(address) { return this.resolveTo(address); }
  async sendCriticalAlert(target, alert) {
    if (this.failSend) throw new Error("telegram: 429 Too Many Requests: retry after 3");
    this.alerts.push({ target, alert });
    return { providerMessageId: "m-77" };
  }
  async retireCriticalAlert(target, sent) { this.retired.push({ target, ...sent }); }
}

function alertEvent(deliveryId, channelType = "telegram") {
  return {
    event_type: "CriticalRequestAlert",
    data: {
      delivery_id: deliveryId, correlation_id: "req-1", channel_type: channelType, kind: "request",
      alert: { kind: "request", service_alias: "accounts.example.test", reason: "a verification code", text: "…" },
      revision: 1, principal: "default", workspace: "default", timestamp: 1,
    },
  };
}

test("ChannelRuntime claims an alert for its channel, sends the card and reports the provider's answer", async () => {
  const magician = new FakeRealtimeMagician();
  const adapter = new AlertAdapter();
  const runtime = new ChannelRuntime({ channelType: "telegram", adapter, magician, enableRealtime: true });
  await runtime.start();
  assert.equal(magician.connections.length, 1, "the feed opens at start, before any inbound message");
  const connection = magician.connections[0];
  await connection.listener(alertEvent("d-1"));
  assert.equal(magician.claims.length, 1);
  assert.equal(magician.claims[0].deliveryId, "d-1");
  assert.equal(magician.claims[0].channelType, "telegram");
  // The generation carries a per-process token, so two processes of one channel
  // never collide on a shared counter: `<token>-<generation>`.
  assert.match(magician.claims[0].connectionGeneration, /^[a-z0-9]{1,8}-1$/);
  assert.equal(adapter.alerts.length, 1);
  assert.equal(adapter.alerts[0].target, "tg:777001", "the address comes from the claim, never the event");
  assert.match(adapter.alerts[0].alert.text, /verification code/);
  assert.equal(magician.reports.length, 1);
  assert.equal(magician.reports[0].status, "provider_accepted");
  assert.equal(magician.reports[0].provider_message_id, "m-77");
  // The report echoes the generation the claim was made with: only that
  // connection may say what the provider did.
  assert.equal(magician.reports[0].connection_generation, magician.claims[0].connectionGeneration);
  // Another channel's alert is not ours.
  await connection.listener(alertEvent("d-2", "kapso"));
  assert.equal(magician.claims.length, 1);
  // The request resolved: the card we sent is retired; an unknown delivery is ignored.
  await connection.listener({ event_type: "CriticalRequestRetired", data: { delivery_id: "d-1", correlation_id: "req-1", channel_type: "telegram", outcome: "responded", timestamp: 2 } });
  await connection.listener({ event_type: "CriticalRequestRetired", data: { delivery_id: "d-9", correlation_id: "req-1", channel_type: "telegram", outcome: "responded", timestamp: 2 } });
  assert.deepEqual(adapter.retired, [{ target: "tg:777001", providerMessageId: "m-77", outcome: "responded" }]);
  await runtime.stop();
});

test("ChannelRuntime reports a failed send and sends nothing when the claim is refused", async () => {
  // A refused claim (another bot was first, or the request resolved): nothing sent, nothing reported.
  const refused = new FakeRealtimeMagician({ claimResponse: () => new Error("Backend request failed (409)") });
  const adapter = new AlertAdapter();
  const runtime = new ChannelRuntime({ channelType: "telegram", adapter, magician: refused, enableRealtime: true });
  await runtime.start();
  await refused.connections[0].listener(alertEvent("d-1"));
  assert.equal(adapter.alerts.length, 0);
  assert.equal(refused.reports.length, 0);
  await runtime.stop();
  // A provider failure is reported with a bounded reason.
  const magician = new FakeRealtimeMagician();
  const failing = new AlertAdapter({ failSend: true });
  const runtime2 = new ChannelRuntime({ channelType: "telegram", adapter: failing, magician, enableRealtime: true });
  await runtime2.start();
  await magician.connections[0].listener(alertEvent("d-2"));
  assert.equal(magician.reports.length, 1);
  assert.equal(magician.reports[0].status, "failed");
  assert.match(magician.reports[0].reason, /429/);
  // An address the adapter cannot resolve fails without a send.
  const unresolvable = new AlertAdapter({ resolveTo: () => null });
  const magician3 = new FakeRealtimeMagician();
  const runtime3 = new ChannelRuntime({ channelType: "telegram", adapter: unresolvable, magician: magician3, enableRealtime: true });
  await runtime3.start();
  await magician3.connections[0].listener(alertEvent("d-3"));
  assert.equal(unresolvable.alerts.length, 0);
  assert.equal(magician3.reports[0].status, "failed");
  assert.match(magician3.reports[0].reason, /does not resolve/);
  await runtime2.stop();
  await runtime3.stop();
});

test("ChannelRuntime falls back to sendText for an adapter without a native alert and reconnects a dropped feed", async () => {
  const magician = new FakeRealtimeMagician();
  const adapter = new FakeAdapter();
  adapter.resolveRealtimeTarget = (address) => `tg:${address}`;
  const runtime = new ChannelRuntime({ channelType: "telegram", adapter, magician, enableRealtime: true });
  await runtime.start();
  await magician.connections[0].listener(alertEvent("d-1"));
  assert.equal(adapter.sent.length, 1);
  assert.match(adapter.sent[0].text, /Open the secure request/);
  assert.deepEqual(adapter.sent[0].options, { allowOutsideWindowTemplate: true });
  assert.equal(magician.reports.length, 1);
  assert.equal(magician.reports[0].status, "provider_accepted");
  assert.equal(magician.reports[0].connection_generation, magician.claims[0].connectionGeneration);
  // The socket drops: a new connection generation opens and later claims carry it.
  magician.connections[0].drop();
  await new Promise((resolve) => setTimeout(resolve, 1_100));
  assert.equal(magician.connections.length, 2, "the runtime reconnected on its own");
  await magician.connections[1].listener(alertEvent("d-2"));
  assert.match(magician.claims[1].connectionGeneration, /^[a-z0-9]{1,8}-2$/);
  assert.equal(
    magician.claims[1].connectionGeneration.split("-")[0],
    magician.claims[0].connectionGeneration.split("-")[0],
    "one process keeps its token across a reconnect; only the counter moves",
  );
  await runtime.stop();
});

test("ChannelRuntime retires a card it sent when the report says the request already closed", async () => {
  const magician = new FakeRealtimeMagician();
  magician.reportCriticalDelivery = async (deliveryId, report) => {
    magician.reports.push({ deliveryId, ...report });
    const error = new Error("Backend request failed (409)");
    error.status = 409;
    throw error;
  };
  const adapter = new AlertAdapter();
  const runtime = new ChannelRuntime({ channelType: "telegram", adapter, magician, enableRealtime: true });
  await runtime.start();
  await magician.connections[0].listener(alertEvent("d-1"));
  assert.equal(adapter.alerts.length, 1, "the card was sent before the refusal");
  assert.deepEqual(adapter.retired, [{ target: "tg:777001", providerMessageId: "m-77", outcome: "superseded" }]);
  // A later retirement for the same delivery has nothing left to do.
  await magician.connections[0].listener({ event_type: "CriticalRequestRetired", data: { delivery_id: "d-1", correlation_id: "req-1", channel_type: "telegram", outcome: "responded", timestamp: 2 } });
  assert.equal(adapter.retired.length, 1);
  await runtime.stop();
});
