<script lang="ts">
	export let visible: boolean = false;
	export let message: string = 'You have reached your current quota.';
	export let used: number = 0;
	export let limit: number = 0;
	export let retryAfterSec: number = 0;
	export let queued: boolean = false;
	export let perUserUsed: number = 0;
	export let perUserLimit: number = 0;
	export let queuedRequestId: string | null = null;

	$: retryMinutes = retryAfterSec > 0 ? Math.ceil(retryAfterSec / 60) : 0;
	$: usagePercent = limit > 0 ? Math.round((used / limit) * 100) : 0;
	$: userUsagePercent = perUserLimit > 0 ? Math.round((perUserUsed / perUserLimit) * 100) : 0;
</script>

{#if visible}
	<div class="quota-banner" role="alert" aria-live="polite">
		<div class="quota-banner-content">
			<div class="quota-banner-icon">⚠️</div>
			<div class="quota-banner-info">
				<div class="quota-banner-title">Quota Limit Reached</div>
				<div class="quota-banner-message">{message}</div>

				{#if limit > 0}
					<div class="quota-usage">
						<span class="usage-label">System Usage:</span>
						<span class="usage-value">{used} / {limit}</span>
						<span class="usage-percent">({usagePercent}%)</span>
					</div>
				{/if}

				{#if perUserLimit > 0}
					<div class="quota-usage">
						<span class="usage-label">Your Usage:</span>
						<span class="usage-value">{perUserUsed} / {perUserLimit}</span>
						<span class="usage-percent">({userUsagePercent}%)</span>
					</div>
				{/if}

				{#if queued}
					<div class="queue-notice">
						<span class="queue-icon">🕐</span>
						<span class="queue-text">Your request has been queued</span>
						{#if queuedRequestId}
							<span class="queue-id">ID: {queuedRequestId}</span>
						{/if}
					</div>
				{/if}

				{#if retryAfterSec > 0}
					<div class="retry-notice">
						Retry available in approximately <strong>{retryMinutes}</strong> minute{retryMinutes !== 1 ? 's' : ''}
					</div>
				{/if}
			</div>
		</div>
	</div>
{/if}

<style>
	.quota-banner {
		position: fixed;
		top: 0;
		left: 0;
		right: 0;
		z-index: 9999;
		background: linear-gradient(135deg, #ef4444 0%, #dc2626 100%);
		color: white;
		padding: 1rem 1.5rem;
		box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1), 0 2px 4px -1px rgba(0, 0, 0, 0.06);
		animation: slideDown 0.3s ease-out;
	}

	@keyframes slideDown {
		from {
			transform: translateY(-100%);
			opacity: 0;
		}
		to {
			transform: translateY(0);
			opacity: 1;
		}
	}

	.quota-banner-content {
		max-width: 1200px;
		margin: 0 auto;
		display: flex;
		align-items: flex-start;
		gap: 1rem;
	}

	.quota-banner-icon {
		font-size: 1.5rem;
		flex-shrink: 0;
	}

	.quota-banner-info {
		flex: 1;
		min-width: 0;
	}

	.quota-banner-title {
		font-size: 1.125rem;
		font-weight: 700;
		margin-bottom: 0.25rem;
	}

	.quota-banner-message {
		font-size: 0.875rem;
		opacity: 0.95;
		margin-bottom: 0.75rem;
	}

	.quota-usage {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.8125rem;
		margin-bottom: 0.25rem;
		opacity: 0.9;
	}

	.usage-label {
		font-weight: 600;
	}

	.usage-value {
		font-family: var(--font-mono);
	}

	.usage-percent {
		opacity: 0.8;
	}

	.queue-notice {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin-top: 0.75rem;
		padding: 0.5rem 0.75rem;
		background: rgba(255, 255, 255, 0.15);
		border-radius: 0.375rem;
		font-size: 0.875rem;
	}

	.queue-icon {
		font-size: 1rem;
	}

	.queue-text {
		font-weight: 600;
	}

	.queue-id {
		opacity: 0.8;
		font-size: 0.75rem;
		font-family: var(--font-mono);
	}

	.retry-notice {
		margin-top: 0.75rem;
		font-size: 0.875rem;
		padding: 0.5rem 0.75rem;
		background: rgba(255, 255, 255, 0.1);
		border-radius: 0.375rem;
	}

	.retry-notice strong {
		font-weight: 700;
		text-decoration: underline;
	}

	@media (max-width: 640px) {
		.quota-banner {
			padding: 0.875rem 1rem;
		}

		.quota-banner-content {
			flex-direction: column;
			gap: 0.5rem;
		}

		.quota-banner-icon {
			font-size: 1.25rem;
		}

		.quota-banner-title {
			font-size: 1rem;
		}

		.quota-usage {
			flex-direction: column;
			align-items: flex-start;
			gap: 0.25rem;
		}
	}
</style>
