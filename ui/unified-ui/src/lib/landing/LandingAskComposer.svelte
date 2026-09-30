<script lang="ts">
	// The conversion control from the retired landing, rebuilt in the active
	// landing system: the example types in the REAL input's placeholder, then
	// the attached stage performs plan -> work -> result. Visitor text always
	// wins immediately. Timers are chained one at a time and are torn down on
	// pause, tab hide, and unmount; there is no synchronous recursive loop.
	import { onDestroy, onMount } from 'svelte';

	import { LANDING_ASK_RUNS, type LandingAskRun } from './landingAskRuns';

	export let value = '';
	export let taking = false;
	export let disabled = false;
	export let onSubmit: () => void = () => {};

	type Phase = 'typing' | 'plan' | 'work' | 'result' | 'file' | 'gap';

	let runIndex = 0;
	let run: LandingAskRun = LANDING_ASK_RUNS[0];
	let phase: Phase = 'typing';
	let typedAsk = '';
	let planShown = 0;
	let beatIndex = -1;
	let completedSteps: boolean[] = [];
	let reducedMotion = false;
	let tabHidden = false;
	let mounted = false;
	let previousPaused = false;
	let timer: ReturnType<typeof setTimeout> | null = null;

	$: paused = value.trim().length > 0;
	$: activeBeat = beatIndex >= 0 && beatIndex < run.beats.length ? run.beats[beatIndex] : null;
	$: ghostAsk = paused ? '' : phase === 'typing' && !reducedMotion ? `${typedAsk}▏` : typedAsk;
	$: if (mounted && paused !== previousPaused) syncPausedState(paused);

	function clearTimer(): void {
		if (!timer) return;
		clearTimeout(timer);
		timer = null;
	}

	function schedule(ms: number, next: () => void): void {
		clearTimer();
		timer = setTimeout(() => {
			timer = null;
			if (paused || tabHidden) return;
			next();
		}, ms);
	}

	function resetRun(index: number): void {
		runIndex =
			((index % LANDING_ASK_RUNS.length) + LANDING_ASK_RUNS.length) % LANDING_ASK_RUNS.length;
		run = LANDING_ASK_RUNS[runIndex];
		phase = 'typing';
		typedAsk = '';
		planShown = 0;
		beatIndex = -1;
		completedSteps = run.plan.map(() => false);
	}

	function startRun(index: number): void {
		clearTimer();
		resetRun(index);
		if (!paused && !tabHidden) typeNextCharacter();
	}

	function showFinishedRun(index: number): void {
		clearTimer();
		resetRun(index);
		typedAsk = run.ask;
		planShown = run.plan.length;
		completedSteps = run.plan.map(() => true);
		phase = 'result';
	}

	function typeNextCharacter(): void {
		if (typedAsk.length >= run.ask.length) {
			schedule(520, showNextPlanLine);
			return;
		}
		typedAsk = run.ask.slice(0, typedAsk.length + 1);
		// A bounded cadence feels typed without introducing a random test or
		// hydration surface. The small 5-step rhythm prevents a robotic metronome.
		const cadence = [42, 58, 48, 70, 52][typedAsk.length % 5];
		schedule(cadence, typeNextCharacter);
	}

	function showNextPlanLine(): void {
		phase = 'plan';
		if (planShown >= run.plan.length) {
			schedule(420, showNextBeat);
			return;
		}
		planShown += 1;
		schedule(420, showNextPlanLine);
	}

	function showNextBeat(): void {
		phase = 'work';
		if (activeBeat) {
			completedSteps[activeBeat.step] = true;
			completedSteps = [...completedSteps];
		}
		beatIndex += 1;
		if (beatIndex >= run.beats.length) {
			completedSteps = run.plan.map(() => true);
			phase = 'result';
			schedule(2600, () => {
				phase = 'file';
				schedule(1900, () => {
					phase = 'gap';
					schedule(650, () => startRun(runIndex + 1));
				});
			});
			return;
		}
		schedule(run.beats[beatIndex].ms, showNextBeat);
	}

	function syncPausedState(nextPaused: boolean): void {
		previousPaused = nextPaused;
		if (nextPaused) {
			clearTimer();
			return;
		}
		if (reducedMotion) showFinishedRun(runIndex);
		else startRun(runIndex);
	}

	function previewRun(index: number): void {
		if (paused || disabled || taking || index === runIndex) return;
		if (reducedMotion) showFinishedRun(index);
		else startRun(index);
	}

	function chooseRun(index: number): void {
		if (disabled || taking) return;
		value = LANDING_ASK_RUNS[index].ask;
	}

	function handleVisibility(): void {
		tabHidden = document.hidden;
		if (tabHidden) clearTimer();
		else if (!paused) {
			if (reducedMotion) showFinishedRun(runIndex);
			else startRun(runIndex);
		}
	}

	onMount(() => {
		mounted = true;
		previousPaused = paused;
		reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
		tabHidden = document.hidden;
		document.addEventListener('visibilitychange', handleVisibility);
		if (reducedMotion) showFinishedRun(0);
		else if (!paused && !tabHidden) startRun(0);
		else resetRun(0);
	});

	onDestroy(() => {
		clearTimer();
		if (typeof document !== 'undefined') {
			document.removeEventListener('visibilitychange', handleVisibility);
		}
	});
