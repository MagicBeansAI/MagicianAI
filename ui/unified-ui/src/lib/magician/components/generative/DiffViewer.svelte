<script lang="ts">
	/**
	 * DiffViewer Component — GD-F02-E
	 *
	 * Simple line-by-line diff comparison.
	 * No external library - uses basic line comparison.
	 */

	export let old: unknown = '';
	export let newContent: unknown = '';
	export let splitView: unknown = false;
	export let showLineNumbers: unknown = true;

	function toString(value: unknown): string {
		return typeof value === 'string' ? value : '';
	}

	function toBoolean(value: unknown): boolean {
		return value === true || value === 'true';
	}

	interface DiffLine {
		type: 'unchanged' | 'added' | 'removed';
		oldLine?: number;
		newLine?: number;
		content: string;
	}

	function computeDiff(oldContent: string, newContentStr: string): DiffLine[] {
		const oldLines = oldContent.split('\n');
		const newLines = newContentStr.split('\n');
		const result: DiffLine[] = [];

		const maxLen = Math.max(oldLines.length, newLines.length);
		let oldIdx = 0;
		let newIdx = 0;

		while (oldIdx < oldLines.length || newIdx < newLines.length) {
			if (oldIdx >= oldLines.length) {
				result.push({
					type: 'added',
					newLine: newIdx + 1,
					content: newLines[newIdx]
				});
				newIdx++;
			} else if (newIdx >= newLines.length) {
				result.push({
					type: 'removed',
					oldLine: oldIdx + 1,
					content: oldLines[oldIdx]
				});
				oldIdx++;
			} else if (oldLines[oldIdx] === newLines[newIdx]) {
				result.push({
					type: 'unchanged',
					oldLine: oldIdx + 1,
					newLine: newIdx + 1,
					content: oldLines[oldIdx]
				});
				oldIdx++;
				newIdx++;
			} else {
				const oldLineInNew = newLines.slice(newIdx).indexOf(oldLines[oldIdx]);
				const newLineInOld = oldLines.slice(oldIdx).indexOf(newLines[newIdx]);

				if (oldLineInNew === -1 && newLineInOld === -1) {
					result.push({
						type: 'removed',
						oldLine: oldIdx + 1,
						content: oldLines[oldIdx]
					});
					result.push({
						type: 'added',
						newLine: newIdx + 1,
						content: newLines[newIdx]
					});
					oldIdx++;
					newIdx++;
				} else if (oldLineInNew !== -1 && (newLineInOld === -1 || oldLineInNew <= newLineInOld)) {
					for (let i = 0; i < oldLineInNew; i++) {
						result.push({
							type: 'added',
							newLine: newIdx + i + 1,
							content: newLines[newIdx + i]
						});
					}
					newIdx += oldLineInNew;
				} else {
					for (let i = 0; i < newLineInOld; i++) {
						result.push({
							type: 'removed',
							oldLine: oldIdx + i + 1,
							content: oldLines[oldIdx + i]
						});
					}
					oldIdx += newLineInOld;
				}
			}
		}

		return result;
	}

	$: safeOld = toString(old);
	$: safeNewContent = toString(newContent);
	$: safeSplitView = toBoolean(splitView);
	$: safeShowLineNumbers = toBoolean(showLineNumbers);

	$: diffLines = computeDiff(safeOld, safeNewContent);
</script>

