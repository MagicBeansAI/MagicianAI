import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { metadataHead, writePublicAssets, markPreview, site } from './public-site-assets.mjs';

function fixture(run) {
  const dir = mkdtempSync(join(tmpdir(), 'public-site-fixture-'));
  try { run(dir); } finally { rmSync(dir, { recursive: true, force: true }); }
}

test('social metadata escapes text, uses absolute images and safe JSON-LD', () => {
  const head = metadataHead({ title: 'A <model> & "tools"', description: '</script><script>unsafe</script>', canonical: site.explain.origin + '/' });
  assert.match(head, /og:image" content="https:\/\/explain\.magican\.ai\/og-image\.png/);
  assert.match(head, /A &lt;model&gt; &amp; &quot;tools&quot;/);
  const schema = JSON.parse(head.match(/<script type="application\/ld\+json">(.*?)<\/script>/s)[1]);
  assert.equal(schema.description, '</script><script>unsafe</script>');
  assert.equal((head.match(/<script/g) || []).length, 1);
});

test('production crawler files list only canonical public pages', () => fixture(dir => {
  writePublicAssets(dir, 'marketing');
  const sitemap = readFileSync(join(dir, 'sitemap.xml'), 'utf8');
  assert.equal((sitemap.match(/<loc>/g) || []).length, 4);
  assert.ok(sitemap.includes(site.origin + '/manifesto'));
  assert.ok(!sitemap.includes('/work'));
  assert.match(readFileSync(join(dir, 'robots.txt'), 'utf8'), /User-agent: \*\nAllow: \//);
  assert.match(readFileSync(join(dir, 'llms.txt'), 'utf8'), /Architecture in plain text/);
}));

test('preview responses are noindex while retaining production canonicals and cache rules', () => fixture(dir => {
  writeFileSync(join(dir, 'index.html'), metadataHead({ title: 'Preview', description: 'Preview', canonical: site.origin + '/' }));
  writeFileSync(join(dir, '_headers'), '/_app/immutable/*\n  Cache-Control: public, max-age=31536000, immutable\n');
  markPreview(dir);
  const html = readFileSync(join(dir, 'index.html'), 'utf8');
  assert.match(html, /name="robots" content="noindex,follow"/);
  assert.ok(html.includes(`rel="canonical" href="${site.origin}/"`));
  assert.match(readFileSync(join(dir, '_headers'), 'utf8'), /X-Robots-Tag: noindex, follow/);
  assert.match(readFileSync(join(dir, '_headers'), 'utf8'), /Cache-Control: public/);
}));

test('text overview preserves feature maturity and escapes catalog content', () => fixture(dir => {
  const model = { features: [{ id: 'example', name: 'Example <feature>', oneLine: 'A & B', maturity: 'partial' }] };
  writeFileSync(join(dir, 'index.html'), `<script id="data" type="application/json">${JSON.stringify(model)}</script>`);
  writePublicAssets(dir, 'explain');
  const html = readFileSync(join(dir, 'overview.html'), 'utf8');
  assert.match(html, /Example &lt;feature&gt;/);
  assert.match(html, /A &amp; B/);
  assert.match(html, /Status: partial/);
  assert.match(html, /<h1>How Magican works<\/h1>/);
  assert.ok(html.includes(`rel="canonical" href="${site.explain.origin}/overview"`));
  assert.ok(readFileSync(join(dir, 'sitemap.xml'), 'utf8').includes(`${site.explain.origin}/overview</loc>`));
}));
