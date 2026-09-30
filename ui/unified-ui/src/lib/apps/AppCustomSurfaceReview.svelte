<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import { toggleCustomSurfaceGrant, type AppInstallationReview, type AppInstallationApproveRequest } from './installationReview';

	export let review: Pick<AppInstallationReview, 'requested_custom_surface'>;
	export let grant: AppInstallationApproveRequest;
	const dispatch = createEventDispatcher<{ change: AppInstallationApproveRequest }>();
</script>

{#if review.requested_custom_surface}
	{@const surface = review.requested_custom_surface}
	<fieldset>
		<legend>Interactive pages</legend>
		<p>Choose which pages this app may open. Scripts run inside an isolated page; app actions remain limited to its granted permissions.</p>
		{#each surface.entry_points as entry (entry.route)}
			<label>
				<input type="checkbox"
					checked={grant.granted_custom_surface_entry_points?.some((selected) => selected.route === entry.route && selected.document === entry.document && selected.reviewed_request_digest === surface.request_digest) ?? false}
					on:change={() => dispatch('change', toggleCustomSurfaceGrant(review, grant, entry.route))} />
				Allow interactive page {entry.route}
			</label>
		{/each}
		<p>Unchecked pages stay unavailable; the app may offer a standard view instead.</p>
		<details>
			<summary>Review page code and isolation</summary>
			<p>{surface.executable_member_count} script file(s), {surface.executable_bytes.toLocaleString()} bytes. {surface.scan_finding_count} scan finding(s).</p>
			<ul>
				{#each surface.entry_points as entry}
					<li>{entry.route}: <code>{entry.document}</code> · <code>{entry.document_digest}</code></li>
				{/each}
				{#each surface.executable_members as member}
					<li><code>{member.path}</code> · {member.byte_len.toLocaleString()} bytes · <code>{member.content_digest}</code></li>
				{/each}
			</ul>
			{#if surface.scan_findings.length}
				<p>Scan findings are review hints; they do not grant network or device access.</p>
				<ul>{#each surface.scan_findings as finding}<li><code>{finding.path}</code>: {finding.pattern}</li>{/each}</ul>
			{/if}
			<p>Sandbox: <code>{surface.sandbox}</code></p>
			<p>Content policy: <code>{surface.csp}</code></p>
		</details>
	</fieldset>
{/if}

<style>
	fieldset { border: 1px solid var(--border-soft); border-radius: 8px; padding: .7rem; min-width: 0; }
	legend { font-weight: 650; }
	p, details { color: var(--text-secondary); font-size: .85rem; }
	label { display: flex; align-items: center; gap: .5rem; margin: .5rem 0; }
	code, li { overflow-wrap: anywhere; }
	summary { cursor: pointer; }
</style>
