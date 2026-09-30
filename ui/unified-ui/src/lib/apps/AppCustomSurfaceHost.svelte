<script lang="ts">
	import type { AppCustomSurfaceHostEnvelope } from './appCustomSurface';

	export let envelope: AppCustomSurfaceHostEnvelope;

	$: title = envelope.installation_id;
</script>

<iframe
	class="custom-surface-frame"
	title={`Custom surface ${title}`}
	sandbox=""
	referrerpolicy="no-referrer"
	srcdoc={envelope.srcdoc}
></iframe>

<style>
	.custom-surface-frame {
		display: block;
		width: 100%;
		min-height: 28rem;
		border: 0;
		background: var(--bg-page, #fff);
	}
</style>
