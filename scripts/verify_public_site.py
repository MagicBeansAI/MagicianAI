#!/usr/bin/env python3
"""Check shipped public HTML and crawler assets before a Pages deployment."""

import argparse
import json
import struct
import xml.etree.ElementTree as ET
from html.parser import HTMLParser
from pathlib import Path


class Page(HTMLParser):
    def __init__(self, text):
        super().__init__(convert_charrefs=True)
        self.meta = {}
        self.canon = []
        self.titles = []
        self.schemas = []
        self.headings = []
        self.body = []
        self.in_head = self.in_body = self.in_title = self.in_h1 = False
        self.in_schema = False
        self.skip = 0
        self.schema_text = ''
        self.feed(text)

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == 'head':
            self.in_head = True
        if tag == 'body':
            self.in_body = True
        if tag == 'h1':
            self.in_h1 = True
        if tag == 'meta':
            key = attrs.get('name', attrs.get('property'))
            self.meta.setdefault(key, []).append(attrs.get('content', ''))
        if tag == 'link' and attrs.get('rel') == 'canonical':
            self.canon.append(attrs.get('href'))
        if tag == 'title' and self.in_head:
            self.in_title = True
            self.titles.append('')
        if tag in ('script', 'style'):
            self.skip += 1
            if tag == 'script' and attrs.get('type') == 'application/ld+json':
                self.in_schema = True
                self.schema_text = ''

    def handle_endtag(self, tag):
        if tag == 'title':
            self.in_title = False
        if tag == 'head':
            self.in_head = False
        if tag == 'body':
            self.in_body = False
        if tag == 'h1':
            self.in_h1 = False
        if tag in ('script', 'style'):
            self.skip -= 1
            if self.in_schema:
                self.schemas.append(json.loads(self.schema_text))
                self.in_schema = False

    def handle_data(self, text):
        if self.in_title:
            self.titles[-1] += text
        if self.in_h1:
            self.headings.append(text)
        if self.in_schema:
            self.schema_text += text
        if self.in_body and self.skip == 0:
            self.body.append(text)


def check(root, origin, paths, preview=False):
    for path in paths:
        relative = 'index.html' if path == '/' else path.lstrip('/')
        if not relative.endswith('.html'):
            relative += '.html'
        file = root / relative
        page = Page(file.read_text())
        assert len(page.titles) == 1, (file, 'duplicate/missing title', page.titles)
        for key in (
            'description', 'robots', 'og:title', 'og:description', 'og:url',
            'og:image', 'og:image:alt', 'og:image:width', 'og:image:height',
            'twitter:card', 'twitter:title', 'twitter:description', 'twitter:image',
        ):
            assert len(page.meta.get(key, [])) == 1, (file, 'duplicate/missing meta', key)
        assert page.canon == [origin + path], (file, page.canon)
        assert page.meta['og:url'] == page.canon
        assert page.meta['og:title'] == page.meta['twitter:title'] == page.titles
        assert page.meta['description'] == page.meta['og:description'] == page.meta['twitter:description']
        assert page.meta['og:image'] == page.meta['twitter:image'] == [origin + '/og-image.png']
        expected_robots = 'noindex,' if preview else 'index,'
        assert page.meta['robots'][0].startswith(expected_robots), (file, page.meta['robots'])
        assert page.schemas and all(s['@context'] == 'https://schema.org' for s in page.schemas)
        assert any(text.strip() for text in page.headings), (file, 'missing crawlable heading/content')
        dimensions = (int(page.meta['og:image:width'][0]), int(page.meta['og:image:height'][0]))
        assert struct.unpack('>II', (root / 'og-image.png').read_bytes()[16:24]) == dimensions
        print(json.dumps({'file': str(file), 'title': page.titles[0], 'canonical': page.canon[0],
                          'body_chars': len(' '.join(page.body))}))
    for asset in ('robots.txt', 'sitemap.xml', 'llms.txt', 'og-image.png', '404.html'):
        assert (root / asset).exists(), (root, asset)
    assert 'noindex' in (root / '404.html').read_text()
    assert 'Allow: /' in (root / 'robots.txt').read_text()
    sitemap = ET.parse(root / 'sitemap.xml')
    locations = {node.text for node in sitemap.findall('.//{http://www.sitemaps.org/schemas/sitemap/0.9}loc')}
    assert locations == {origin + path for path in paths}, ('unexpected sitemap URLs', locations)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('kind', choices=['marketing', 'explain'])
    parser.add_argument('--preview', action='store_true')
    args = parser.parse_args()
    config_path = Path(__file__).resolve().parent.parent / 'ui/unified-ui/src/lib/landing/publicSite.json'
    config = json.loads(config_path.read_text())
    marketing = args.kind == 'marketing'
    origin = config['origin'] if marketing else config['explain']['origin']
    paths = list(config['pages']) if marketing else ['/', '/overview']
    check(args.output, origin, paths, args.preview)
    if marketing:
        assert not (args.output / '200.html').exists(), 'SPA fallback must not be published as marketing HTML'
    if args.preview:
        assert 'X-Robots-Tag: noindex, follow' in (args.output / '_headers').read_text()
