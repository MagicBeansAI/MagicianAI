import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { build } from "vite";
import { JSDOM } from "jsdom";

// Render the actual shipped screen. Only the native command/event boundary is
// a fixture; no backend, account, permission or runtime file is changed.
const root = fileURLToPath(new URL("../..", import.meta.url)).replace(/\/$/, "");
const bundled = await build({
  root,
  logLevel: "error",
  plugins: [{
    name: "setup-flow-test-entry",
    resolveId(id) { if (id === "virtual:setup-flow-test") return "\0setup-flow-test"; },
    load(id) {
      if (id !== "\0setup-flow-test") return;
      return `import {mount,unmount} from 'svelte';
        import Setup from ${JSON.stringify(`${root}/src/lib/Setup.svelte`)};
        const app=mount(Setup,{target:document.body,props:{onComplete:async()=>{window.setupCompleted++;}}});
        window.disposeSetup=()=>unmount(app);`;
    },
  }],
  build: {
    write: false,
    minify: false,
    rollupOptions: { input: "virtual:setup-flow-test", output: { format: "iife", inlineDynamicImports: true } },
  },
});
const screen = bundled.output.find((entry) => entry.type === "chunk").code;

async function waitFor(predicate) {
  const deadline = Date.now() + 5000;
  while (!predicate()) {
    assert.ok(Date.now() < deadline, "setup screen did not reach the expected state");
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

async function fixture(t) {
  const dom = new JSDOM("<html><body></body></html>", {
    url: "http://localhost/setup", runScripts: "outside-only", pretendToBeVisual: true,
  });
  const window = dom.window;
  Object.assign(window, { TextEncoder, TextDecoder, Response, Request, Headers, setupCompleted: 0 });
  const callbacks = new Map();
  const events = new Map();
  const calls = [];
  let sequence = 0;
  let resolveApproval;
  let rejectApproval;
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener() {} };
  window.__TAURI_INTERNALS__ = {
    transformCallback(callback) { callbacks.set(++sequence, callback); return sequence; },
    unregisterCallback(id) { callbacks.delete(id); },
    async invoke(command, args) {
      calls.push({ command, args });
      if (command === "plugin:event|listen") { events.set(args.event, args.handler); return args.handler; }
      if (command === "plugin:event|unlisten") return;
      if (command === "get_desktop_permissions") return [];
      if (command === "get_setup_options") return {
        platform: "macos", current_placement: "native_install", current_engine_url: "http://127.0.0.1:3002",
        data_root: "/fixture/notes", placements: [{ id: "native_install", label: "Install native backend",
          description: "Build from this checkout", available: true, recommended: true }],
      };
      if (command === "apply_setup_selection") return {
        next_action: "install", replace_existing_container: false, message: "Review source installation.",
        install_plan: { needs_homebrew: false, runtime_name: "native", needs_runtime_install: true,
          host_prerequisites: [], replaces_existing_container: false, summary: [
            "Build from /fixture/source checkout", `Keep data in ${args.selection.data_root}`,
            "Verify the package and start the per-user backend service",
          ] },
      };
      if (command === "approve_setup") return new Promise((resolve, reject) => {
        resolveApproval = resolve; rejectApproval = reject;
      });
      if (command === "get_magician_connection_auth") return { origin: "http://127.0.0.1:3002", token: null, revision: 0 };
      if (command === "magician_http_request") {
        assert.equal(args.request.url, "http://127.0.0.1:3002/api/magician/v2/auth/session");
        return { status: 401, headers: [], body: Array.from(new TextEncoder().encode("{}")) };
      }
      throw new Error(`Unexpected native command: ${command}`);
    },
  };
  window.eval(screen);
  t.after(async () => { resolveApproval?.(); await window.disposeSetup(); window.close(); });
  const text = () => window.document.body.textContent;
  const click = (label) => {
    const button = [...window.document.querySelectorAll("button")].find((entry) => entry.textContent.trim() === label);
    assert.ok(button, `missing button: ${label}`);
    assert.equal(button.disabled, false);
    button.click();
  };
  await waitFor(() => text().includes("Where should the backend run?"));
  return {
    window, calls, text, click,
    emit(payload) { callbacks.get(events.get("setup-progress"))({ event: "setup-progress", payload }); },
    finish() { resolveApproval(); }, fail(error) { rejectApproval(new Error(error)); },
  };
}

test("source consent preserves the chosen data folder and waits for approval", async (t) => {
  const ui = await fixture(t);
  const input = ui.window.document.querySelector("#data-root");
  input.value = "/fixture/owner notes";
  input.dispatchEvent(new ui.window.Event("input", { bubbles: true }));
  ui.click("Continue");
  await waitFor(() => ui.text().includes("Review installation"));
  assert.ok(ui.text().includes("/fixture/source checkout"));
  assert.ok(ui.text().includes("/fixture/owner notes"));
  assert.equal(ui.calls.some((call) => call.command === "approve_setup"), false);
  ui.click("Install and start");
  await waitFor(() => ui.calls.some((call) => call.command === "approve_setup"));
  assert.equal(ui.calls.find((call) => call.command === "approve_setup").args.selection.data_root, "/fixture/owner notes");
  assert.equal(ui.window.setupCompleted, 0);
});

test("failed source build shows its log and cannot advance to owner setup", async (t) => {
  const ui = await fixture(t);
  ui.click("Continue");
  await waitFor(() => ui.text().includes("Review installation"));
  ui.click("Install and start");
  await waitFor(() => ui.calls.some((call) => call.command === "approve_setup"));
  ui.emit({ step: "InstallingBackend", progress: 0.2, message: "Building; output: /fixture/notes/logs/source-install.log" });
  const error = "Source installation failed; see /fixture/notes/logs/source-install.log";
  ui.emit({ step: { Failed: error }, progress: 0, message: error });
  ui.fail(error);
  await waitFor(() => ui.text().includes(error));
  assert.equal(ui.calls.some((call) => call.command === "get_magician_connection_auth"), false);
  assert.equal(ui.window.setupCompleted, 0);
  assert.ok([...ui.window.document.querySelectorAll("button")].some((button) => /retry|try again/i.test(button.textContent)));
});

test("core readiness requires owner sign-in before capability setup can finish", async (t) => {
  const ui = await fixture(t);
  ui.click("Continue");
  await waitFor(() => ui.text().includes("Review installation"));
  ui.click("Install and start");
  await waitFor(() => ui.calls.some((call) => call.command === "approve_setup"));
  ui.emit({ step: "Ready", progress: 1, message: "Core installation verified." });
  ui.finish();
  await waitFor(() => ui.text().includes("Create the owner account or sign in to this server."));
  assert.equal(ui.window.setupCompleted, 0);
  assert.ok(ui.window.document.querySelector('input[type="password"]'));
});
