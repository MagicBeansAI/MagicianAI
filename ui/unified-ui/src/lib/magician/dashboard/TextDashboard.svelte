<!--
  TextDashboard — renders `text/plain` task user-output.

  Auto-detects whether the content reads as prose or code:
    code  → mono <pre>, theme surface, generous padding
    prose → chrome typography, paragraph spacing, max-width 70ch

  Heuristic: if more than ~15% of non-empty lines start with whitespace
  OR if more than ~25% of characters are in `[{};()=<>]`, treat as code.
  Both checks together catch shell snippets, JSON dumps, log lines.
-->
<script lang="ts">
	export let text: string = '';

	function isCode(input: string): boolean {
		if (!input.trim()) return false;
		const lines = input.split(/\r?\n/).filter((l) => l.length > 0);
		if (lines.length === 0) return false;

		let indented = 0;
		for (const line of lines) {
			if (/^\s/.test(line)) indented += 1;
		}
		const indentRatio = indented / lines.length;

		const codeChars = (input.match(/[{};()=<>[\]]/g) ?? []).length;
		const codeRatio = codeChars / input.length;

		return indentRatio > 0.15 || codeRatio > 0.025;
	}

	$: codeLike = isCode(text);
</script>

{#if codeLike}
	<pre class="text-code">{text}</pre>
{:else}
	<div class="text-prose">
		{#each text.split(/\n\n+/) as paragraph (paragraph)}
			<p>{paragraph}</p>
		{/each}
	</div>
{/if}

<style>
	.text-code {
		font-family: var(--theme-font-mono, ui-monospace, SFMono-Regular, monospace);
		font-size: 0.875rem;
		line-height: 1.55;
		background-color: var(--theme-color-surface, #fff);
		border: 1px solid var(--theme-color-border, rgba(0, 0, 0, 0.06));
		border-radius: 8px;
		padding: 18px 22px;
		overflow-x: auto;
		color: var(--theme-color-foreground);
		margin: 0;
		white-space: pre;
		box-shadow: 0 1px 3px var(--theme-color-shadow, rgba(0, 0, 0, 0.04));
	}

	.text-prose {
		font-family: var(--theme-font-body, system-ui);
		color: var(--theme-color-foreground);
		line-height: var(--theme-prose-line-height, 1.65);
		max-width: 70ch;
	}

	.text-prose p {
		margin: 0 0 1em;
		white-space: pre-wrap;
	}

	.text-prose p:last-child {
		margin-bottom: 0;
	}
</style>
