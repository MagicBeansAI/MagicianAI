<script lang="ts">
	/**
	 * Skills page — the system-wide skill catalog: every skill authored in
	 * skillshub, every extras-root skill, skills installed into scopes from
	 * `path:` sources, and the built-in compiled packs. Procedures and
	 * personality modes are first-class rows with the same affordances as
	 * tool skills.
	 *
	 * Data: `GET /api/magician/v2/skills/catalog` (origin + install footprint
	 * per scope). The page is tabbed (`?tab=`, deep-linkable):
	 *   All · Tools · Procedures · Personalities · Built-ins
	 *
	 * Actions (admin, `X-Magician-Setup-Token` — persisted in sessionStorage,
	 * find the token on /vault):
	 *   - Install into scopes — POST /skills/install (`skillshub:<name>`).
	 *   - Remove from scopes — POST /skills/{name}/uninstall. Default removal
	 *     keeps the skill's config/.env, auth/ and .skill-state/ (reinstall
	 *     restores them); "purge" deletes everything behind a confirm.
	 *   - Allow-for-agent — edits an agent definition's `tools:` list.
	 *   - Install from path — the header form for `path:<abs>` sources.
	 */
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { page } from '$app/stores';
	import { scopeIdentityStore } from '$lib/stores/scopeIdentityStore';
	import { timedFetch } from '$lib/shared/fetch';
	import SkillsEnvironmentPanel from './SkillsEnvironmentPanel.svelte';
	import SkillEnvEditor from './SkillEnvEditor.svelte';

	const SETUP_TOKEN_STORAGE_KEY = 'magician:vault:setup-token';

	type CatalogKind = 'tool' | 'procedure' | 'personality' | 'compiled';
	type CatalogTab = 'all' | CatalogKind;
	// 'environment' renders the runtime env panel instead of the catalog.
	type PageTab = CatalogTab | 'environment';

	interface CatalogEntry {
		name: string;
		description: string;
		kind: CatalogKind;
		// 'skillshub' (authored source) | 'extras' | 'scope' (path-installed)
		// | 'built-in' (compiled packs).
		origin: string;
		requires_bins: string[];
		requires_env: string[];
		installed_scopes: string[];
	}

	interface CatalogResponse {
		skills: CatalogEntry[];
	}

	const TAB_LABELS: Record<PageTab, string> = {
		all: 'All',
		tool: 'Tools',
		procedure: 'Procedures',
		personality: 'Personalities',
		compiled: 'Built-ins',
		environment: 'Environment'
	};
	const TAB_ORDER: PageTab[] = [
		'all',
		'tool',
		'procedure',
		'personality',
		'compiled',
		'environment'
	];

	function parseTab(value: string | null): PageTab {
		return TAB_ORDER.includes(value as PageTab) ? (value as PageTab) : 'all';
	}

	let skills: CatalogEntry[] = [];
	let loading = true;
	let error: string | null = null;
	let textFilter = '';

	// Install-from-path form state.
	let installFormOpen = false;
	let installSource = '';
	let installWorkspaces = '';
	let installToken = '';
	let installSubmitting = false;
	let installError: string | null = null;
	let installSuccess: string | null = null;

	// Per-row action state — inflight flags + last result per row.
	let actionInflight: Record<string, boolean> = {};
	let actionMessage: Record<string, { kind: 'ok' | 'err'; text: string }> = {};
	// Row-level confirm state for Remove (second click commits) and purge opt-in.
	let confirmRemove: string | null = null;
	let removePurge = false;

	// Allow-for-agent state.
	interface AgentSummary {
		agent_id: string;
		tools: string[];
	}
	let agents: AgentSummary[] = [];
	let agentsLoaded = false;
	let openAllowFor: string | null = null;
	// Per-skill env editor expander (skills installed in the active scope).
	let openEnvFor: string | null = null;

	$: scope = $scopeIdentityStore;
	$: currentScope = `${scope?.principal || 'anonymous'}/${scope?.workspace || 'default'}`;
	$: activeTab = parseTab($page.url.searchParams.get('tab'));
	// The Environment tab is not a catalog filter; it swaps the whole panel.
	$: activeKind = activeTab === 'environment' ? 'all' : activeTab;

	$: filteredSkills = skills.filter((s) => {
		if (activeKind !== 'all' && s.kind !== activeKind) return false;
		if (textFilter) {
			const needle = textFilter.toLowerCase();
			if (
				!s.name.toLowerCase().includes(needle) &&
				!s.description.toLowerCase().includes(needle)
			) {
				return false;
			}
		}
		return true;
	});

	$: counts = {
		all: skills.length,
		tool: skills.filter((s) => s.kind === 'tool').length,
		procedure: skills.filter((s) => s.kind === 'procedure').length,
		personality: skills.filter((s) => s.kind === 'personality').length,
		compiled: skills.filter((s) => s.kind === 'compiled').length
	};

	function selectTab(tab: PageTab) {
		const params = new URLSearchParams($page.url.searchParams);
		if (tab === 'all') params.delete('tab');
		else params.set('tab', tab);
		const query = params.toString();
		void goto(`${$page.url.pathname}${query ? `?${query}` : ''}`, { replaceState: true });
	}

	async function loadSkills() {
		loading = true;
		error = null;
		try {
			// The catalog exposes every scope's install footprint, so the
			// server gates it behind the setup token (enter it once below or
			// on /vault; it persists for the session).
			const headers: Record<string, string> = {};
			const token = installToken.trim() || readStoredToken();
			if (token) headers['X-Magician-Setup-Token'] = token;
			const res = await timedFetch('/api/magician/v2/skills/catalog', { headers });
			if (res.status === 401) {
				throw new Error(
					'Setup token required — open “+ Install from path”, paste the token (find it on /vault), then Reload.'
				);
			}
			if (!res.ok) {
				throw new Error(`server returned ${res.status}`);
			}
			const data: CatalogResponse = await res.json();
			skills = data.skills ?? [];
		} catch (e) {
			error = e instanceof Error ? e.message : String(e);
			skills = [];
		} finally {
			loading = false;
		}
	}

	onMount(() => {
		try {
			const saved = sessionStorage.getItem(SETUP_TOKEN_STORAGE_KEY);
			if (saved) installToken = saved;
		} catch {
			// sessionStorage may be unavailable in restricted contexts; ignore.
		}
		loadSkills();
	});

	function readStoredToken(): string {
		try {
			return sessionStorage.getItem(SETUP_TOKEN_STORAGE_KEY) ?? '';
		} catch {
			return '';
		}
	}

	function requireToken(rowKey: string): string | null {
		const token = installToken.trim() || readStoredToken();
		if (!token) {
			actionMessage = {
				...actionMessage,
				[rowKey]: { kind: 'err', text: 'Setup token required (find it on /vault).' }
			};
			return null;
		}
		return token;
	}

	function persistToken(token: string) {
		try {
			sessionStorage.setItem(SETUP_TOKEN_STORAGE_KEY, token);
		} catch {
			// non-fatal
		}
	}

	async function postSkill(
		url: string,
		body: unknown,
		rowKey: string,
		okText: (data: any) => string
	): Promise<boolean> {
		if (actionInflight[rowKey]) return false;
		const token = requireToken(rowKey);
		if (!token) return false;
		actionInflight = { ...actionInflight, [rowKey]: true };
		try {
			const res = await timedFetch(url, {
				method: 'POST',
				headers: {
					'Content-Type': 'application/json',
					'X-Magician-Setup-Token': token
				},
				body: JSON.stringify(body)
			});
			const data = await res.json().catch(() => null);
			if (!res.ok) {
				actionMessage = {
					...actionMessage,
					[rowKey]: {
						kind: 'err',
						text:
							(data && (data.reason || data.message || data.error)) ||
							`server returned ${res.status}`
					}
				};
				return false;
			}
			persistToken(token);
			actionMessage = { ...actionMessage, [rowKey]: { kind: 'ok', text: okText(data) } };
			await loadSkills();
			return true;
		} catch (e) {
			actionMessage = {
				...actionMessage,
				[rowKey]: { kind: 'err', text: e instanceof Error ? e.message : String(e) }
			};
			return false;
		} finally {
			actionInflight = { ...actionInflight, [rowKey]: false };
		}
	}

	function installedHere(skill: CatalogEntry): boolean {
		return skill.installed_scopes.includes(currentScope);
	}

	function installedBadge(skill: CatalogEntry): string {
		const n = skill.installed_scopes.length;
		return n > 1 ? `installed · ${n} scopes` : 'installed';
	}

	function elsewhereBadge(skill: CatalogEntry): string {
		const n = skill.installed_scopes.length;
		return `${n} other scope${n === 1 ? '' : 's'}`;
	}

	function installTargetsFor(skill: CatalogEntry): string[] {
		// Row installs target the ACTIVE scope only; multi-scope installs go
		// through the explicit install form (whose workspaces field must not
		// leak into row actions).
		return skill.installed_scopes.includes(currentScope) ? [] : [currentScope];
	}

	function installSkill(skill: CatalogEntry) {
		const targets = installTargetsFor(skill);
		if (targets.length === 0) return;
		void postSkill(
			'/api/magician/v2/skills/install',
			{ source: `skillshub:${skill.name}`, target: { workspaces: targets } },
			`${skill.name}:install`,
			() => `Installed into ${targets.join(', ')}`
		);
	}

	function requestRemove(skill: CatalogEntry) {
		confirmRemove = confirmRemove === skill.name ? null : skill.name;
		removePurge = false;
		pendingPurge = null;
	}

	let pendingPurge: { name: string; token: string } | null = null;

	async function removeSkillFlow(skill: CatalogEntry) {
		const key = `${skill.name}:remove`;
		if (removePurge && pendingPurge?.name !== skill.name) {
			// First purge call asks for confirmation; the server returns the
			// token without deleting.
			const token = installToken.trim() || readStoredToken();
			if (!token) {
				requireToken(key);
				return;
			}
			actionInflight = { ...actionInflight, [key]: true };
			try {
				const res = await timedFetch(
					`/api/magician/v2/skills/${encodeURIComponent(skill.name)}/uninstall`,
					{
						method: 'POST',
						headers: {
							'Content-Type': 'application/json',
							'X-Magician-Setup-Token': token
						},
						body: JSON.stringify({ workspaces: [currentScope], purge: true })
					}
				);
				const data = await res.json().catch(() => null);
				if (!res.ok) {
					actionMessage = {
						...actionMessage,
						[key]: {
							kind: 'err',
							text:
								(data && (data.reason || data.message)) ||
								`server returned ${res.status}`
						}
					};
					return;
				}
				if (data?.confirm_token) {
					pendingPurge = { name: skill.name, token: data.confirm_token };
					actionMessage = {
						...actionMessage,
						[key]: {
							kind: 'ok',
							text: 'Purge preview ready — confirm to delete everything.'
						}
					};
				} else {
					pendingPurge = null;
					actionMessage = {
						...actionMessage,
						[key]: { kind: 'ok', text: (data?.targets ?? []).join(' · ') || 'Purged' }
					};
					await loadSkills();
				}
			} catch (e) {
				actionMessage = {
					...actionMessage,
					[key]: { kind: 'err', text: e instanceof Error ? e.message : String(e) }
				};
			} finally {
				actionInflight = { ...actionInflight, [key]: false };
			}
			return;
		}
		const body = removePurge
			? { workspaces: [currentScope], purge: true, confirm_token: pendingPurge?.token }
			: { workspaces: [currentScope] };
		const ok = await postSkill(
			`/api/magician/v2/skills/${encodeURIComponent(skill.name)}/uninstall`,
			body,
			key,
			(data) => (data?.targets ?? []).join(' · ') || (removePurge ? 'Purged' : 'Removed')
		);
		if (ok) {
			confirmRemove = null;
			pendingPurge = null;
		}
	}

	function openInstallForm() {
		installFormOpen = true;
		installError = null;
		installSuccess = null;
	}

	function closeInstallForm() {
		installFormOpen = false;
		installWorkspaces = '';
	}

	async function submitInstall() {
		installError = null;
		installSuccess = null;
		const source = installSource.trim();
		if (!source) {
			installError = 'Source is required (e.g. `path:/abs/path` or `skillshub:awk`).';
			return;
		}
		const workspaces = installWorkspaces
			.split(',')
			.map((s) => s.trim())
			.filter(Boolean);
		if (workspaces.length === 0) {
			installError = 'Pick at least one workspace target (e.g. anonymous/default).';
			return;
		}
		const token = installToken.trim();
		if (!token) {
			installError = 'Setup token is required (find it on /vault).';
			return;
		}

		installSubmitting = true;
		try {
			const res = await timedFetch('/api/magician/v2/skills/install', {
				method: 'POST',
				headers: {
					'Content-Type': 'application/json',
					'X-Magician-Setup-Token': token
				},
				body: JSON.stringify({ source, target: { workspaces } })
			});
			const data = await res.json().catch(() => null);
			if (!res.ok) {
				installError =
					(data && (data.reason || data.message)) || `server returned ${res.status}`;
				return;
			}
			const installedNames: string[] = data?.installed ?? [];
			installSuccess = `Installed ${installedNames.join(', ') || 'skill'}.`;
			persistToken(token);
			installSource = '';
			await loadSkills();
		} catch (e) {
			installError = e instanceof Error ? e.message : String(e);
		} finally {
			installSubmitting = false;
		}
	}

	async function loadAgents() {
		if (agentsLoaded) return;
		try {
			const res = await timedFetch('/api/magician/v2/agents');
			if (!res.ok) {
				agents = [];
				return;
			}
			const data = await res.json().catch(() => null);
			// The list endpoint returns { agents: [{ definition: { agent_id,
			// tools, … }, version, … }] } (definitions nested, not flattened).
			const raw: any[] = Array.isArray(data) ? data : data?.agents ?? [];
			agents = raw
				.map((a: any) => ({
					agent_id: a.definition?.agent_id ?? a.agent_id ?? a.id ?? '',
					tools: Array.isArray(a.definition?.tools)
						? a.definition.tools
						: Array.isArray(a.tools)
							? a.tools
							: []
				}))
				.filter((a: AgentSummary) => a.agent_id !== '');
			agentsLoaded = true;
		} catch {
			agents = [];
		}
	}

	async function openAllowDropdown(skill: CatalogEntry) {
		openAllowFor = openAllowFor === skill.name ? null : skill.name;
		if (openAllowFor) await loadAgents();
	}

	async function toggleAgentAllowance(skill: CatalogEntry, agent: AgentSummary) {
		const has = agent.tools.includes(skill.name);
		const action = has ? 'remove' : 'add';
		const key = `${skill.name}:allow`;
		if (actionInflight[`${skill.name}:allow:${agent.agent_id}`]) return;
		const token = requireToken(key);
		if (!token) return;
		actionInflight = { ...actionInflight, [`${skill.name}:allow:${agent.agent_id}`]: true };
		try {
			const res = await timedFetch(
				`/api/magician/v2/skills/${encodeURIComponent(skill.name)}/allow-for-agent`,
				{
					method: 'POST',
					headers: {
						'Content-Type': 'application/json',
						'X-Magician-Setup-Token': token
					},
					// scope keeps the edit on the scoped definition the agent
					// actually runs — without it the handler edits the system
					// template copy.
					body: JSON.stringify({ agent_id: agent.agent_id, action, scope: currentScope })
				}
			);
			const data = await res.json().catch(() => null);
			if (!res.ok) {
				const reason =
					(data && (data.reason || data.message)) || `server returned ${res.status}`;
				actionMessage = { ...actionMessage, [key]: { kind: 'err', text: reason } };
			} else {
				const newTools = Array.isArray(data?.tools) ? data.tools : agent.tools;
				agents = agents.map((a) =>
					a.agent_id === agent.agent_id ? { ...a, tools: newTools } : a
				);
				actionMessage = {
					...actionMessage,
					[key]: {
						kind: 'ok',
						text:
							action === 'add'
								? `Allowed for ${agent.agent_id}`
								: `Removed from ${agent.agent_id}`
					}
				};
			}
		} catch (e) {
			actionMessage = {
				...actionMessage,
				[key]: { kind: 'err', text: e instanceof Error ? e.message : String(e) }
			};
		} finally {
			actionInflight = { ...actionInflight, [`${skill.name}:allow:${agent.agent_id}`]: false };
		}
	}
