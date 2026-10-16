<script lang="ts">
	import { createEventDispatcher, onDestroy } from 'svelte';
	import swaggerBundleUrl from 'swagger-ui-dist/swagger-ui-bundle.js?url';
	import swaggerCssUrl from 'swagger-ui-dist/swagger-ui.css?url';

	export let open: boolean = false;
	export let originKey: string = '';
	export let originUrl: string = '';

	const dispatch = createEventDispatcher<{ close: void }>();

	let swaggerContainer: HTMLDivElement;
	let swaggerUiLoaded = false;

	type SwaggerUiBundle = (options: {
		domNode: HTMLDivElement;
		url: string;
		supportedSubmitMethods: string[];
		docExpansion: string;
		defaultModelsExpandDepth: number;
	}) => void;

	function swaggerWindow(): Window & { SwaggerUIBundle?: SwaggerUiBundle } {
		return window as Window & { SwaggerUIBundle?: SwaggerUiBundle };
	}

	$: if (open && swaggerContainer && originKey) {
		loadSwagger();
	}

	async function loadSwagger() {
		try {
			await Promise.all([loadSwaggerCSS(), loadSwaggerScript()]);
			const SwaggerUIBundle = swaggerWindow().SwaggerUIBundle;
			if (!SwaggerUIBundle) {
				throw new Error('Swagger UI bundle did not initialize');
			}

			// Clear previous content
			if (swaggerContainer) {
				swaggerContainer.innerHTML = '';
			}

			SwaggerUIBundle({
				domNode: swaggerContainer,
				url: `/api/magician/v2/api-mining/openapi/${originKey}`,
				supportedSubmitMethods: [], // Disable "Try it out" — replay goes through our pipeline
				docExpansion: 'list',
				defaultModelsExpandDepth: 1
			});

			swaggerUiLoaded = true;
		} catch (err) {
			console.error('Failed to load Swagger UI:', err);
			if (swaggerContainer) {
				swaggerContainer.textContent = '';
				const p = document.createElement('p');
				p.className = 'swagger-error';
				p.textContent = `Failed to load API documentation. ${err instanceof Error ? err.message : String(err)}`;
				swaggerContainer.appendChild(p);
			}
		}
	}

	async function loadSwaggerCSS() {
		// Only inject the stylesheet once
		if (document.querySelector('link[data-swagger-css]')) return;
		const link = document.createElement('link');
		link.rel = 'stylesheet';
		link.dataset.swaggerCss = 'true';
		link.href = swaggerCssUrl;
		document.head.appendChild(link);

		// Wait for CSS to load before rendering Swagger UI
		await new Promise<void>((resolve, reject) => {
			link.onload = () => resolve();
			link.onerror = () => reject(new Error('Failed to load swagger-ui CSS'));
		});
	}

	async function loadSwaggerScript() {
		if (swaggerWindow().SwaggerUIBundle) return;

		const existing = document.querySelector<HTMLScriptElement>('script[data-swagger-js]');
		if (existing) {
			await new Promise<void>((resolve, reject) => {
				existing.addEventListener('load', () => resolve(), { once: true });
				existing.addEventListener('error', () => reject(new Error('Failed to load Swagger UI bundle')), {
					once: true
				});
			});
			return;
		}

		const script = document.createElement('script');
		script.src = swaggerBundleUrl;
		script.async = true;
		script.dataset.swaggerJs = 'true';
		document.body.appendChild(script);

		await new Promise<void>((resolve, reject) => {
			script.onload = () => resolve();
			script.onerror = () => reject(new Error('Failed to load Swagger UI bundle'));
		});
	}

	function handleClose() {
		open = false;
		dispatch('close');
	}

	function handleBackdropClick(event: MouseEvent) {
		if (event.target === event.currentTarget) {
			handleClose();
		}
	}

	onDestroy(() => {
		swaggerUiLoaded = false;
	});
</script>

