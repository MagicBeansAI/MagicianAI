<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { Slider } from 'bits-ui';
	import { thinkingLabel, type ModelChoice } from './modelCatalog';

	export let control: ModelChoice['thinking'];
	export let value = 'auto';
	export let disabled = false;
	const dispatch = createEventDispatcher<{ change: string }>();
	let preview: string | null = null;
	$: displayedValue = preview ?? value;
	$: levels = control.levels;
	$: index = Math.max(0, levels.indexOf(displayedValue === 'auto' ? control.default ?? '' : displayedValue));
	$: binary = control.mode === 'toggle';
	$: enabled = value !== 'none' && (value !== 'auto' || control.default !== 'none');
	$: unresolved = displayedValue === 'auto' && !control.default;
	$: selectedLabel = unresolved ? 'Engine default' : thinkingLabel(levels[index]);
	function commit(next: number) {
		if (disabled) return;
		preview = null;
		dispatch('change', levels[next]);
	}
</script>

{#if control.mode !== 'none' && levels.length > 0}
	<fieldset class="thinking-control" {disabled}>
		<legend>Thinking</legend>
		<div class="thinking-heading">
			<button type="button" class:active={displayedValue === 'auto'} aria-pressed={displayedValue === 'auto'}
				on:click={() => { preview = null; dispatch('change', 'auto'); }}>Auto</button>
			<span class="thinking-value" aria-live="polite">{displayedValue === 'auto' ? (control.default ? `Default · ${thinkingLabel(control.default)}` : 'Engine default') : thinkingLabel(displayedValue)}</span>
		</div>
		{#if binary}
			<button class="thinking-switch" type="button" role="switch" aria-checked={enabled}
				aria-label="Enable thinking" on:click={() => dispatch('change', enabled ? 'none' : 'on')}>
				<span class:enabled></span>{enabled ? 'On' : 'Off'}
			</button>
		{:else if levels.length > 1}
			<div class="thinking-slider-wrap">
				<Slider.Root type="single" min={0} max={levels.length - 1} step={1} value={index}
					{disabled} thumbPositioning="exact" class={`thinking-slider${unresolved ? ' is-unresolved' : ''}`}
					onValueChange={(next) => preview = levels[next]} onValueCommit={commit}>
					<span class="thinking-track" aria-hidden="true">
						<Slider.Range class="thinking-fill" />
					</span>
					{#each levels as level, stepIndex}
						<span class="thinking-tick" class:passed={!unresolved && stepIndex <= index}
							style:left={`${stepIndex / (levels.length - 1) * 100}%`} aria-hidden="true"></span>
					{/each}
					<Slider.Thumb index={0} class="thinking-thumb" aria-label="Thinking level"
						aria-valuetext={unresolved ? 'Auto · engine default' : selectedLabel} onblur={() => preview = null}>
						<span class="thinking-grip" aria-hidden="true"></span>
						<span class="thinking-tooltip" class:at-start={index === 0} class:at-end={index === levels.length - 1}
							aria-hidden="true">{selectedLabel}</span>
					</Slider.Thumb>
				</Slider.Root>
			</div>
			<div class="thinking-steps">
				{#each levels as level, stepIndex}
					<button type="button" class:current={!unresolved && stepIndex === index}
						aria-label={`Set thinking to ${thinkingLabel(level)}`} aria-pressed={!unresolved && stepIndex === index}
						on:click={() => commit(stepIndex)}>{thinkingLabel(level)}</button>
				{/each}
			</div>
		{/if}
		<p>Higher thinking can improve difficult answers and take more time.</p>
	</fieldset>
{/if}

<style>
	.thinking-control { border: 0; padding: 0; margin: 0; min-width: 0; color: var(--text-primary); }
	legend { font-size: .7rem; font-weight: 600; margin-bottom: .3rem; }
	.thinking-heading { display: flex; align-items: center; justify-content: space-between; gap: .5rem; margin-bottom: .2rem; }
	button { font: inherit; color: inherit; cursor: pointer; }
	.thinking-heading button { border: 1px solid var(--border-soft); background: var(--bg-soft); border-radius: 4px; padding: .2rem .6rem; font-size: .7rem; }
	.thinking-heading button.active { background: var(--accent-fill); color: var(--text-on-accent); border-color: transparent; }
	.thinking-value { font-size: .7rem; color: var(--text-secondary); }
	.thinking-slider-wrap { padding: 2px 11px 0; }
	.thinking-control :global(.thinking-slider) { position: relative; display: flex; align-items: center; width: 100%; height: 36px; touch-action: none; user-select: none; cursor: pointer; }
	.thinking-track { position: relative; display: block; width: 100%; height: 8px; overflow: hidden; border: 1px solid var(--border-soft); border-radius: 3px; background: var(--bg-soft); box-sizing: border-box; }
	.thinking-control :global(.thinking-fill) { position: absolute; height: 100%; background: linear-gradient(90deg, var(--accent-fill), var(--accent-primary)); border-radius: inherit; transition: width 120ms ease; }
	.thinking-tick { position: absolute; top: 16px; width: 2px; height: 4px; transform: translateX(-50%); border-radius: 1px; background: var(--text-faint); pointer-events: none; }
	.thinking-tick.passed { background: var(--text-on-accent); opacity: .75; }
	.thinking-control :global(.thinking-thumb) { display: flex; align-items: center; justify-content: center; width: 20px; height: 24px; box-sizing: border-box; border: 1px solid var(--accent-primary); border-radius: 6px; background: linear-gradient(160deg, var(--bg-elevated), var(--accent-soft)); box-shadow: var(--shadow-sm), inset 0 1px 0 var(--bg-elevated); cursor: grab; transition: background 120ms ease, box-shadow 120ms ease; }
	.thinking-control :global(.thinking-thumb[data-active]) { cursor: grabbing; box-shadow: var(--shadow-md), 0 0 0 3px var(--accent-soft); }
	.thinking-control :global(.thinking-thumb:focus-visible) { outline: 2px solid var(--accent-primary); outline-offset: 3px; }
	.thinking-grip { width: 4px; height: 9px; border-left: 1px solid var(--accent-primary); border-right: 1px solid var(--accent-primary); opacity: .8; }
	.thinking-tooltip { position: absolute; bottom: calc(100% + 6px); left: 50%; transform: translate(-50%, 3px); padding: 3px 7px; border: 1px solid var(--border-soft); border-radius: 5px; background: var(--bg-elevated); color: var(--text-primary); box-shadow: var(--shadow-sm); font-size: .65rem; line-height: 1.3; white-space: nowrap; opacity: 0; pointer-events: none; transition: opacity 120ms ease, transform 120ms ease; }
	.thinking-tooltip.at-start { left: -1px; transform: translateY(3px); }
	.thinking-tooltip.at-end { left: auto; right: -1px; transform: translateY(3px); }
	.thinking-control :global(.thinking-thumb:is(:hover, :focus-visible, [data-active]) .thinking-tooltip) { opacity: 1; transform: translate(-50%, 0); }
	.thinking-control :global(.thinking-thumb:is(:hover, :focus-visible, [data-active]) .thinking-tooltip.at-start),
	.thinking-control :global(.thinking-thumb:is(:hover, :focus-visible, [data-active]) .thinking-tooltip.at-end) { transform: translateY(0); }
	.thinking-control :global(.is-unresolved .thinking-fill) { opacity: 0; }
	.thinking-control :global(.is-unresolved .thinking-thumb) { border-style: dashed; }
	.thinking-steps { display: flex; justify-content: space-between; gap: 2px; margin: 0 -3px; }
	.thinking-steps button { min-width: 0; padding: 4px 3px; border: 0; border-radius: 4px; background: transparent; color: var(--text-muted); font-size: .65rem; white-space: nowrap; }
	.thinking-steps button.current { color: var(--text-primary); background: var(--accent-soft); font-weight: 700; }
	.thinking-steps button:hover { background: var(--bg-soft); color: var(--text-primary); }
	button:focus-visible { outline: 2px solid var(--accent-primary); outline-offset: 2px; }
	p { font-size: .65rem; line-height: 1.4; color: var(--text-muted); margin: .4rem 0 0; }
	.thinking-switch { display: flex; align-items: center; gap: .65rem; background: transparent; border: 0; padding: .25rem 0; font-size: .8rem; }
	.thinking-switch span { width: 2rem; height: 1.1rem; background: var(--border-default); border-radius: 1rem; position: relative; }
	.thinking-switch span::after { content: ''; position: absolute; width: .8rem; height: .8rem; top: .15rem; left: .15rem; border-radius: 50%; background: var(--bg-elevated); transition: transform .15s; }
	.thinking-switch span.enabled { background: var(--accent-fill); }
	.thinking-switch span.enabled::after { transform: translateX(.9rem); }
	:disabled { opacity: .55; cursor: wait; }
	.thinking-control :global([data-disabled]) { opacity: .55; cursor: not-allowed; }
	@media (pointer: coarse) { .thinking-control :global(.thinking-slider) { height: 44px; } .thinking-tick { top: 20px; } .thinking-steps button { min-height: 32px; } }
	@media (prefers-reduced-motion: reduce) { .thinking-control :global(.thinking-fill), .thinking-control :global(.thinking-thumb), .thinking-tooltip, .thinking-switch span::after { transition: none; } }
</style>
