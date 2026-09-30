<script lang="ts">
	/**
	 * CodeBlock Component — GD-F02-D
	 *
	 * Code display with optional line numbers and filename header.
	 * No syntax highlighting - uses monospace font only.
	 */

	export let code: unknown = '';
	export let language: unknown = 'plaintext';
	export let showLineNumbers: unknown = false;
	export let filename: unknown = undefined;
	export let maxHeight: unknown = undefined;

	function toString(value: unknown): string {
		return typeof value === 'string' ? value : '';
	}

	function toBoolean(value: unknown): boolean {
		return value === true || value === 'true';
	}

	function toNumber(value: unknown): number | undefined {
		if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
			return value;
		}
		return undefined;
	}

	$: safeCode = toString(code);
	$: safeLanguage = toString(language) || 'plaintext';
	$: safeShowLineNumbers = toBoolean(showLineNumbers);
	$: safeFilename = toString(filename) || undefined;
	$: safeMaxHeight = toNumber(maxHeight);

	$: codeLines = safeCode.split('\n');
	$: lineCount = codeLines.length;
</script>

<div class="muij-codeblock" class:has-filename={safeFilename}>
	{#if safeFilename}
		<div class="muij-codeblock-header">
			<span class="muij-codeblock-filename">{safeFilename}</span>
			<span class="muij-codeblock-language">{safeLanguage}</span>
		</div>
	{/if}

	<div
		class="muij-codeblock-body"
		style:max-height={safeMaxHeight ? `${safeMaxHeight}px` : undefined}
	>
		{#if safeShowLineNumbers}
			<div class="muij-codeblock-lines" aria-hidden="true">
				{#each Array(lineCount) as _, i}
					<div class="muij-codeblock-line-number">{i + 1}</div>
				{/each}
			</div>
		{/if}

		<pre class="muij-codeblock-pre"><code class="muij-codeblock-code">{safeCode}</code></pre>
	</div>
</div>

<style>
	.muij-codeblock {
		background: var(--bg-elevated);
		border-radius: var(--radius-sm);
		border: 1px solid var(--border-soft);
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		overflow: hidden;
	}

	.muij-codeblock-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: var(--space-xs) var(--space-sm);
		background: var(--bg-surface);
		border-bottom: 1px solid var(--border-soft);
	}

	.muij-codeblock-filename {
		color: var(--text-primary);
		font-size: 0.75rem;
	}

	.muij-codeblock-language {
		color: var(--text-muted);
		font-size: 0.6875rem;
		text-transform: uppercase;
	}

	.muij-codeblock-body {
		display: flex;
		overflow: auto;
		scrollbar-width: thin;
		scrollbar-color: var(--border-soft) transparent;
	}

	.muij-codeblock-body::-webkit-scrollbar {
		width: 8px;
		height: 8px;
	}

	.muij-codeblock-body::-webkit-scrollbar-track {
		background: transparent;
	}

	.muij-codeblock-body::-webkit-scrollbar-thumb {
		background: var(--border-soft);
		border-radius: 4px;
	}

	.muij-codeblock-lines {
		flex-shrink: 0;
		padding: var(--space-sm) var(--space-xs);
		text-align: right;
		user-select: none;
		border-right: 1px solid var(--border-soft);
		background: var(--bg-surface);
	}

	.muij-codeblock-line-number {
		color: var(--text-muted);
		line-height: 1.5;
	}

	.muij-codeblock-pre {
		margin: 0;
		padding: var(--space-sm);
		flex: 1;
		min-width: 0;
	}

	.muij-codeblock-code {
		color: var(--text-primary);
		white-space: pre;
		display: block;
		line-height: 1.5;
	}

	/* Magican dark theme */
	:global([data-theme="soft-machine-dark"]) .muij-codeblock {
		background: color-mix(in srgb, var(--bg-elevated) 96%, black);
		border-color: color-mix(in srgb, var(--accent-secondary) 18%, var(--border-soft));
		box-shadow: 0 14px 28px rgba(0, 0, 0, 0.24);
	}

	:global([data-theme="soft-machine-dark"]) .muij-codeblock-header {
		background: color-mix(in srgb, var(--bg-surface) 94%, black);
		border-bottom-color: color-mix(in srgb, var(--accent-secondary) 14%, var(--border-soft));
	}

	:global([data-theme="soft-machine-dark"]) .muij-codeblock-filename,
	:global([data-theme="soft-machine-dark"]) .muij-codeblock-code {
		color: color-mix(in srgb, var(--text-primary) 96%, white);
	}

	:global([data-theme="soft-machine-dark"]) .muij-codeblock-language,
	:global([data-theme="soft-machine-dark"]) .muij-codeblock-line-number {
		color: color-mix(in srgb, var(--text-primary) 56%, var(--text-muted));
	}

	:global([data-theme="soft-machine-dark"]) .muij-codeblock-lines {
		background: color-mix(in srgb, var(--bg-surface) 90%, black);
		border-right-color: color-mix(in srgb, var(--accent-secondary) 14%, var(--border-soft));
	}

	:global([data-theme="soft-machine-dark"]) .muij-codeblock-body {
		scrollbar-color: color-mix(in srgb, var(--accent-secondary) 24%, var(--border-soft)) transparent;
	}

	/* Light Theme Overrides — use CSS vars instead of hardcoded dark colors */
	:global([data-theme="soft-machine"]) .muij-codeblock,
	:global([data-theme="arcane-terminal-light"]) .muij-codeblock {
		background: var(--bg-elevated);
	}

	:global([data-theme="soft-machine"]) .muij-codeblock-header,
	:global([data-theme="arcane-terminal-light"]) .muij-codeblock-header {
		background: var(--bg-surface);
		border-bottom-color: var(--border-soft);
	}

	:global([data-theme="soft-machine"]) .muij-codeblock-filename,
	:global([data-theme="arcane-terminal-light"]) .muij-codeblock-filename {
		color: var(--text-primary);
	}

	:global([data-theme="soft-machine"]) .muij-codeblock-language,
	:global([data-theme="arcane-terminal-light"]) .muij-codeblock-language {
		color: var(--text-muted);
	}

	:global([data-theme="soft-machine"]) .muij-codeblock-code,
	:global([data-theme="arcane-terminal-light"]) .muij-codeblock-code {
		color: var(--text-primary);
	}

	:global([data-theme="soft-machine"]) .muij-codeblock-lines,
	:global([data-theme="arcane-terminal-light"]) .muij-codeblock-lines {
		background: var(--bg-surface);
		border-right-color: var(--border-soft);
	}

	:global([data-theme="soft-machine"]) .muij-codeblock-line-number,
	:global([data-theme="arcane-terminal-light"]) .muij-codeblock-line-number {
		color: var(--text-muted);
	}

	:global([data-theme="soft-machine"]) .muij-codeblock-body,
	:global([data-theme="arcane-terminal-light"]) .muij-codeblock-body {
		scrollbar-color: var(--border-soft) transparent;
	}

	/* Retro 16-bit Theme Overrides */
	:global([data-theme^="retro-16bit"]) .muij-codeblock {
		background: var(--bg-base);
		border-radius: 0;
		border: 1px solid var(--text-primary);
	}

	:global([data-theme^="retro-16bit"]) .muij-codeblock-header {
		background: var(--bg-surface);
		border-bottom: 1px dashed var(--text-primary);
	}

	:global([data-theme^="retro-16bit"]) .muij-codeblock-filename,
	:global([data-theme^="retro-16bit"]) .muij-codeblock-code {
		color: var(--text-primary);
	}

	:global([data-theme^="retro-16bit"]) .muij-codeblock-language {
		color: var(--text-muted);
	}

	:global([data-theme^="retro-16bit"]) .muij-codeblock-lines {
		background: var(--bg-surface);
		border-right: 1px dashed var(--text-primary);
	}

	:global([data-theme^="retro-16bit"]) .muij-codeblock-line-number {
		color: var(--text-muted);
	}
</style>
