<script lang="ts">
	/**
	 * QRCode Component — GD-F02-G
	 *
	 * QR code generation using external API.
	 * No external npm dependencies.
	 */

	export let value: unknown = '';
	export let size: unknown = 128;
	export let bgColor: unknown = '#ffffff';
	export let fgColor: unknown = '#000000';

	function toString(value: unknown): string {
		return typeof value === 'string' ? value : '';
	}

	function toNumber(value: unknown, fallback: number): number {
		if (typeof value === 'number' && Number.isFinite(value) && value > 0) {
			return Math.floor(value);
		}
		return fallback;
	}

	function toColor(value: unknown, fallback: string): string {
		if (typeof value === 'string' && /^#[0-9a-fA-F]{6}$/.test(value)) {
			return value.replace('#', '');
		}
		if (typeof value === 'string' && /^[0-9a-fA-F]{6}$/.test(value)) {
			return value;
		}
		return fallback.replace('#', '');
	}

	$: safeValue = toString(value);
	$: safeSize = toNumber(size, 128);
	$: safeBgColor = toColor(bgColor, 'ffffff');
	$: safeFgColor = toColor(fgColor, '000000');

	$: qrUrl = safeValue
		? `https://api.qrserver.com/v1/create-qr-code/?size=${safeSize}x${safeSize}&bgcolor=${safeBgColor}&color=${safeFgColor}&data=${encodeURIComponent(safeValue)}`
		: '';

	let hasError = false;

	function handleError(): void {
		hasError = true;
	}
</script>

{#if !safeValue}
	<div class="muij-qrcode-placeholder" style:width="{safeSize}px" style:height="{safeSize}px">
		<span class="muij-qrcode-placeholder-icon" aria-hidden="true">📱</span>
		<span class="muij-qrcode-placeholder-text">No data</span>
	</div>
{:else if hasError}
	<div class="muij-qrcode-error" style:width="{safeSize}px" style:height="{safeSize}px">
		<span class="muij-qrcode-error-icon" aria-hidden="true">⚠️</span>
		<span class="muij-qrcode-error-text">QR failed</span>
	</div>
{:else}
	<img
		class="muij-qrcode"
		src={qrUrl}
		alt="QR code for: {safeValue}"
		width={safeSize}
		height={safeSize}
		on:error={handleError}
	/>
{/if}

<style>
	.muij-qrcode {
		display: block;
		border-radius: var(--radius-sm);
	}

	.muij-qrcode-placeholder,
	.muij-qrcode-error {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: var(--space-xs);
		background: var(--bg-soft);
		border: 1px dashed var(--border-soft);
		border-radius: var(--radius-sm);
		color: var(--text-muted);
		font-family: var(--font-primary);
	}

	.muij-qrcode-placeholder-icon,
	.muij-qrcode-error-icon {
		font-size: 1.5rem;
	}

	.muij-qrcode-placeholder-text,
	.muij-qrcode-error-text {
		font-size: 0.625rem;
	}
</style>
