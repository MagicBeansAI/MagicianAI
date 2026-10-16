import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { supportedRecipeFixtures } from "../.recipe-build/recipe-fixtures.js";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const recipesRoot = resolve(projectRoot, "app", "recipes");
await mkdir(recipesRoot, { recursive: true });

for (const [name, bundle] of Object.entries(supportedRecipeFixtures())) {
  const destination = resolve(recipesRoot, `${name}.json`);
  if (!destination.startsWith(`${recipesRoot}/`)) {
    throw new Error(`refusing recipe path outside package: ${name}`);
  }
  await writeFile(destination, `${JSON.stringify(bundle, null, 2)}\n`, {
    encoding: "utf8",
    mode: 0o600,
  });
}
