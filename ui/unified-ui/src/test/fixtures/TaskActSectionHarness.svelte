<script lang="ts">
	import { ACT_TITLES, type ActId } from '$lib/magician/tasks/taskCapabilities';
	import TaskActSection from '$lib/magician/tasks/TaskActSection.svelte';

	/**
	 * Exists for the two things a bare `render` cannot supply: the act body, which
	 * arrives through the default slot, and a listener for the toggle event. The
	 * prop-only cases render `TaskActSection` directly rather than through here —
	 * including the provenance cases, which is why no `provenance` prop is
	 * forwarded: a prop no test passes is a prop nothing checks still works.
	 *
	 * The slot holds **a focusable control as well as prose**, because that is what
	 * the panel puts here — an output row's Open and Reveal, a preview toggle, an
	 * ask's submit — and because the focus-rescue case needs somewhere inside the
	 * body for the caret to be. It used to stand on the act's own `Details` button,
	 * which was the one focusable thing a bare `TaskActSection` rendered; retiring
	 * that disclosure left the component with none of its own, and a body whose only
	 * content is a paragraph cannot express "the reader was standing in it".
	 */

	export let id: ActId = 'run';
	export let open = false;
	export let summary = '14 steps · 2 retries · 3m 12s';

	let toggled: ActId[] = [];
</script>

<TaskActSection
	{id}
	title={ACT_TITLES[id]}
	{summary}
	{open}
	on:toggle={(event) => (toggled = [...toggled, event.detail])}
>
	<p>Read 3 files</p>
	<!-- Named so it collides with neither `/^Run/` nor `/^Plan/`, which is how the
	     header is found. -->
	<button type="button">Open output file</button>
</TaskActSection>

<output data-testid="act-toggles">{toggled.join(',')}</output>
