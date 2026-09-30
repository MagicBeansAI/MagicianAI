<script lang="ts">
	/**
	 * ApprovalFlow Component — GD-F03-E
	 *
	 * Linear approval workflow visualization.
	 */

	interface Approver {
		id: string;
		name: string;
		avatar?: string;
		status: 'pending' | 'approved' | 'rejected';
		timestamp?: number;
	}

	export let approvers: unknown = [];
	export let requireAll: unknown = true;

	function isApprover(item: unknown): item is Approver {
		return (
			typeof item === 'object' &&
			item !== null &&
			typeof (item as Record<string, unknown>).id === 'string' &&
			typeof (item as Record<string, unknown>).name === 'string' &&
			((item as Record<string, unknown>).status === 'pending' ||
				(item as Record<string, unknown>).status === 'approved' ||
				(item as Record<string, unknown>).status === 'rejected')
		);
	}

	function normalizeApprovers(value: unknown): Approver[] {
		if (!Array.isArray(value)) return [];
		return value.filter(isApprover);
	}

	function toBoolean(value: unknown): boolean {
		return value === true || value === 'true';
	}

	function getStatusIcon(status: string): string {
		switch (status) {
			case 'approved':
				return '✓';
			case 'rejected':
				return '✗';
			default:
				return '⏳';
		}
	}

	function getStatusClass(status: string): string {
		return `muij-approval-status-${status}`;
	}

	function getInitials(name: string): string {
		if (!name || !name.trim()) return '??';
		return name.trim().slice(0, 2).toUpperCase();
	}

	$: safeApprovers = normalizeApprovers(approvers);
	$: safeRequireAll = toBoolean(requireAll);

	$: approvedCount = safeApprovers.filter((a) => a.status === 'approved').length;
	$: rejectedCount = safeApprovers.filter((a) => a.status === 'rejected').length;
</script>

{#if safeApprovers.length > 0}
	<div class="muij-approval-flow" role="group" aria-label="Approval flow">
		<div class="muij-approval-summary">
			<span class="muij-approval-count">
				{approvedCount}/{safeApprovers.length} approved
			</span>
			{#if rejectedCount > 0}
				<span class="muij-approval-rejected">• {rejectedCount} rejected</span>
			{/if}
		</div>

		<div class="muij-approval-steps">
			{#each safeApprovers as approver, i}
				<div class="muij-approval-step" class:muij-approval-step-complete={approver.status !== 'pending'}>
					<div class="muij-approval-connector">
						{#if i > 0}
							<div class="muij-approval-line"></div>
						{/if}
						<div class="muij-approval-node {getStatusClass(approver.status)}">
							{#if approver.avatar}
								<img class="muij-approval-avatar" src={approver.avatar} alt={approver.name} />
							{:else}
								<span class="muij-approval-initials">{getInitials(approver.name)}</span>
							{/if}
						</div>
						{#if i < safeApprovers.length - 1}
							<div class="muij-approval-line" class:muij-approval-line-active={approver.status === 'approved'}></div>
						{/if}
					</div>
					<div class="muij-approval-info">
						<span class="muij-approval-name">{approver.name}</span>
						<span class="muij-approval-status {getStatusClass(approver.status)}">
							{getStatusIcon(approver.status)} {approver.status}
						</span>
					</div>
				</div>
			{/each}
		</div>
	</div>
{:else}
	<div class="muij-approval-empty">
		<span class="muij-approval-empty-text">No approvers</span>
	</div>
{/if}

<style>
	.muij-approval-flow {
		font-family: var(--font-primary);
		font-size: 0.8125rem;
	}

	.muij-approval-summary {
		margin-bottom: var(--space-sm);
		font-size: 0.75rem;
		color: var(--text-muted);
	}

	.muij-approval-count {
		font-weight: 500;
		color: var(--text-secondary);
	}

	.muij-approval-rejected {
		color: #cb2431;
	}

	.muij-approval-steps {
		display: flex;
		flex-direction: column;
		gap: 0;
	}

	.muij-approval-step {
		display: flex;
		gap: var(--space-sm);
	}

	.muij-approval-connector {
		display: flex;
		flex-direction: column;
		align-items: center;
		flex-shrink: 0;
	}

	.muij-approval-line {
		width: 2px;
		height: var(--space-sm);
		background: var(--border-soft);
	}

	.muij-approval-line-active {
		background: #22c55e;
	}

	.muij-approval-node {
		width: 32px;
		height: 32px;
		border-radius: 50%;
		background: var(--bg-soft);
		border: 2px solid var(--border-soft);
		display: flex;
		align-items: center;
		justify-content: center;
		overflow: hidden;
	}

	.muij-approval-avatar {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}

	.muij-approval-initials {
		font-size: 0.625rem;
		font-weight: 600;
		color: var(--text-muted);
	}

	:global(.muij-approval-status-approved) .muij-approval-node {
		border-color: #22c55e;
		background: #ecfdf5;
	}

	:global(.muij-approval-status-rejected) .muij-approval-node {
		border-color: #cb2431;
		background: #fef2f2;
	}

	.muij-approval-info {
		flex: 1;
		padding-top: 4px;
	}

	.muij-approval-name {
		display: block;
		font-weight: 500;
		color: var(--text-primary);
	}

	.muij-approval-status {
		font-size: 0.6875rem;
		color: var(--text-muted);
	}

	.muij-approval-status-approved {
		color: #22c55e;
	}

	.muij-approval-status-rejected {
		color: #cb2431;
	}

	.muij-approval-empty {
		padding: var(--space-md);
		text-align: center;
		color: var(--text-muted);
		font-size: 0.75rem;
	}
</style>
