// Public website packaging only: shared metadata, crawler files and a readable
// architecture index. No runtime configuration or credentials enter this data.
import { readFileSync, writeFileSync, cpSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const site = JSON.parse(readFileSync(join(ROOT, 'ui/unified-ui/src/lib/landing/publicSite.json'), 'utf8'));
export const escapeHtml = value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));

export function metadataHead({ title, description, canonical, indexable = true }) {
  const image = new URL(site.image.path, canonical).href;
  const properties = {
    'og:type': 'website', 'og:site_name': site.name, 'og:locale': 'en_US',
    'og:title': title, 'og:description': description, 'og:url': canonical,
    'og:image': image, 'og:image:secure_url': image, 'og:image:type': 'image/png',
    'og:image:width': site.image.width, 'og:image:height': site.image.height, 'og:image:alt': site.image.alt
  };
  const names = {
    description,
    robots: indexable ? 'index,follow,max-image-preview:large,max-snippet:-1,max-video-preview:-1' : 'noindex,follow',
    'twitter:card': 'summary_large_image', 'twitter:title': title,
    'twitter:description': description, 'twitter:image': image, 'twitter:image:alt': site.image.alt
  };
  const schema = { '@context': 'https://schema.org', '@type': 'WebPage', url: canonical, name: title, description, inLanguage: 'en', relatedLink: [site.origin + '/', site.source] };
  return `<title>${escapeHtml(title)}</title>\n<link rel="canonical" href="${escapeHtml(canonical)}">\n` +
    Object.entries(names).map(([name, content]) => `<meta name="${name}" content="${escapeHtml(content)}">`).join('\n') + '\n' +
    Object.entries(properties).map(([property, content]) => `<meta property="${property}" content="${escapeHtml(content)}">`).join('\n') +
    `\n<script type="application/ld+json">${JSON.stringify(schema).replace(/</g, '\\u003c')}</script>`;
}

