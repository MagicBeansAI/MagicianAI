<script lang="ts">
	// `/settings?embed=1` (rerouted here by `src/hooks.ts`): the same Settings
	// page, in a shell holding only what Settings itself uses.
	import EmbeddedSettingsShell from '$lib/shell/EmbeddedSettingsShell.svelte';
	import SettingsPage from '../../(app)/settings/+page.svelte';
</script>

<EmbeddedSettingsShell>
	<SettingsPage />
</EmbeddedSettingsShell>
