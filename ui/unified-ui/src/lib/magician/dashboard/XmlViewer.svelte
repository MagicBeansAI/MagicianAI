<!--
  XmlViewer — collapsible XML tree for `text/xml` / `application/xml`
  published surfaces. Parses via the browser's DOMParser, delegates the
  actual rendering to XmlNode (recursive).
-->
<script lang="ts">
	import XmlNode from './XmlNode.svelte';
	import type { XmlNodeInfo } from './xmlTypes';

	export let xml: string = '';

	let parsed: XmlNodeInfo | null = null;
	let parseError: string | null = null;

	function parse(source: string): { root: XmlNodeInfo | null; error: string | null } {
		if (!source.trim()) return { root: null, error: null };
		const doc = new DOMParser().parseFromString(source, 'application/xml');
		const errorNode = doc.querySelector('parsererror');
		if (errorNode) {
			return { root: null, error: errorNode.textContent?.trim() || 'XML parse error' };
		}
		return { root: convert(doc.documentElement), error: null };
	}

	function convert(node: Node): XmlNodeInfo {
		if (node.nodeType === Node.ELEMENT_NODE) {
			const el = node as Element;
			return {
				kind: 'element',
				name: el.nodeName,
				attributes: Array.from(el.attributes).map((a) => ({ name: a.name, value: a.value })),
				children: Array.from(el.childNodes).map(convert)
			};
		}
		if (node.nodeType === Node.TEXT_NODE) {
			return { kind: 'text', text: node.textContent ?? '' };
		}
		if (node.nodeType === Node.CDATA_SECTION_NODE) {
			return { kind: 'cdata', text: node.textContent ?? '' };
		}
		if (node.nodeType === Node.COMMENT_NODE) {
			return { kind: 'comment', text: node.textContent ?? '' };
		}
		if (node.nodeType === Node.PROCESSING_INSTRUCTION_NODE) {
			return { kind: 'pi', text: node.textContent ?? '' };
		}
		return { kind: 'text', text: '' };
	}

	$: {
		const result = parse(xml);
		parsed = result.root;
		parseError = result.error;
	}
</script>

{#if parseError}
	<div class="xml-error">XML parse error: {parseError}</div>
{:else if parsed}
	<div class="xml-tree">
		<XmlNode node={parsed} depth={0} />
	</div>
{:else}
	<div class="xml-empty">Empty XML document.</div>
{/if}

<style>
	.xml-tree {
		font-family: var(--theme-font-mono, ui-monospace, monospace);
		font-size: 0.875rem;
		line-height: 1.5;
		color: var(--theme-color-foreground);
		background-color: var(--theme-color-surface, #fff);
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.06));
		border-radius: 8px;
		padding: 16px 18px;
		overflow-x: auto;
	}

	.xml-error {
		font-family: var(--theme-font-mono);
		color: var(--theme-color-accent);
		padding: 14px;
		border: 1px solid var(--theme-color-accent);
		border-radius: 6px;
		background-color: var(--theme-color-surface);
	}

	.xml-empty {
		padding: 24px;
		text-align: center;
		font-style: italic;
		color: var(--theme-color-foreground-muted);
	}
</style>
