<!--
  XmlNode — recursive renderer for a single XML node (element / text /
  cdata / comment / pi). Used by XmlViewer.
-->
<script lang="ts">
	import type { XmlNodeInfo } from './xmlTypes';

	export let node: XmlNodeInfo;
	export let depth: number = 0;

	const INITIAL_OPEN_DEPTH = 4;
	let open: boolean = depth <= INITIAL_OPEN_DEPTH;

	function isEmptyText(text: string | undefined): boolean {
		return !text || !text.trim();
	}

	function toggle(): void {
		open = !open;
	}

	function isElementOnly(children: XmlNodeInfo[] | undefined): boolean {
		if (!children) return false;
		return children.some((c) => c.kind === 'element');
	}
</script>

{#if node.kind === 'element'}
	{@const elementChildren = (node.children ?? []).filter((c) => !(c.kind === 'text' && isEmptyText(c.text)))}
	{@const onlyTextChild = elementChildren.length === 1 && elementChildren[0].kind === 'text'}
	<div class="xml-element">
		{#if elementChildren.length === 0}
			<span class="xml-tag">
				&lt;<span class="xml-element-name">{node.name}</span>{#each node.attributes ?? [] as attr (attr.name)}
					{' '}<span class="xml-attr-name">{attr.name}</span>=<span class="xml-attr-value">"{attr.value}"</span>{/each} /&gt;
			</span>
		{:else if onlyTextChild}
			<span class="xml-tag">
				&lt;<span class="xml-element-name">{node.name}</span>{#each node.attributes ?? [] as attr (attr.name)}
					{' '}<span class="xml-attr-name">{attr.name}</span>=<span class="xml-attr-value">"{attr.value}"</span>{/each}&gt;
			</span>
			<span class="xml-text-inline">{elementChildren[0].text}</span>
			<span class="xml-tag">&lt;/<span class="xml-element-name">{node.name}</span>&gt;</span>
		{:else}
			<button class="xml-toggle" type="button" on:click={toggle} aria-expanded={open}>
				{open ? '▾' : '▸'}
			</button>
			<span class="xml-tag">
				&lt;<span class="xml-element-name">{node.name}</span>{#each node.attributes ?? [] as attr (attr.name)}
					{' '}<span class="xml-attr-name">{attr.name}</span>=<span class="xml-attr-value">"{attr.value}"</span>{/each}&gt;
			</span>
			{#if !open}
				<span class="xml-summary">… {elementChildren.length} child{elementChildren.length === 1 ? '' : 'ren'}</span>
			{/if}
			{#if open}
				<div class="xml-children">
					{#each elementChildren as child, i (i)}
						<svelte:self node={child} depth={depth + 1} />
					{/each}
				</div>
			{/if}
			<span class="xml-tag">&lt;/<span class="xml-element-name">{node.name}</span>&gt;</span>
		{/if}
	</div>
{:else if node.kind === 'text'}
	{#if !isEmptyText(node.text)}
		<div class="xml-text-block">{node.text}</div>
	{/if}
{:else if node.kind === 'cdata'}
	<div class="xml-cdata">&lt;![CDATA[{node.text}]]&gt;</div>
{:else if node.kind === 'comment'}
	<div class="xml-comment">&lt;!--{node.text}--&gt;</div>
{:else if node.kind === 'pi'}
	<div class="xml-pi">&lt;?{node.text}?&gt;</div>
{/if}

<style>
	.xml-element {
		display: block;
		padding: 1px 0;
	}

	.xml-toggle {
		background: transparent;
		border: none;
		cursor: pointer;
		color: var(--theme-color-foreground-muted);
		padding: 0 4px 0 0;
		font-family: inherit;
		font-size: 0.75rem;
	}

	.xml-tag {
		color: var(--theme-color-foreground-muted);
	}

	.xml-element-name {
		color: var(--theme-color-accent);
		font-weight: 500;
	}

	.xml-attr-name {
		color: var(--theme-color-accent-alt, var(--theme-color-foreground-muted));
	}

	.xml-attr-value {
		color: var(--theme-color-foreground);
	}

	.xml-text-inline {
		color: var(--theme-color-foreground);
	}

	.xml-text-block {
		padding-left: 16px;
		color: var(--theme-color-foreground);
		white-space: pre-wrap;
	}

	.xml-children {
		padding-left: 16px;
	}

	.xml-summary {
		color: var(--theme-color-foreground-muted);
		font-style: italic;
		padding: 0 4px;
	}

	.xml-cdata,
	.xml-comment,
	.xml-pi {
		color: var(--theme-color-foreground-muted);
		font-style: italic;
		padding-left: 16px;
	}
</style>
