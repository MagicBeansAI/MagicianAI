<!-- The three-promise pillars used to open this file: "A superintelligence
     with one user", then Knows you / Acts for you / Belongs to you as three
     cards. The promise ROAD now spends nine scroll-scrubbed stations making
     exactly those three arguments, each titled in turn, immediately above
     this. Restating them as a static summary directly afterwards asked the
     visitor to read the conclusion of something they had just watched.

     What remains is the part the road does NOT cover: who the thing answers
     to, and the three concrete mechanisms behind that. -->

<section class="tr" aria-label="Trust">
	<!-- Capability and the three promises now arrive first. This section can
	     therefore answer the natural objection directly: power this personal
	     is acceptable only when its secrets, boundaries and actions answer to
	     the person it serves. -->
	<p class="tr-restore">Power this personal must answer to you.</p>
	<p class="tr-lede">
		A system this personal will know your messages, your money, your plans. That only works if it
		answers to exactly one party: you. Not your employer, an advertiser, or an AI company.
	</p>

	<div class="tr-beats">
		<article class="tr-beat">
			<h4>The vault</h4>
			<div class="tr-login" aria-hidden="true">
				<div class="tr-login-row"><span>email</span><span class="tr-mono">you@…</span></div>
				<div class="tr-login-row">
					<span>password</span><span class="tr-mono tr-secret">••••••••</span>
				</div>
				<div class="tr-login-tag">the model never saw this</div>
			</div>
			<p>
				Passwords and codes stay sealed. The runtime inserts them at the moment of action; the
				model works with placeholders it can never read.
			</p>
		</article>

		<article class="tr-beat">
			<h4>Models you choose</h4>
			<div class="tr-boundary" aria-hidden="true">
				<span class="tr-packet tr-mono">operation</span>
				<span class="tr-wall"></span>
				<span class="tr-device tr-mono">your model choice</span>
			</div>
			<p>
				Choose local or hosted models for individual operations in Settings. Local models have
				limits; demanding work may need more capability. Our principle: you decide what stays
				private and what context you share for that capability.
			</p>
		</article>

		<article class="tr-beat">
			<h4>Your hand on the wheel</h4>
			<div class="tr-toggles" aria-hidden="true">
				<div class="tr-toggle on"><span></span> ask before spending</div>
				<div class="tr-toggle on"><span></span> show every plan before it runs</div>
				<div class="tr-toggle on"><span></span> keep a receipt of every action</div>
			</div>
			<p>
				Plans are visible and editable. Spending waits for you. Every run leaves a receipt you can
				open.
			</p>
		</article>
	</div>
</section>

<style>
	.tr {
		--tr-primary: var(--accent-primary, #9e59ff);
		--tr-secondary: var(--accent-secondary, var(--tr-primary));
		--tr-bridge: color-mix(in srgb, var(--tr-primary) 48%, var(--tr-secondary));
		--tr-halo: color-mix(in srgb, var(--tr-primary) 22%, transparent);
		max-width: 64rem;
		margin: 0 auto;
		padding: clamp(3rem, 8vh, 5.5rem) clamp(1rem, 4vw, 2.5rem);
		padding-top: clamp(2rem, 5vh, 3.5rem);
		font-family: var(--lp-font);
		color: var(--text-primary);
	}
	.tr-restore {
		font-size: clamp(1.7rem, 4vw, 2.6rem);
		font-weight: 420;
		letter-spacing: -0.015em;
		margin: 0 0 0.8rem;
		background: linear-gradient(
			95deg,
			var(--text-primary),
			var(--tr-primary) 48%,
			var(--tr-secondary) 66%,
			var(--text-primary)
		);
		-webkit-background-clip: text;
		background-clip: text;
		color: transparent;
	}
	.tr-lede {
		color: var(--landing-subtitle);
		max-width: 44rem;
		font-size: clamp(0.98rem, 1.8vw, 1.12rem);
		line-height: 1.55;
		margin: 0 0 2.2rem;
	}
	.tr-beats {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(15rem, 1fr));
		gap: 1rem;
	}
	.tr-beat {
		border: 1px solid var(--border-default, color-mix(in srgb, var(--text-primary) 14%, transparent));
		background: color-mix(in srgb, var(--bg-elevated, var(--landing-bg)) 88%, transparent);
		box-shadow: var(--shadow-md, var(--landing-task-card-shadow));
		border-radius: var(--radius-lg, 16px);
		padding: 1.2rem 1.3rem;
	}
	.tr-beat h4 {
		margin: 0 0 0.7rem;
		font-size: 1rem;
		letter-spacing: -0.01em;
	}
	.tr-beat p {
		color: var(--landing-subtitle);
		font-size: 0.9rem;
		line-height: 1.5;
		margin: 0.8rem 0 0;
	}
	.tr-mono {
		font-family: var(--lp-mono);
		font-size: 0.85em;
	}
	.tr-login {
		display: grid;
		gap: 0.4rem;
		border: 1px solid var(--border-soft, var(--landing-form-border));
		background: var(--bg-soft, var(--landing-input-surface));
		border-radius: var(--radius-md, 10px);
		padding: 0.7rem 0.8rem;
	}
	.tr-login-row {
		display: flex;
		justify-content: space-between;
		font-size: 0.82rem;
		color: var(--landing-subtitle);
	}
	.tr-secret {
		color: var(--tr-primary);
		letter-spacing: 0.14em;
	}
	.tr-login-tag {
		font-family: var(--lp-mono);
		font-size: 0.66rem;
		color: var(--tr-primary);
		border-top: 1px dashed var(--landing-task-line);
		padding-top: 0.4rem;
	}
	.tr-boundary {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding: 0.9rem 0.4rem;
	}
	.tr-packet {
		font-size: 0.72rem;
		border: 1px solid var(--landing-chip-border);
		border-radius: 6px;
		padding: 0.2rem 0.5rem;
		color: var(--landing-subtitle);
	}
	.tr-wall {
		flex: 1;
		height: 0;
		border-top: 2px dashed var(--tr-halo);
		position: relative;
	}
	.tr-wall::after {
		content: '⇤';
		position: absolute;
		right: -2px;
		top: -0.75em;
		color: var(--tr-primary);
	}
	.tr-device {
		font-size: 0.72rem;
		color: var(--tr-primary);
		border: 1px solid var(--tr-halo);
		border-radius: 6px;
		padding: 0.2rem 0.5rem;
	}
	.tr-toggles {
		display: grid;
		gap: 0.45rem;
	}
	.tr-toggle {
		display: flex;
		align-items: center;
		gap: 0.55rem;
		font-size: 0.84rem;
		color: var(--text-primary);
	}
	.tr-toggle span {
		width: 26px;
		height: 15px;
		border-radius: 999px;
		background: var(--tr-primary);
		position: relative;
		flex: none;
	}
	.tr-toggle span::after {
		content: '';
		position: absolute;
		right: 2px;
		top: 2px;
		width: 11px;
		height: 11px;
		border-radius: 50%;
		background: var(--text-on-accent, white);
	}
</style>
