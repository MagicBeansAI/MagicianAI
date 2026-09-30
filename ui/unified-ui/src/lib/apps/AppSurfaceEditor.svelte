<script lang="ts">
	import { createEventDispatcher } from 'svelte';
	import type { AppSurfaceFieldBinding } from './appSurfaceRuntime';
	import { coerceAppSurfaceForm, formatAppSurfaceTimestampInput } from './appSurfaceRuntime';

	export let fields: AppSurfaceFieldBinding[] = [];
	export let initialValues: Record<string, unknown> = {};
	export let mode: 'create' | 'edit' = 'create';
	export let saving = false;
	export let allowDelete = false;
	/** Embedded declarative forms stay mounted and omit modal-style close controls. */
	export let embedded = false;

	const dispatch = createEventDispatcher<{
		save: { values: Record<string, unknown> };
		cancel: void;
		delete: void;
	}>();
	let validationError = '';

	function label(field: string): string {
		return field
			.split(/[_-]+/g)
			.filter(Boolean)
			.map((part) => part.charAt(0).toUpperCase() + part.slice(1))
			.join(' ');
	}

	function inputValue(field: AppSurfaceFieldBinding): string {
		const value = initialValues[field.field];
		if (value == null) return '';
		if (field.kind === 'timestamp') return formatAppSurfaceTimestampInput(value);
		return String(value);
	}

	function submit(event: SubmitEvent): void {
		validationError = '';
		const form = event.currentTarget as HTMLFormElement;
		const formData = new FormData(form);
		const raw: Record<string, string | boolean> = {};
		for (const field of fields) {
			raw[field.field] = field.kind === 'boolean' && !field.nullable
				? formData.has(field.field)
				: String(formData.get(field.field) ?? '');
		}
		try {
			dispatch('save', { values: coerceAppSurfaceForm(fields, raw) });
		} catch (error) {
			validationError = error instanceof Error ? error.message : 'Please check the form.';
		}
	}
</script>

<section class="app-surface-editor" aria-label={mode === 'create' ? 'Create record' : 'Edit record'}>
	<header>
		<div>
			<p>{mode === 'create' ? 'New record' : 'Edit record'}</p>
			<span>Changes are saved with optimistic revision checks.</span>
		</div>
		{#if !embedded}<button type="button" class="quiet" disabled={saving} on:click={() => dispatch('cancel')}>Close</button>{/if}
	</header>

	<form on:submit|preventDefault={submit}>
		<div class="fields">
			{#each fields as field (field.field)}
				<label class:wide={field.kind === 'markdown'}>
					<span>{label(field.field)}{field.required ? ' *' : ''}</span>
					{#if field.kind === 'markdown'}
						<textarea name={field.field} rows="5" disabled={saving}>{inputValue(field)}</textarea>
					{:else if field.kind === 'enum'}
						<select name={field.field} disabled={saving} required={field.required && !field.nullable} value={inputValue(field)}>
							{#if field.nullable || !field.required}<option value="">None</option>{/if}
							{#each field.allowedValues as option}
								<option value={option}>{label(option)}</option>
							{/each}
						</select>
					{:else if field.kind === 'boolean' && field.nullable}
						<select name={field.field} disabled={saving} value={inputValue(field)}>
							<option value="">None</option>
							<option value="true">Yes</option>
							<option value="false">No</option>
						</select>
					{:else if field.kind === 'boolean'}
						<input class="checkbox" type="checkbox" name={field.field} disabled={saving} checked={initialValues[field.field] === true} />
					{:else}
						<input
							name={field.field}
							type={field.kind === 'integer' || field.kind === 'decimal' ? 'number' : field.kind === 'timestamp' ? 'datetime-local' : 'text'}
							step={field.kind === 'integer' ? '1' : field.kind === 'decimal' ? 'any' : field.kind === 'timestamp' ? '0.001' : undefined}
							value={inputValue(field)}
							required={field.required && !field.nullable}
							disabled={saving}
							autocomplete="off"
						/>
					{/if}
				</label>
			{/each}
		</div>
		{#if validationError}<p class="error" role="alert">{validationError}</p>{/if}
		<footer>
			{#if allowDelete}<button type="button" class="danger" disabled={saving} on:click={() => dispatch('delete')}>Delete</button>{/if}
			<span class="footer-spacer"></span>
			{#if !embedded}<button type="button" class="quiet" disabled={saving} on:click={() => dispatch('cancel')}>Cancel</button>{/if}
			<button type="submit" class="primary" disabled={saving}>{saving ? 'Saving…' : mode === 'create' ? 'Create' : 'Save'}</button>
		</footer>
	</form>
</section>

<style>
	.app-surface-editor { border: 1px solid var(--border-soft); border-radius: var(--radius-xl, 18px); background: var(--bg-card); box-shadow: var(--shadow-lg); overflow: hidden; }
	header, footer { display: flex; align-items: center; justify-content: space-between; gap: 1rem; padding: 1rem 1.15rem; }
	header { border-bottom: 1px solid var(--border-soft); }
	header p { margin: 0; color: var(--text-primary); font-weight: 700; }
	header span { color: var(--text-muted); font-size: .82rem; }
	form { padding: 1rem 1.15rem; }
	.fields { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: .9rem; }
	label { display: grid; gap: .35rem; color: var(--text-secondary); font-size: .82rem; font-weight: 600; }
	label.wide { grid-column: 1 / -1; }
	input, select, textarea { width: 100%; box-sizing: border-box; border: 1px solid var(--border-soft); border-radius: var(--radius-md, 10px); background: var(--bg-soft); color: var(--text-primary); padding: .65rem .72rem; font: inherit; }
	textarea { resize: vertical; }
	input:focus, select:focus, textarea:focus { outline: 2px solid color-mix(in srgb, var(--accent-primary) 40%, transparent); border-color: var(--accent-primary); }
	.checkbox { width: 1.15rem; height: 1.15rem; margin-top: .55rem; }
	footer { justify-content: flex-end; padding: 1rem 0 0; }
	button { border-radius: var(--radius-full, 999px); padding: .55rem .9rem; font: inherit; font-weight: 650; cursor: pointer; }
	button:disabled { opacity: .55; cursor: default; }
	button.quiet { border: 1px solid var(--border-soft); background: transparent; color: var(--text-secondary); }
	button.primary { border: 1px solid transparent; background: var(--accent-primary); color: var(--text-on-accent, #fff); }
	button.danger { border: 1px solid color-mix(in srgb, var(--color-error, #c43d4d) 35%, var(--border-soft)); background: transparent; color: var(--color-error, #c43d4d); }
	.footer-spacer { flex: 1; }
	.error { margin: .8rem 0 0; color: var(--color-error, #c43d4d); font-size: .84rem; }
	@media (max-width: 720px) { .fields { grid-template-columns: 1fr; } label.wide { grid-column: auto; } header { align-items: flex-start; } }
</style>