</script>

<div
	class="ask-shell"
	class:is-taking={taking}
	class:is-paused={paused}
>
	<form class="ask-form" on:submit|preventDefault={onSubmit}>
		<label class="sr-only" for="landing-task-description">Tell Magican what you want done</label>
		<input
			id="landing-task-description"
			class="lp-ask-input"
			type="text"
			bind:value
			placeholder={ghostAsk || 'Tell Magican what you want done'}
			required
			autocomplete="off"
			spellcheck="false"
			disabled={disabled || taking}
		/>
		<button
			class="ask-submit"
			type="submit"
			disabled={disabled || taking || !value.trim()}
			aria-label="Start delegating"
			title="Start delegating"
		>
			<span aria-hidden="true">→</span>
		</button>
	</form>

	<div class="ask-starters" aria-label="Starter tasks">
		{#each LANDING_ASK_RUNS as example, index (example.id)}
			<button
				type="button"
				class:is-playing={!paused && index === runIndex}
				disabled={disabled || taking}
				on:pointerenter={(event) => {
					if (event.pointerType === 'mouse') previewRun(index);
				}}
				on:focus={() => previewRun(index)}
				on:click={() => chooseRun(index)}
			>
				{example.chip}
			</button>
		{/each}
	</div>

	<aside class="ask-stage" data-phase={phase} aria-hidden="true">
		{#if paused}
			<div class="stage-yield">
				<span class="stage-yield-orb"></span>
				<strong>{taking ? 'Handing this to Magican…' : 'Your turn.'}</strong>
				<span>
					{taking
						? 'The conversation is opening.'
						: 'Say it naturally — Magican will work out the steps.'}
				</span>
			</div>
		{:else}
			<div class="stage-head">
				<span class="stage-live"></span>
				<span>Example run</span>
				<span class="stage-run-name">{run.chip}</span>
			</div>

			<ol class="stage-plan">
				{#each run.plan as line, index (run.id + line)}
					<li class:is-shown={index < planShown} class:is-done={completedSteps[index]}>
						<span class="stage-check">{completedSteps[index] ? '✓' : index + 1}</span>
						<span>{line}</span>
					</li>
				{/each}
			</ol>

			<div class="stage-floor">
				{#if activeBeat && phase === 'work'}
					{#key beatIndex}
						<div class="stage-activity" class:needs-you={activeBeat.needsYou}>
							<span class="stage-tool">{activeBeat.tool}</span>
							<span>{activeBeat.detail}</span>
						</div>
					{/key}
				{:else if phase === 'result' || phase === 'file'}
					<div class="stage-result" class:is-filed={phase === 'file'}>
						<strong>{run.result.title}</strong>
						{#each run.result.lines as line (line)}<span>{line}</span>{/each}
					</div>
				{/if}
			</div>

			<div class="stage-file" class:is-visible={phase === 'file' || phase === 'gap'}>
				<span>✓</span> {run.filedAs}
			</div>
		{/if}
	</aside>
</div>

<style>
	.ask-shell {
		--ask-accent: var(--accent-primary, #c2502a);
		--ask-accent-soft: var(--accent-primary-soft, rgba(194, 80, 42, 0.12));
		width: 100%;
		text-align: left;
		font-family: var(--font-primary, system-ui, sans-serif);
		background: var(--bg-elevated, #fff);
		border: 1px solid var(--border-default, rgba(0, 0, 0, 0.18));
		border-radius: var(--radius-lg, 14px);
		box-shadow:
			0 1px 0 rgba(255, 255, 255, 0.04) inset,
			var(--shadow-lg, 0 24px 48px -12px rgba(0, 0, 0, 0.25));
		overflow: hidden;
		backdrop-filter: blur(20px) saturate(140%);
		-webkit-backdrop-filter: blur(20px) saturate(140%);
		transition:
			transform 650ms var(--ease-settle, ease),
			border-color 650ms ease,
			box-shadow 650ms ease;
	}

	.ask-shell:focus-within {
		border-color: var(--ask-accent);
		box-shadow:
			0 1px 0 rgba(255, 255, 255, 0.06) inset,
			var(--shadow-lg, 0 28px 56px -12px rgba(0, 0, 0, 0.32)),
			var(--input-focus-shadow, 0 0 0 4px rgba(194, 80, 42, 0.12));
	}

	.ask-shell.is-taking {
		transform: translateY(2px) scale(0.988);
	}

	.ask-form {
		display: grid;
		grid-template-columns: minmax(0, 1fr) auto;
		align-items: center;
		gap: 10px;
		padding: 12px 12px 10px 14px;
		background: transparent;
	}

	.lp-ask-input {
		min-width: 0;
		min-height: 38px;
		padding: 8px 0;
		border: 0;
		outline: none;
		background: transparent;
		color: var(--text-primary);
		caret-color: var(--ask-accent);
		font-family: var(--font-primary, inherit);
		font-size: 14.5px;
		font-weight: 450;
		line-height: 22px;
		letter-spacing: -0.005em;
		view-transition-name: command-ask;
	}

	.lp-ask-input::placeholder {
		color: var(--text-faint, #aaa);
		opacity: 1;
	}

	.ask-submit {
		width: 36px;
		height: 36px;
		padding: 0;
		border: none;
		border-radius: var(--radius-full, 999px);
		background: var(--button-primary-bg, var(--text-ink, #1a1a1a));
		color: var(--button-primary-color, #fff);
		font-family: system-ui, sans-serif;
		font-size: 1.25rem;
		font-weight: 700;
		line-height: 1;
		cursor: pointer;
		transition:
			transform 650ms cubic-bezier(0.22, 1, 0.36, 1),
			opacity 650ms ease;
	}

	.ask-submit:hover:not(:disabled) {
		background: var(--ask-accent);
		transform: translateY(-1px);
	}

	.ask-submit:focus-visible {
		outline: 2px solid var(--ask-accent);
		outline-offset: 3px;
	}

	.ask-submit:disabled,
	.ask-starters button:disabled {
		cursor: default;
		opacity: 0.42;
	}

	.ask-starters {
		display: flex;
		gap: 0.38rem;
		overflow-x: auto;
		scrollbar-width: none;
		padding: 3px 12px 9px 14px;
		background: transparent;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
	}

	.ask-starters::-webkit-scrollbar {
		display: none;
	}

	.ask-starters button {
		flex: 0 0 auto;
		padding: 0.28rem 0.58rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: 999px;
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		color: var(--text-secondary);
		font-family: var(--font-primary, inherit);
		font-size: 0.68rem;
		font-weight: 560;
		cursor: pointer;
		transition:
			background 650ms ease,
			border-color 650ms ease,
			color 650ms ease;
	}

	.ask-starters button.is-playing {
		border-color: var(--accent-border, color-mix(in srgb, var(--ask-accent) 32%, transparent));
		background: var(--accent-soft, var(--ask-accent-soft));
		color: var(--ask-accent);
	}

	.ask-starters button:focus-visible {
		outline: 2px solid var(--ask-accent);
		outline-offset: 1px;
	}

	.ask-stage {
		height: 225px;
		box-sizing: border-box;
		display: flex;
		flex-direction: column;
		gap: 0.48rem;
		overflow: hidden;
		padding: 0.72rem 1rem 0.62rem;
		background: var(--bg-elevated, #fff);
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		color: var(--text-primary);
	}

	.stage-head {
		display: flex;
		align-items: center;
		gap: 0.46rem;
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 0.66rem;
		letter-spacing: 0.035em;
		color: var(--text-faint, #999);
	}

	.stage-live {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: var(--ask-accent);
		box-shadow: 0 0 0 5px var(--ask-accent-soft);
		animation: stage-breathe 1.8s ease-in-out infinite;
	}

	.stage-run-name {
		margin-left: auto;
		color: var(--text-secondary);
	}

	.stage-plan {
		min-height: 4.5rem;
		display: flex;
		flex-direction: column;
		gap: 0.26rem;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.stage-plan li {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		opacity: 0;
		transform: translateX(-7px);
		color: var(--text-secondary);
		font-size: 0.82rem;
		transition:
			opacity 700ms ease,
			transform 700ms ease,
			color 700ms ease;
	}

	.stage-plan li.is-shown {
		opacity: 1;
		transform: translateX(0);
	}

	.stage-plan li.is-done {
		color: var(--text-faint, #999);
	}

	.stage-check {
		width: 1.05rem;
		height: 1.05rem;
		flex: 0 0 auto;
		display: grid;
		place-items: center;
		border: 1px solid var(--border-default, rgba(0, 0, 0, 0.16));
		border-radius: 50%;
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 0.58rem;
		color: var(--ask-accent);
	}

	.stage-floor {
		min-height: 3.6rem;
		flex: 1;
		display: grid;
		align-items: center;
	}

	.stage-activity {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		font-size: 0.78rem;
		color: var(--text-secondary);
		animation: stage-rise 700ms ease both;
	}

	.stage-tool {
		flex: 0 0 auto;
		padding: 0.18rem 0.46rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: 999px;
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 0.62rem;
		color: var(--text-primary);
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
	}

	.stage-activity.needs-you .stage-tool {
		border-color: var(--ask-accent);
		color: var(--ask-accent);
	}

	.stage-result {
		display: grid;
		gap: 0.12rem;
		padding: 0.5rem 0.68rem;
		border: 1px solid var(--border-soft, rgba(0, 0, 0, 0.08));
		border-radius: var(--radius-md, 10px);
		background: var(--bg-soft, rgba(0, 0, 0, 0.04));
		box-shadow: var(--shadow-sm, 0 4px 12px rgba(0, 0, 0, 0.08));
		font-size: 0.74rem;
		color: var(--text-secondary);
		animation: stage-rise 750ms var(--ease-settle, ease) both;
		transition:
			opacity 800ms ease,
			transform 800ms ease;
	}

	.stage-result strong {
		font-size: 0.84rem;
		color: var(--text-primary);
	}

	.stage-result.is-filed {
		opacity: 0;
		transform: translateY(20px) scale(0.96);
	}

	.stage-file {
		min-height: 1.15rem;
		padding-top: 0.3rem;
		border-top: 1px solid var(--border-soft, rgba(0, 0, 0, 0.06));
		font-family: var(--font-mono, ui-monospace, monospace);
		font-size: 0.63rem;
		color: var(--text-faint, #999);
		opacity: 0;
		transition: opacity 750ms ease 180ms;
	}

	.stage-file.is-visible {
		opacity: 1;
	}

	.stage-file span {
		color: var(--ask-accent);
	}

	.stage-yield {
		flex: 1;
		display: grid;
		place-content: center;
		justify-items: center;
		gap: 0.44rem;
		text-align: center;
	}

	.stage-yield strong {
		font-size: 1rem;
	}

	.stage-yield > span:last-child {
		max-width: 24rem;
		font-size: 0.78rem;
		color: var(--text-secondary);
	}

	.stage-yield-orb {
		width: 25px;
		height: 25px;
		border-radius: 45% 55% 60% 40% / 55% 44% 56% 45%;
		background: var(--ask-accent);
		box-shadow: 0 0 22px var(--ask-accent-soft);
		animation: stage-organic 2.4s ease-in-out infinite alternate;
	}

	.sr-only {
		position: absolute;
		width: 1px;
		height: 1px;
		padding: 0;
		margin: -1px;
		overflow: hidden;
		clip: rect(0, 0, 0, 0);
		white-space: nowrap;
		border: 0;
	}

	@keyframes stage-breathe {
		50% { opacity: 0.42; transform: scale(0.78); }
	}

	@keyframes stage-rise {
		from { opacity: 0; transform: translateY(6px); }
	}

	@keyframes stage-organic {
		to {
			transform: scale(1.1) rotate(12deg);
			border-radius: 60% 40% 45% 55% / 42% 58% 45% 55%;
		}
	}

	@media (max-width: 640px) {
		.ask-form {
			padding: 0.48rem;
		}

		.lp-ask-input {
			min-height: 40px;
		}

		.ask-stage {
			height: 232px;
			padding-inline: 0.82rem;
		}

		.stage-activity {
			align-items: flex-start;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.ask-shell,
		.ask-submit,
		.stage-plan li,
		.stage-result,
		.stage-file {
			transition-duration: 0.01ms;
		}

		.stage-live,
		.stage-yield-orb,
		.stage-activity,
		.stage-result {
			animation: none;
		}
	}
</style>
