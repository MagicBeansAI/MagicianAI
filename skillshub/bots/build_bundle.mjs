#!/usr/bin/env node
/**
 * Bundle a bot daemon into a dist/index.js using esbuild.
 *
 * For most bots (gmail, kapso, telegram, telegram-self) the output is
 * fully self-contained — all non-builtin imports are inlined and the
 * bot can be launched with just `dist/index.js` and an env file.
 *
 * Some bots need to keep specific deps as runtime imports because
 * those deps use bundling-hostile patterns (worker_threads with file
 * paths, `_require("../../package.json")` for self-version lookup,
 * `__dirname`-based resource resolution). Pass `--external <pkg>`
 * (repeatable) for each such dep. The bundle then becomes a thin
 * file that imports them at runtime, and `install_bot_bundles.py` is
 * responsible for ensuring those packages are installed at the scope
 * bot directory (`<scope>/bots/<name>/node_modules/`).
 *
 * Currently only `whatsapp` uses externals — `@ibrahimwithi/wu-cli`
 * (+ its transitive `pino`, `baileys`) cannot be ESM-bundled cleanly.
 * See `skillshub/bots/whatsapp/README.md` for the deployment model.
 *
 * Usage (from a bot directory like `skillshub/bots/<name>`):
 *   node ../build_bundle.mjs                         # default ESM, no externals
 *   node ../build_bundle.mjs --external <pkg>        # repeatable
 *
 * Honors --entry and --outfile if a bot needs a non-default entrypoint.
 */
import { build } from "esbuild";
import { mkdirSync } from "node:fs";
import { dirname, resolve } from "node:path";

const args = process.argv.slice(2);
function arg(flag, fallback) {
  const idx = args.indexOf(flag);
  return idx >= 0 && idx < args.length - 1 ? args[idx + 1] : fallback;
}
function argsAll(flag) {
  const out = [];
  for (let i = 0; i < args.length - 1; i += 1) {
    if (args[i] === flag) out.push(args[i + 1]);
  }
  return out;
}

const cwd = process.cwd();
const entry = resolve(cwd, arg("--entry", "src/index.ts"));
const outfile = resolve(cwd, arg("--outfile", "dist/index.js"));
const external = argsAll("--external");

mkdirSync(dirname(outfile), { recursive: true });

await build({
  entryPoints: [entry],
  outfile,
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node22",
  external,
  // Some bundled deps internally call CommonJS `require()`. With ESM
  // output, `require` isn't a global, so emit a tiny shim.
  banner: {
    js:
      "import { createRequire as __cr } from 'node:module';" +
      "const require = __cr(import.meta.url);",
  },
  logLevel: "info",
  legalComments: "none",
  metafile: false,
});
