import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { fileURLToPath } from "node:url";

const svelteClientRuntime = fileURLToPath(
  new URL("./node_modules/svelte/src/index-client.js", import.meta.url),
);

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  resolve: {
    alias: [
      // Tauri renders only in the browser webview. Pin the bare Svelte
      // runtime import to the client entry so Vite does not inspect the
      // server renderer and warn about Node-only modules during builds.
      { find: /^svelte$/, replacement: svelteClientRuntime },
    ],
  },
  server: {
    port: 5173,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2021",
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});
