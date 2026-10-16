<script lang="ts">
	import type { TaskAskState } from '$lib/magician/tasks/taskAsk';
	import type { TaskFilePreview } from '$lib/magician/tasks/taskFilePreview';
	import UnifiedTaskPanel, {
		type TaskPanelModel
	} from '$lib/magician/tasks/UnifiedTaskPanel.svelte';

	/**
	 * Exists for the one thing a bare `render` cannot supply: a listener for the
	 * events the panel dispatches — `retry`, the ask's `answer`, the run picker's
	 * `selectRun`, and the Output act's `openFile`, `revealFile` and
	 * `previewFile`. Every other case renders
	 * `UnifiedTaskPanel` directly — the panel owns which act is open, so its
	 * `toggle` handling is observable from its own markup, and `rerender` reaches
	 * every prop.
	 */
	export let task: TaskPanelModel | null = null;
	export let loadError: string | null = null;
	export let lastLoadedAt: number | null = null;
	export let now = 0;
	export let outputActions = false;
	export let filePreviews = false;
	export let filePreview: TaskFilePreview | null = null;
	export let answerAsk = false;
	export let askState: TaskAskState | null = null;

	let retries = 0;
	/**
	 * Each answer as `<ask id>:<what was carried>`. The id is recorded beside the
	 * value because the value alone cannot tell a panel that answered the ask it
	 * was showing from one that answered whichever ask it happened to hold, and
	 * `open` records the handoff — an ask with no answer to carry, only a request
	 * to be taken somewhere that can collect one.
	 */
	let askEvents: string[] = [];
	/**
	 * Each file event as `kind:name@index`. The index is recorded beside the name
	 * because the name alone cannot tell a panel that reports the row it was
	 * asked about from one that reports a row it picked.
	 *
	 * `previewFile` is recorded in the same list as the other two on purpose: what
	 * the list shows is not only which row was asked about but **when** anything
	 * was asked at all, and a preview request that fired on render rather than on
	 * expand would show up here before any click.
	 */
	let fileEvents: string[] = [];
	/**
	 * Each run the reader picked, in order. **The harness does not act on them** —
	 * it records and re-renders nothing — which is what lets a test assert that the
	 * panel reports a choice without also asserting that the model it is handed
	 * changed. Those are two different claims, and the second belongs to the
	 * adapter's own test.
	 */
	let runEvents: string[] = [];

	function record(kind: string, detail: { file: { name: string }; index: number }): void {
		fileEvents = [...fileEvents, `${kind}:${detail.file.name}@${detail.index}`];
	}
</script>

<UnifiedTaskPanel
	{task}
	{loadError}
	{lastLoadedAt}
	{now}
	{outputActions}
	{filePreviews}
	{filePreview}
	on:retry={() => (retries += 1)}
	on:openFile={(event) => record('open', event.detail)}
	on:revealFile={(event) => record('reveal', event.detail)}
	on:previewFile={(event) => record('preview', event.detail)}
	{answerAsk}
	{askState}
	on:answer={(event) =>
		(askEvents = [
			...askEvents,
			`${event.detail.ask.id}:${event.detail.result === null ? 'open' : JSON.stringify(event.detail.result)}`
		])}
	on:selectRun={(event) => (runEvents = [...runEvents, event.detail.executionId])}
/>

<output data-testid="panel-retries">{retries}</output>
<output data-testid="panel-file-events">{fileEvents.join(' ')}</output>
<output data-testid="panel-ask-events">{askEvents.join(' ')}</output>
<output data-testid="panel-run-events">{runEvents.join(' ')}</output>
