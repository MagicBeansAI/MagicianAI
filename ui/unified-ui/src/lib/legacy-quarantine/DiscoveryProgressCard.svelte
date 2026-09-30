<script lang="ts">
	/**
	 * DiscoveryProgressCard - Shows progress of autonomous parameter discovery
	 * Displays method, status, and results for discovery attempts
	 */
	export let parameterName: string = '';
	export let discoveryMethod: string = 'ContextAnalysis'; // "WebSearch", "FilesystemSearch", "APIQuery", etc.
	export let status: 'attempting' | 'succeeded' | 'failed' = 'attempting';
	export let discoveredValue: any = null;
	export let confidence: number = 0;
	export let reason: string = '';
	export let externalActions: boolean = false;

	const methodConfig: Record<string, { icon: string; label: string; color: string }> = {
		WebSearch: { icon: '🌐', label: 'Web Search', color: '#3b82f6' },
		FilesystemSearch: { icon: '📁', label: 'Filesystem Search', color: '#10b981' },
		APIQuery: { icon: '🔌', label: 'API Query', color: '#6366f1' },
		ToolExecution: { icon: '🔧', label: 'Tool Execution', color: '#f59e0b' },
		ContextAnalysis: { icon: '🧩', label: 'Context Analysis', color: '#8b5cf6' }
	};

	$: method = methodConfig[discoveryMethod] || {
		icon: '🔍',
		label: discoveryMethod,
		color: '#64748b'
	};

	$: statusConfig = {
		attempting: { icon: '⏳', label: 'Discovering...', color: '#f59e0b', bgColor: '#fef3c7' },
		succeeded: { icon: '✅', label: 'Discovered', color: '#10b981', bgColor: '#d1fae5' },
		failed: { icon: '❌', label: 'Failed', color: '#ef4444', bgColor: '#fef2f2' }
	};

	$: currentStatus = statusConfig[status];
</script>

<div class="discovery-card" class:attempting={status === 'attempting'}>
	<!-- Header -->
	<div class="card-header">
		<div class="method-info">
			<span class="method-icon" style="color: {method.color}">{method.icon}</span>
			<div class="method-details">
				<span class="method-label">{method.label}</span>
				<span class="parameter-name">{parameterName}</span>
			</div>
		</div>
		<div
			class="status-badge"
			style="background: {currentStatus.bgColor}; color: {currentStatus.color}; border-color: {currentStatus.color}"
		>
			<span class="status-icon">{currentStatus.icon}</span>
			<span class="status-label">{currentStatus.label}</span>
		</div>
	</div>

	<!-- Progress/Result -->
	{#if status === 'attempting'}
		<div class="discovery-progress">
			<div class="progress-bar">
				<div class="progress-fill"></div>
			</div>
			<p class="progress-text">Searching for parameter value...</p>
		</div>
	{:else if status === 'succeeded' && discoveredValue !== null}
		<div class="discovery-result success">
			<div class="result-header">
				<span class="result-label">Discovered Value</span>
				<span class="confidence-badge">{(confidence * 100).toFixed(0)}% confidence</span>
			</div>
			<div class="result-value">
				<pre>{JSON.stringify(discoveredValue, null, 2)}</pre>
			</div>
			{#if externalActions}
				<div class="external-actions-note">
					<span class="note-icon">⚠️</span>
					<span class="note-text">External actions were performed during discovery</span>
				</div>
			{/if}
		</div>
	{:else if status === 'failed'}
		<div class="discovery-result failure">
			<div class="result-header">
				<span class="result-label">Discovery Failed</span>
			</div>
			{#if reason}
				<p class="failure-reason">{reason}</p>
			{/if}
		</div>
	{/if}
</div>

<style>
	.discovery-card {
		background: white;
		border: 1px solid #e2e8f0;
		border-radius: 8px;
		padding: 1rem;
		transition: all 0.3s ease;
	}

	.discovery-card.attempting {
		border-color: #f59e0b;
		box-shadow: 0 0 0 3px #fef3c7;
	}

	.card-header {
		display: flex;
		justify-content: space-between;
		align-items: flex-start;
		margin-bottom: 0.75rem;
	}

	.method-info {
		display: flex;
		gap: 0.625rem;
		align-items: flex-start;
	}

	.method-icon {
		font-size: 1.5rem;
		line-height: 1;
	}

	.method-details {
		display: flex;
		flex-direction: column;
		gap: 0.125rem;
	}

	.method-label {
		font-size: 0.875rem;
		font-weight: 600;
		color: #1e293b;
	}

	.parameter-name {
		font-size: 0.75rem;
		font-family: var(--font-mono);
		color: #64748b;
	}

	.status-badge {
		display: flex;
		align-items: center;
		gap: 0.25rem;
		padding: 0.25rem 0.5rem;
		border-radius: 4px;
		border: 1px solid;
		font-size: 0.7rem;
		font-weight: 600;
	}

	.status-icon {
		font-size: 0.875rem;
	}

	/* Progress */
	.discovery-progress {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.progress-bar {
		height: 6px;
		background: #e5e7eb;
		border-radius: 3px;
		overflow: hidden;
	}

	.progress-fill {
		height: 100%;
		background: linear-gradient(90deg, #f59e0b 0%, #fbbf24 50%, #f59e0b 100%);
		background-size: 200% 100%;
		animation: progress-flow 1.5s ease-in-out infinite;
	}

	@keyframes progress-flow {
		0% {
			background-position: 200% 0;
		}
		100% {
			background-position: -200% 0;
		}
	}

	.progress-text {
		margin: 0;
		font-size: 0.75rem;
		color: #64748b;
		font-style: italic;
	}

	/* Results */
	.discovery-result {
		padding: 0.75rem;
		border-radius: 6px;
	}

	.discovery-result.success {
		background: #f0fdf4;
		border: 1px solid #86efac;
	}

	.discovery-result.failure {
		background: #fef2f2;
		border: 1px solid #fecaca;
	}

	.result-header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		margin-bottom: 0.5rem;
	}

	.result-label {
		font-size: 0.75rem;
		font-weight: 600;
		color: #1e293b;
		text-transform: uppercase;
		letter-spacing: 0.5px;
	}

	.confidence-badge {
		font-size: 0.7rem;
		font-weight: 700;
		color: #10b981;
		background: white;
		padding: 0.125rem 0.375rem;
		border-radius: 3px;
		border: 1px solid #86efac;
	}

	.result-value {
		background: white;
		border: 1px solid #d1fae5;
		border-radius: 4px;
		padding: 0.5rem;
		max-height: 150px;
		overflow-y: auto;
	}

	.result-value pre {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		color: #1e293b;
		white-space: pre-wrap;
		word-break: break-all;
	}

	.external-actions-note {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin-top: 0.5rem;
		padding: 0.5rem;
		background: #fefce8;
		border: 1px solid #fde047;
		border-radius: 4px;
	}

	.note-icon {
		font-size: 1rem;
	}

	.note-text {
		font-size: 0.7rem;
		color: #854d0e;
	}

	.failure-reason {
		margin: 0;
		font-size: 0.75rem;
		color: #991b1b;
		line-height: 1.4;
	}
</style>
