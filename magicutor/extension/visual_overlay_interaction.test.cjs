const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const source = fs.readFileSync(path.join(__dirname, 'visual_overlay.js'), 'utf8');

function cssRule(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const match = source.match(new RegExp(`${escaped}\\s*\\{([^}]+)\\}`));
  assert.ok(match, `missing ${selector} rule`);
  return match[1];
}

test('the hidden automation status panel cannot intercept page clicks', () => {
  assert.match(cssRule('.status-panel'), /pointer-events:\s*none\s*;/);
  assert.match(cssRule('.status-panel.visible'), /pointer-events:\s*auto\s*;/);
});

test('the automation cursor is a small black pointer with a quiet hue', () => {
  const cursor = cssRule('.cursor');
  assert.match(cursor, /width:\s*22px/);
  assert.match(cursor, /height:\s*22px/);
  assert.match(source, /--cursor-fill:\s*#111111/);
  assert.match(source, /--cursor-stroke:\s*#ffffff/);
  assert.doesNotMatch(cssRule('.cursor svg'), /drop-shadow/);
  assert.match(cssRule('.cursor::before'), /radial-gradient/);
  assert.match(source, /Math\.sin\(t \/ 900\)/);
  assert.match(source, /Q9\.8 9\.8 9\.3 11\.2/);
  assert.match(cssRule('.cursor svg'), /stroke-width:\s*3\.4/);
  assert.doesNotMatch(source, /14\.12 22\.88/);
});