{#if open}
	<div class="swagger-modal-backdrop" role="presentation" on:click={handleBackdropClick}>
		<div
			class="swagger-modal"
			role="dialog"
			aria-modal="true"
			aria-labelledby="swagger-modal-title"
			tabindex="-1"
		>
			<header class="swagger-modal-header">
				<div>
					<h2 id="swagger-modal-title">OpenAPI Spec</h2>
					<p>{originUrl || originKey}</p>
				</div>
				<button class="swagger-close" type="button" aria-label="Close OpenAPI spec" on:click={handleClose}>
					x
				</button>
			</header>
			<div class="swagger-modal-content">
				<div bind:this={swaggerContainer} class="swagger-container"></div>
				{#if !swaggerUiLoaded && open}
					<div class="swagger-loading">Loading API documentation...</div>
				{/if}
			</div>
		</div>
	</div>
{/if}

<style>
	.swagger-modal-backdrop {
		align-items: flex-start;
		background: color-mix(in srgb, var(--bg-base, #000) 44%, transparent);
		display: flex;
		inset: 0;
		justify-content: center;
		overflow: auto;
		padding: 2rem 1rem;
		position: fixed;
		z-index: 1000;
	}

	.swagger-modal {
		background: var(--bg-card);
		border: 1px solid var(--border-soft);
		border-radius: 8px;
		box-shadow: var(--shadow-lg, 0 18px 48px rgb(0 0 0 / 0.22));
		color: var(--text-primary);
		display: grid;
		gap: 1rem;
		min-height: min(86vh, 900px);
		padding: 1rem;
		width: min(100%, 1180px);
	}

	.swagger-modal-header {
		align-items: flex-start;
		border-bottom: 1px solid var(--border-soft);
		display: flex;
		gap: 1rem;
		justify-content: space-between;
		padding-bottom: 0.75rem;
	}

	.swagger-modal-header h2,
	.swagger-modal-header p {
		letter-spacing: 0;
		margin: 0;
	}

	.swagger-modal-header h2 {
		font-size: 1.1rem;
		line-height: 1.25;
	}

	.swagger-modal-header p {
		color: var(--text-secondary);
		font-size: 0.84rem;
		line-height: 1.45;
		overflow-wrap: anywhere;
	}

	.swagger-close {
		align-items: center;
		background: transparent;
		border: 1px solid var(--border-soft);
		border-radius: 6px;
		color: var(--text-primary);
		cursor: pointer;
		display: inline-flex;
		font: inherit;
		font-size: 0.9rem;
		font-weight: 760;
		height: 2rem;
		justify-content: center;
		line-height: 1;
		width: 2rem;
	}

	.swagger-modal-content {
		min-height: 400px;
		position: relative;
	}

	.swagger-container {
		width: 100%;
	}

	.swagger-loading {
		position: absolute;
		top: 50%;
		left: 50%;
		transform: translate(-50%, -50%);
		color: var(--text-secondary, #6b7280);
		font-size: 14px;
	}

	/* Override swagger-ui defaults to fit our theme */
	.swagger-container :global(.swagger-ui) {
		font-family: inherit;
		color: var(--text-primary);
	}

	.swagger-container :global(.swagger-ui .wrapper),
	.swagger-container :global(.swagger-ui .opblock),
	.swagger-container :global(.swagger-ui .scheme-container),
	.swagger-container :global(.swagger-ui section.models) {
		background: var(--bg-card);
		border-color: var(--border-soft);
		box-shadow: none;
	}

	.swagger-container :global(.swagger-ui .topbar) {
		display: none;
	}

	.swagger-container :global(.swagger-ui .info) {
		margin: 20px 0;
	}

	.swagger-container :global(.swagger-ui .scheme-container) {
		padding: 10px 0;
	}

	:global(.swagger-error) {
		color: var(--text-danger, #ef4444);
		padding: 20px;
		text-align: center;
	}

	@media (max-width: 760px) {
		.swagger-modal-backdrop {
			padding: 1rem 0.75rem;
		}

		.swagger-modal-header {
			flex-direction: column;
		}

		.swagger-close {
			width: 100%;
		}
	}
</style>
