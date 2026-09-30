import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import test from "node:test";

const css = readFileSync(new URL("./styles.css", import.meta.url), "utf8");
const themeStore = readFileSync(
  new URL("../../ui/unified-ui/src/lib/shared/stores/themeStore.ts", import.meta.url),
  "utf8",
);
const webThemeCss = readFileSync(
  new URL("../../ui/unified-ui/src/app.css", import.meta.url),
  "utf8",
);
const androidThemeSource = readFileSync(
  new URL("../../magdroid/android/app/src/main/kotlin/ai/magicbeans/magdroid/ui/ThemeTypography.kt", import.meta.url),
  "utf8",
);
const iosThemeSource = readFileSync(
  new URL("../../magios/Magios/ThemeManager.swift", import.meta.url),
  "utf8",
);
const families = new Map([
  ["Outfit", "Outfit.ttf"], ["Manrope", "Manrope.ttf"], ["Geist Mono", "GeistMono.ttf"],
  ["Quicksand", "Quicksand.ttf"], ["Fredoka", "Fredoka.ttf"],
  ["JetBrains Mono", "JetBrainsMono.ttf"], ["Space Grotesk", "SpaceGrotesk.ttf"],
  ["IBM Plex Mono", "IBMPlexMono-Regular.ttf"], ["Fira Code", "FiraCode.ttf"],
  ["Press Start 2P", "PressStart2P-Regular.ttf"], ["Pixelify Sans", "PixelifySans.ttf"],
  ["Bricolage Grotesque", "BricolageGrotesque.ttf"],
  ["Special Elite", "SpecialElite-Regular.ttf"],
  ["Permanent Marker", "PermanentMarker-Regular.ttf"], ["Inter", "Inter.ttf"],
  ["Lilita One", "LilitaOne-Regular.ttf"], ["Rajdhani", "Rajdhani-Regular.ttf"],
]);

test("Tauri bundles every Web theme font and applies all four roles", () => {
  for (const [family, filename] of families) {
    assert.match(css, new RegExp(`font-family: [\\"']${family.replaceAll(" ", "\\s")}[\\"']`));
    assert.equal(existsSync(new URL(`../public/fonts/${filename}`, import.meta.url)), true, filename);
  }
  assert.match(css, /body\s*\{[^}]*var\(--font-body-local\)/s);
  assert.match(css, /h1, h2, h3, h4, h5, h6\s*\{[^}]*var\(--font-display-local\)/s);
  assert.match(css, /code, pre, kbd, samp\s*\{[^}]*var\(--font-mono-local\)/s);
  assert.match(css, /--font-brand-local:\s*var\(--font-brand, "Outfit"/);
  for (const token of ["--font-primary", "--font-display", "--font-brand", "--font-mono"]) {
    assert.match(themeStore, new RegExp(`['\"]${token}['\"]`));
  }
});

function webThemeRoles() {
  const roles = new Map();
  const themeBlock = /\[data-theme="([^"]+)"\]\s*\{([^}]*)\}/gs;
  for (const match of webThemeCss.matchAll(themeBlock)) {
    const values = Object.fromEntries(
      [...match[2].matchAll(/--font-(primary|display|brand|mono):\s*(['"])(.*?)\2/g)]
        .map((entry) => [entry[1], entry[3]]),
    );
    if (!values.primary || !values.display || !values.mono) continue;
    roles.set(match[1], {
      brand: values.brand ?? "Outfit",
      display: values.display,
      body: values.primary,
      mono: values.mono,
    });
  }
  return roles;
}

function androidThemeRoles() {
  return new Map([...androidThemeSource.matchAll(
    /"([^"]+)" to ThemeFontNames\("([^"]+)", "([^"]+)", "([^"]+)", "([^"]+)"\)/g,
  )].map((match) => [match[1], {
    brand: match[2], display: match[3], body: match[4], mono: match[5],
  }]));
}

function iosThemeRoles() {
  return new Map([...iosThemeSource.matchAll(
    /"([^"]+)": \.init\(brand: "([^"]+)", display: "([^"]+)", body: "([^"]+)", mono: "([^"]+)"\)/g,
  )].map((match) => [match[1], {
    brand: match[2], display: match[3], body: match[4], mono: match[5],
  }]));
}

test("Android and iOS mirror every Web theme font role", () => {
  const web = [...webThemeRoles()].sort();
  assert.equal(web.length, 22);
  assert.deepEqual([...androidThemeRoles()].sort(), web);
  assert.deepEqual([...iosThemeRoles()].sort(), web);
});