</script>

<svelte:head>
	<title>Skills · Magican</title>
</svelte:head>

<div class="skills-page">
	<header class="skills-header">
		<div>
			<h1>Skills</h1>
			<p class="subhead">
				Every skill in the system — skillshub source, extras, scope installs, and built-ins.
				Install into scopes, remove, and manage agent allowances.
			</p>
		</div>
		<div class="header-actions">
			<button class="evolution-btn" on:click={() => goto('/skills/evolution')} type="button">
				Evolution →
			</button>
			<button class="reload-btn" on:click={loadSkills} disabled={loading}>
				{loading ? 'Loading…' : 'Reload'}
			</button>
			<button class="install-btn" on:click={openInstallForm} type="button">
				+ Install from path
			</button>
		</div>
	</header>

	{#if installFormOpen}
		<form class="install-form" on:submit|preventDefault={submitInstall}>
			<div class="install-form-row">
				<label for="install-source">Source</label>
				<input
					id="install-source"
					type="text"
					bind:value={installSource}
					placeholder="path:/abs/path/to/skill  OR  skillshub:awk"
					disabled={installSubmitting}
				/>
			</div>
			<div class="install-form-row">
				<label for="install-workspaces">Target workspaces</label>
				<input
					id="install-workspaces"
					type="text"
					bind:value={installWorkspaces}
					placeholder="anonymous/default, alice/workspace"
					disabled={installSubmitting}
				/>
			</div>
			<div class="install-form-row">
				<label for="install-token">Setup token</label>
				<input
					id="install-token"
					type="password"
					bind:value={installToken}
					placeholder="X-Magician-Setup-Token (find on /vault)"
					disabled={installSubmitting}
					autocomplete="off"
				/>
			</div>
			{#if installError}
				<div class="install-error">{installError}</div>
			{/if}
			{#if installSuccess}
				<div class="install-success">{installSuccess}</div>
			{/if}
			<div class="install-form-actions">
				<button type="button" on:click={closeInstallForm} disabled={installSubmitting}>
					Close
				</button>
				<button type="submit" class="primary" disabled={installSubmitting}>
					{installSubmitting ? 'Installing…' : 'Install'}
				</button>
			</div>
		</form>
	{/if}

	<div class="tab-row">
		<div class="tabs" role="tablist" aria-label="Skill kinds">
			{#each TAB_ORDER as tab (tab)}
				<button
					id={`skills-tab-${tab}`}
					type="button"
					class="tab"
					class:active={activeTab === tab}
					role="tab"
					aria-selected={activeTab === tab}
					on:click={() => selectTab(tab)}
				>
					{TAB_LABELS[tab]}
					{#if tab !== 'environment'}<span class="count">{counts[tab]}</span>{/if}
				</button>
			{/each}
		</div>
		<input
			type="search"
			placeholder="Filter by name or description…"
			bind:value={textFilter}
			class="text-filter"
		/>
	</div>

	{#if activeTab === 'environment'}
		<SkillsEnvironmentPanel />
	{:else if error}
		<div class="error">Error loading the catalog: {error}</div>
	{:else if loading && skills.length === 0}
		<div class="empty">Loading…</div>
	{:else if filteredSkills.length === 0}
		<div class="empty">
			{skills.length === 0 ? 'No skills found on this system.' : 'No skills match the current filter.'}
		</div>
	{:else}
		<ul class="skill-list">
			{#each filteredSkills as skill (skill.name)}
				{@const here = installedHere(skill)}
				{@const installable =
						skill.origin === 'skillshub' && installTargetsFor(skill).length > 0}
				<li class="skill-row">
					<div class="skill-name-row">
						<span class="skill-name">{skill.name}</span>
						<span class="badge badge-kind">{skill.kind}</span>
						<span class="badge badge-origin" title="Where this skill lives">{skill.origin}</span>
						{#if here}
							<span class="badge badge-installed" title={skill.installed_scopes.join(', ')}>
								{installedBadge(skill)}
							</span>
						{:else if skill.installed_scopes.length > 0}
							<span class="badge badge-elsewhere" title={skill.installed_scopes.join(', ')}>
								{elsewhereBadge(skill)}
							</span>
						{/if}
						<div class="row-actions">
							{#if skill.kind !== 'compiled'}
								{#if installable}
									<button
										type="button"
										class="row-action"
										on:click={() => installSkill(skill)}
										disabled={actionInflight[`${skill.name}:install`]}
										title="Install into {installTargetsFor(skill).join(', ')}"
									>
										{actionInflight[`${skill.name}:install`] ? '…' : 'Install'}
									</button>
								{/if}
								{#if here}
									{#if confirmRemove === skill.name}
										<label class="purge-check" title="Also delete the retained config, auth and state">
											<input type="checkbox" bind:checked={removePurge} />
											purge
										</label>
										<button
											type="button"
											class="row-action row-action--danger"
											on:click={() => removeSkillFlow(skill)}
											disabled={actionInflight[`${skill.name}:remove`]}
										>
											{actionInflight[`${skill.name}:remove`]
												? '…'
												: removePurge && pendingPurge?.name !== skill.name
													? 'Confirm purge'
													: removePurge
														? 'Delete everything'
														: 'Confirm remove'}
										</button>
										<button
											type="button"
											class="row-action"
											on:click={() => (confirmRemove = null)}
										>
											Cancel
										</button>
									{:else}
										<button
											type="button"
											class="row-action row-action--danger"
											on:click={() => requestRemove(skill)}
											title="Remove from {currentScope} (keeps the skill's config, auth and state)"
										>
											Remove
										</button>
									{/if}
								{/if}
								{#if skill.kind === 'procedure' || skill.kind === 'tool'}
									<button
										type="button"
										class="row-action"
										on:click={() => openAllowDropdown(skill)}
										title="Add this skill to an agent's tools allowlist"
									>
										Allow for agent ▾
									</button>
								{/if}
								{#if here}
									<button
										type="button"
										class="row-action"
										on:click={() => (openEnvFor = openEnvFor === skill.name ? null : skill.name)}
										title="Edit this skill's config/.env in {currentScope}"
									>
										Env ▾
									</button>
								{/if}
							{/if}
						</div>
					</div>
					{#if openAllowFor === skill.name}
						<div class="agent-picker">
							{#if agents.length === 0}
								<div class="agent-picker-empty">No agent definitions found.</div>
							{:else}
								{#each agents as agent (agent.agent_id)}
									{@const allowed = agent.tools.includes(skill.name)}
									{@const inflightKey = `${skill.name}:allow:${agent.agent_id}`}
									<button
										type="button"
										class="agent-picker-row"
										class:agent-picker-row--allowed={allowed}
										on:click={() => toggleAgentAllowance(skill, agent)}
										disabled={actionInflight[inflightKey]}
									>
										<span class="agent-picker-check">
											{actionInflight[inflightKey] ? '…' : allowed ? '✓' : '+'}
										</span>
										<span class="agent-picker-name">{agent.agent_id}</span>
										<span class="agent-picker-count">{agent.tools.length} tools</span>
									</button>
								{/each}
							{/if}
						</div>
					{/if}
					{#if openEnvFor === skill.name}
						<SkillEnvEditor skill={skill.name} scope={currentScope} />
					{/if}
					<p class="skill-desc">{skill.description}</p>
					{#if skill.requires_bins.length > 0 || skill.requires_env.length > 0}
						<div class="skill-requires">
							{#if skill.requires_bins.length > 0}
								<span class="req-label">bins:</span>
								{#each skill.requires_bins as bin}
									<code class="req-pill">{bin}</code>
								{/each}
							{/if}
							{#if skill.requires_env.length > 0}
								<span class="req-label">env:</span>
								{#each skill.requires_env as env}
									<code class="req-pill">{env}</code>
								{/each}
							{/if}
						</div>
					{/if}
					{#if actionMessage[`${skill.name}:install`]}
						<div class="row-msg row-msg--{actionMessage[`${skill.name}:install`].kind}">
							{actionMessage[`${skill.name}:install`].text}
						</div>
					{/if}
					{#if actionMessage[`${skill.name}:remove`]}
						<div class="row-msg row-msg--{actionMessage[`${skill.name}:remove`].kind}">
							{actionMessage[`${skill.name}:remove`].text}
						</div>
					{/if}
					{#if actionMessage[`${skill.name}:allow`]}
						<div class="row-msg row-msg--{actionMessage[`${skill.name}:allow`].kind}">
							{actionMessage[`${skill.name}:allow`].text}
						</div>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</div>

<style>
	.skills-page {
		width: 100%;
		max-width: var(--app-content-max, 1320px);
		margin: 0 auto;
		padding: 1rem;
	}

	.skills-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 1rem;
		margin-bottom: 1rem;
	}

	.skills-header h1 {
		margin: 0;
		font-size: 1.5rem;
		font-weight: 600;
	}

	.subhead {
		margin: 0.25rem 0 0;
		font-size: 0.85rem;
		color: var(--text-muted);
	}

	.header-actions {
		display: flex;
		gap: 0.5rem;
	}

	.reload-btn,
	.install-btn,
	.evolution-btn {
		padding: 0.4rem 0.8rem;
		font-size: 0.85rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.4rem;
		background: var(--bg-card);
		color: var(--text-primary);
		cursor: pointer;
		white-space: nowrap;
	}

	.install-btn {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--text-on-accent, #fff);
	}

	.evolution-btn {
		background: var(--bg-elevated, var(--bg-card));
		color: var(--accent-primary);
		border-color: var(--accent-primary);
	}

	.evolution-btn:hover {
		background: var(--accent-primary);
		color: var(--text-on-accent, #fff);
	}

	.reload-btn:disabled,
	.install-btn:disabled,
	.evolution-btn:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.install-form {
		margin-bottom: 1rem;
		padding: 1rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.5rem;
		background: var(--bg-soft, var(--bg-card));
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}

	.install-form-row {
		display: flex;
		flex-direction: column;
		gap: 0.25rem;
	}

	.install-form-row label {
		font-size: 0.75rem;
		text-transform: uppercase;
		letter-spacing: 0.04em;
		color: var(--text-muted);
	}

	.install-form-row input[type='text'],
	.install-form-row input[type='password'] {
		padding: 0.4rem 0.6rem;
		font-size: 0.85rem;
		font-family: var(--font-mono);
		border: 1px solid var(--border-soft);
		border-radius: 0.4rem;
		background: var(--bg-card);
		color: var(--text-primary);
	}

	.install-error {
		padding: 0.4rem 0.6rem;
		border-radius: 0.4rem;
		background: var(--surface-danger, rgba(204, 51, 51, 0.1));
		color: var(--text-danger, #c33);
		font-size: 0.8rem;
	}

	.install-success {
		padding: 0.4rem 0.6rem;
		border-radius: 0.4rem;
		background: var(--surface-success, rgba(40, 160, 80, 0.1));
		color: var(--text-success, #2a8a4a);
		font-size: 0.8rem;
	}

	.install-form-actions {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
	}

	.install-form-actions button {
		padding: 0.4rem 0.9rem;
		font-size: 0.85rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.4rem;
		background: var(--bg-card);
		color: var(--text-primary);
		cursor: pointer;
	}

	.install-form-actions button.primary {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--text-on-accent, #fff);
	}

	.install-form-actions button:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.tab-row {
		display: flex;
		justify-content: space-between;
		align-items: center;
		gap: 1rem;
		margin-bottom: 0.75rem;
	}

	.tabs {
		display: flex;
		gap: 0.25rem;
		flex-wrap: wrap;
	}

	.tab {
		padding: 0.35rem 0.8rem;
		font-size: 0.85rem;
		border: 1px solid var(--border-soft);
		border-bottom: none;
		border-radius: 0.4rem 0.4rem 0 0;
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		cursor: pointer;
	}

	.tab.active {
		background: var(--accent-primary);
		color: var(--text-on-accent, #fff);
		border-color: var(--accent-primary);
	}

	.tab .count {
		opacity: 0.65;
		margin-left: 0.25rem;
		font-size: 0.75rem;
	}

	.text-filter {
		flex: 1;
		max-width: 18rem;
		padding: 0.4rem 0.6rem;
		font-size: 0.85rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.4rem;
		background: var(--bg-card);
		color: var(--text-primary);
	}

	.error,
	.empty {
		padding: 1.5rem;
		text-align: center;
		color: var(--text-muted);
	}

	.error {
		color: var(--text-danger, #c33);
	}

	.skill-list {
		list-style: none;
		padding: 0;
		margin: 0;
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
	}

	.skill-row {
		padding: 0.75rem 1rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.5rem;
		background: var(--bg-card);
	}

	.skill-name-row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		margin-bottom: 0.3rem;
		flex-wrap: wrap;
	}

	.skill-name {
		font-family: var(--font-mono);
		font-size: 0.95rem;
		font-weight: 600;
	}

	.badge {
		font-size: 0.7rem;
		padding: 0.1rem 0.4rem;
		border-radius: 0.25rem;
		border: 1px solid var(--border-soft);
		background: var(--bg-soft, transparent);
		color: var(--text-muted);
	}

	.badge-kind {
		text-transform: capitalize;
	}

	.badge-installed {
		border-color: var(--accent-primary);
		color: var(--accent-primary);
	}

	.badge-elsewhere {
		color: var(--text-muted);
	}

	.skill-desc {
		margin: 0.3rem 0 0;
		font-size: 0.85rem;
		color: var(--text-primary);
		line-height: 1.45;
	}

	.skill-requires {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.3rem;
		margin-top: 0.4rem;
		font-size: 0.75rem;
	}

	.req-label {
		color: var(--text-muted);
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.req-pill {
		padding: 0.05rem 0.4rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.25rem;
		font-size: 0.75rem;
		background: var(--bg-soft, transparent);
	}

	.row-actions {
		margin-left: auto;
		display: flex;
		gap: 0.4rem;
		align-items: center;
	}

	.row-action {
		padding: 0.2rem 0.5rem;
		font-size: 0.75rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.3rem;
		background: var(--bg-soft, var(--bg-card));
		color: var(--text-primary);
		cursor: pointer;
		white-space: nowrap;
	}

	.row-action:hover:not(:disabled) {
		background: var(--accent-primary);
		border-color: var(--accent-primary);
		color: var(--text-on-accent, #fff);
	}

	.row-action--danger:hover:not(:disabled) {
		background: var(--text-danger, #c33);
		border-color: var(--text-danger, #c33);
	}

	.row-action:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.purge-check {
		display: inline-flex;
		align-items: center;
		gap: 0.25rem;
		font-size: 0.75rem;
		color: var(--text-danger, #c33);
		cursor: pointer;
		white-space: nowrap;
	}

	.purge-check input {
		margin: 0;
	}

	.row-msg {
		margin-top: 0.4rem;
		padding: 0.3rem 0.5rem;
		border-radius: 0.3rem;
		font-size: 0.75rem;
	}

	.row-msg--ok {
		background: var(--surface-success, rgba(40, 160, 80, 0.1));
		color: var(--text-success, #2a8a4a);
	}

	.row-msg--err {
		background: var(--surface-danger, rgba(204, 51, 51, 0.1));
		color: var(--text-danger, #c33);
	}

	.agent-picker {
		margin-top: 0.5rem;
		padding: 0.4rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.4rem;
		background: var(--bg-card);
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		max-height: 14rem;
		overflow-y: auto;
	}

	.agent-picker-empty {
		padding: 0.5rem;
		font-size: 0.8rem;
		color: var(--text-muted);
		text-align: center;
	}

	.agent-picker-row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.3rem 0.5rem;
		border: 1px solid var(--border-soft);
		border-radius: 0.3rem;
		background: transparent;
		color: var(--text-primary);
		font-size: 0.8rem;
		cursor: pointer;
		text-align: left;
	}

	.agent-picker-row:hover:not(:disabled) {
		background: var(--bg-soft);
	}

	.agent-picker-row--allowed {
		border-color: var(--accent-primary);
		background: var(--accent-primary-soft);
	}

	.agent-picker-row:disabled {
		opacity: 0.6;
		cursor: not-allowed;
	}

	.agent-picker-check {
		width: 1.2rem;
		text-align: center;
		font-family: var(--font-mono, ui-monospace, monospace);
		font-weight: 600;
	}

	.agent-picker-name {
		flex: 1;
		font-family: var(--font-mono, ui-monospace, monospace);
	}

	.agent-picker-count {
		font-size: 0.7rem;
		color: var(--text-muted);
	}
</style>
