<script lang="ts">
	export let matrix: Array<Array<number | null>> = [];
	export let rowLabels: string[] = [];
	export let colLabels: string[] = [];
	export let showValues: boolean = false;

	function toFiniteNumber(value: unknown): number | undefined {
		if (typeof value !== 'number' || !Number.isFinite(value)) return undefined;
		return value;
	}

	function toDisplayLabel(value: unknown, fallback: string): string {
		if (typeof value === 'string' && value.trim().length > 0) return value.trim();
		return fallback;
	}

	function cellColor(value: number | null, min: number, max: number): string {
		if (value === null) return 'var(--bg-soft)';
		if (max <= min) return 'hsl(204 86% 55%)';
		const ratio = (value - min) / (max - min);
		const hue = 210 - ratio * 170;
		const lightness = 92 - ratio * 42;
		return `hsl(${hue.toFixed(1)} 78% ${lightness.toFixed(1)}%)`;
	}

	function formatCell(value: number | null): string {
		if (value === null) return 'N/A';
		if (Math.abs(value) >= 1000) return value.toLocaleString();
		if (Math.abs(value) >= 10) return value.toFixed(1);
		return value.toFixed(2);
	}

	$: safeMatrix = matrix
		.filter((row): row is number[] => Array.isArray(row))
		.map((row) => row.map((cell) => {
			const numeric = toFiniteNumber(cell);
			return numeric === undefined ? null : numeric;
		}));

	$: rowCount = safeMatrix.length;
	$: colCount = Math.max(...safeMatrix.map((row) => row.length), 0);
	$: finiteValues = safeMatrix.flat().filter((cell): cell is number => cell !== null);
	$: minValue = finiteValues.length > 0 ? Math.min(...finiteValues) : 0;
	$: maxValue = finiteValues.length > 0 ? Math.max(...finiteValues) : 1;
	$: resolvedRowLabels = Array.from({ length: rowCount }, (_, idx) =>
		toDisplayLabel(rowLabels[idx], `Row ${idx + 1}`)
	);
	$: resolvedColLabels = Array.from({ length: colCount }, (_, idx) =>
		toDisplayLabel(colLabels[idx], `Col ${idx + 1}`)
	);
</script>

<div class="muij-heatmap">
	{#if rowCount === 0 || colCount === 0}
		<div class="muij-heatmap-empty">No data</div>
	{:else}
		<div class="muij-heatmap-grid-wrap">
			<div class="muij-heatmap-row-head"></div>
			<div class="muij-heatmap-col-head" style={`--cols:${colCount}`}>
				{#each resolvedColLabels as label, colLabelIndex (`${label}:${colLabelIndex}`)}
					<div class="muij-heatmap-col-label" title={label}>{label}</div>
				{/each}
			</div>

			{#each safeMatrix as row, rowIndex (rowIndex)}
				<div class="muij-heatmap-row-label" title={resolvedRowLabels[rowIndex]}>{resolvedRowLabels[rowIndex]}</div>
				<div class="muij-heatmap-row" style={`--cols:${colCount}`}>
					{#each Array.from({ length: colCount }) as _, colIndex (colIndex)}
						{@const cell = row[colIndex] ?? null}
						<div
							class="muij-heatmap-cell"
							style={`background:${cellColor(cell, minValue, maxValue)}`}
							title={`${resolvedRowLabels[rowIndex]} · ${resolvedColLabels[colIndex]}: ${formatCell(cell)}`}
						>
							{#if showValues}
								<span>{formatCell(cell)}</span>
							{/if}
						</div>
					{/each}
				</div>
			{/each}
		</div>
	{/if}
</div>

<style>
	.muij-heatmap {
		display: flex;
		flex-direction: column;
		gap: var(--space-sm);
		min-width: 0;
	}

	.muij-heatmap-empty {
		font-family: var(--font-primary);
		font-size: 0.75rem;
		color: var(--text-secondary);
		font-style: italic;
	}

	.muij-heatmap-grid-wrap {
		display: grid;
		grid-template-columns: minmax(74px, auto) minmax(0, 1fr);
		gap: 6px;
		align-items: center;
	}

	.muij-heatmap-col-head,
	.muij-heatmap-row {
		display: grid;
		grid-template-columns: repeat(var(--cols), minmax(14px, 1fr));
		gap: 4px;
	}

	.muij-heatmap-col-label,
	.muij-heatmap-row-label {
		font-family: var(--font-primary);
		font-size: 0.625rem;
		color: var(--text-secondary);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.muij-heatmap-cell {
		min-height: 22px;
		border-radius: var(--radius-xs);
		display: flex;
		align-items: center;
		justify-content: center;
		font-family: var(--font-mono);
		font-size: 0.5625rem;
		color: #0f172a;
		transition: transform 120ms ease;
	}

	.muij-heatmap-cell:hover {
		transform: scale(1.04);
	}
</style>