function overview(model) {
  const features = [...model.features].sort((a,b) => a.name.localeCompare(b.name));
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
${metadataHead({ title: site.explain.overviewTitle, description: site.explain.description, canonical: site.explain.origin + '/overview' })}
<link rel="icon" href="/favicon.svg" type="image/svg+xml">
<style>html{color-scheme:light dark}body{margin:auto;max-width:52rem;padding:2rem 1.3rem 4rem;font:1.05rem/1.7 system-ui,sans-serif}nav{display:flex;flex-wrap:wrap;gap:1.2rem}a{color:inherit}h1{font-size:clamp(2rem,6vw,3.2rem);line-height:1.15}h2{margin-top:2.5rem}h3{margin-bottom:.2rem}li p{margin:.2rem 0}small{opacity:.75}img{max-width:100%}</style></head><body>
<nav aria-label="Overview navigation"><a href="${site.origin}/">Magican website</a><a href="/">Interactive model</a><a href="${site.source}">Public source on GitHub</a></nav>
<main><h1>How Magican works</h1>
<p>Magican is a personal AI for work, play and side quests, powered by the open-source Magician platform. You choose which models handle your work, what context they can use, and which actions need your review.</p>
<p>The interactive model opens the system level by level: connected devices, server processes, Rust crates, modules, files and types. Its diagrams and counts are generated from the code. A narrated tour follows requests through the system.</p>
<h2>The architecture</h2><ul>
<li><strong>Magician:</strong> the server coordinates chat, Work, agents, memory and tool execution.</li>
<li><strong>Magicutor:</strong> the browser sidecar connects browser sessions and the extension bridge.</li>
<li><strong>Magic Supervisor:</strong> starts and health-checks the runtime services.</li>
<li><strong>Clients:</strong> the web, desktop and phone apps connect to the server. The model also shows optional device clients.</li>
<li><strong>Models and skills:</strong> model routing and tool permissions determine which providers and capabilities an operation can use.</li>
</ul>
<p><a href="/?tour">Open the narrated architecture tour</a> or <a href="${site.origin}/manifesto">read the principles behind Magican</a>.</p>
<h2>Feature directory</h2><p>This is the same feature catalog as the interactive model, including its recorded maturity labels. A listed feature's maturity describes its current scope; inclusion is not a claim that every integration is complete.</p>
<ul>${features.map(f => `<li id="${escapeHtml(f.id)}"><h3>${escapeHtml(f.name)}</h3><p>${escapeHtml(f.oneLine || '')}</p><small>Status: ${escapeHtml(f.maturity || 'not specified')}</small></li>`).join('\n')}</ul>
</main><footer><a href="${site.origin}/privacy">Privacy</a> · <a href="${site.origin}/terms">Terms</a></footer></body></html>`;
}

export function writePublicAssets(output, kind) {
  const explain = kind === 'explain';
  if (!explain && kind !== 'marketing') throw new Error('Expected marketing or explain');
  const origin = explain ? site.explain.origin : site.origin;
  const paths = explain ? ['/', '/overview'] : Object.keys(site.pages);
  writeFileSync(join(output, 'robots.txt'), `# Public pages are accessible to search and user-directed AI retrieval.\nUser-agent: *\nAllow: /\n\nSitemap: ${origin}/sitemap.xml\n`);
  writeFileSync(join(output, 'sitemap.xml'), `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${paths.map(path => `  <url><loc>${escapeHtml(origin + path)}</loc></url>`).join('\n')}\n</urlset>\n`);
  writeFileSync(join(output, 'llms.txt'), `# Magican\n\n> Personal AI for work, play and side quests, powered by the open-source Magician platform.\n\nChoose your models, connect your apps, and decide which actions need review. Product access and setup depend on your environment and connected services.\n\n## Product and architecture\n- [Magican overview](${site.origin}/): product, examples and setup requests.\n- [Architecture in plain text](${site.explain.origin}/overview): processes, clients and the feature catalog with maturity labels.\n- [Interactive model](${site.explain.origin}/): code-derived 3D architecture and narrated tours.\n- [Manifesto](${site.origin}/manifesto): personal intelligence under your control.\n\n## Source and policies\n- [Magician public source](${site.source}): open-source platform behind Magican.\n- [Privacy](${site.origin}/privacy): storage, providers and controls.\n- [Terms](${site.origin}/terms): responsibilities and limitations.\n`);
  if (explain) {
    const html = readFileSync(join(output, 'index.html'), 'utf8');
    const marker = '<script id="data" type="application/json">';
    const start = html.indexOf(marker);
    if (start < 0) throw new Error('Missing explorer model data');
    const jsonStart = start + marker.length;
    const model = JSON.parse(html.slice(jsonStart, html.indexOf('</script>', jsonStart)));
    writeFileSync(join(output, 'overview.html'), overview(model));
    cpSync(join(ROOT, 'ui/unified-ui/static/og-image.png'), join(output, 'og-image.png'));
    cpSync(join(ROOT, 'ui/unified-ui/static/favicon.svg'), join(output, 'favicon.svg'));
    cpSync(join(ROOT, 'scripts/marketing-site-404.html'), join(output, '404.html'));
  }
}

export function markPreview(output) {
  function walk(dir) {
    for (const file of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, file.name);
      if (file.isDirectory()) walk(path);
      else if (file.name.endsWith('.html')) {
        const html = readFileSync(path, 'utf8').replace(/<meta\s+name="robots"\s+content="[^"]*"\s*\/?\s*>/g, '<meta name="robots" content="noindex,follow">');
        writeFileSync(path, html);
      }
    }
  }
  walk(output);
  const path = join(output, '_headers');
  writeFileSync(path, readFileSync(path, 'utf8') + '\n/*\n  X-Robots-Tag: noindex, follow\n');
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [output, kind, flag] = process.argv.slice(2);
  if (!output) throw new Error('Usage: public-site-assets.mjs OUTPUT marketing|explain [--preview]');
  writePublicAssets(resolve(output), kind);
  if (flag === '--preview') markPreview(resolve(output));
}