<div class="muij-diff" class:split-view={safeSplitView}>
	{#if safeSplitView}
		<div class="muij-diff-panel muij-diff-old">
			<div class="muij-diff-panel-header">Old</div>
			<div class="muij-diff-panel-content">
				{#each diffLines as line}
					<div
						class="muij-diff-line"
						class:muij-diff-removed={line.type === 'removed'}
						class:muij-diff-unchanged={line.type === 'unchanged'}
					>
						{#if safeShowLineNumbers}
							<span class="muij-diff-line-num">{line.oldLine ?? ''}</span>
						{/if}
						{#if line.type === 'removed'}
							<span class="muij-diff-marker">-</span>
						{:else}
							<span class="muij-diff-marker"> </span>
						{/if}
						<span class="muij-diff-content">{line.content}</span>
					</div>
				{/each}
			</div>
		</div>
		<div class="muij-diff-panel muij-diff-new">
			<div class="muij-diff-panel-header">New</div>
			<div class="muij-diff-panel-content">
				{#each diffLines as line}
					<div
						class="muij-diff-line"
						class:muij-diff-added={line.type === 'added'}
						class:muij-diff-unchanged={line.type === 'unchanged'}
					>
						{#if safeShowLineNumbers}
							<span class="muij-diff-line-num">{line.newLine ?? ''}</span>
						{/if}
						{#if line.type === 'added'}
							<span class="muij-diff-marker">+</span>
						{:else}
							<span class="muij-diff-marker"> </span>
						{/if}
						<span class="muij-diff-content">{line.content}</span>
					</div>
				{/each}
			</div>
		</div>
	{:else}
		<div class="muij-diff-unified">
			{#each diffLines as line}
				<div
					class="muij-diff-line"
					class:muij-diff-added={line.type === 'added'}
					class:muij-diff-removed={line.type === 'removed'}
					class:muij-diff-unchanged={line.type === 'unchanged'}
				>
					{#if safeShowLineNumbers}
						<span class="muij-diff-line-num-old">{line.oldLine ?? ''}</span>
						<span class="muij-diff-line-num-new">{line.newLine ?? ''}</span>
					{/if}
					{#if line.type === 'added'}
						<span class="muij-diff-marker">+</span>
					{:else if line.type === 'removed'}
						<span class="muij-diff-marker">-</span>
					{:else}
						<span class="muij-diff-marker"> </span>
					{/if}
					<span class="muij-diff-content">{line.content}</span>
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.muij-diff {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		border: 1px solid var(--border-soft);
		border-radius: var(--radius-sm);
		overflow: hidden;
		background: #fafafa;
	}

	.muij-diff.split-view {
		display: flex;
		gap: 1px;
		background: var(--border-soft);
	}

	.muij-diff-panel {
		flex: 1;
		min-width: 0;
		background: #fafafa;
	}

	.muij-diff-panel-header {
		padding: var(--space-xs) var(--space-sm);
		background: var(--bg-soft);
		border-bottom: 1px solid var(--border-soft);
		font-size: 0.75rem;
		font-weight: 600;
		color: var(--text-muted);
	}

	.muij-diff-panel-content,
	.muij-diff-unified {
		overflow: auto;
		max-height: 400px;
	}

	.muij-diff-line {
		display: flex;
		line-height: 1.5;
	}

	.muij-diff-line-num,
	.muij-diff-line-num-old,
	.muij-diff-line-num-new {
		flex-shrink: 0;
		width: 2.5rem;
		padding: 0 var(--space-xs);
		text-align: right;
		color: #999;
		background: #f0f0f0;
		user-select: none;
		border-right: 1px solid var(--border-soft);
	}

	.muij-diff-line-num-old,
	.muij-diff-line-num-new {
		width: 2rem;
	}

	.muij-diff-marker {
		flex-shrink: 0;
		width: 1rem;
		text-align: center;
		font-weight: 600;
	}

	.muij-diff-content {
		flex: 1;
		padding: 0 var(--space-sm);
		white-space: pre-wrap;
		word-break: break-all;
	}

	.muij-diff-added {
		background: #e6ffec;
	}

	.muij-diff-added .muij-diff-marker {
		color: #22863a;
	}

	.muij-diff-removed {
		background: #ffebe9;
	}

	.muij-diff-removed .muij-diff-marker {
		color: #cb2431;
	}

	.muij-diff-unchanged {
		background: transparent;
	}

	.muij-diff-unchanged .muij-diff-marker {
		color: #bbb;
	}
</style>
