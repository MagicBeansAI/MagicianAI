<script lang="ts">
	// The ONE pulsing "audio is being captured right now" indicator, shared by
	// TopBar, the Meetings page, and the Attention
	// page — so the capture signal can't drift apart across surfaces.
	export let size = 8;
</script>

<span class="rec-dot" style:width="{size}px" style:height="{size}px" aria-hidden="true"></span>

<style>
	.rec-dot {
		display: inline-block;
		border-radius: 50%;
		background: var(--accent-danger, #e5484d);
		animation: rec-dot-pulse 1.6s ease-in-out infinite;
	}

	@keyframes rec-dot-pulse {
		0%,
		100% {
			opacity: 1;
		}
		50% {
			opacity: 0.35;
		}
	}
</style>
