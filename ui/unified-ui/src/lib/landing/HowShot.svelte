<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import Icon from '$lib/shared/icons/Icon.svelte';
	import { motionEnabled } from '$lib/motion';
	import type { HowClaimId } from './howTrack';
	import { howShotFrames, shotCycleIndex } from './howShot';

	export let kind: HowClaimId;
	/** Only the pinned station cycles. Inactive shots stay on their first frame. */
	export let active = false;

	$: frames = howShotFrames(kind);
	$: reduced = !$motionEnabled;
	$: cycling = mounted && active && visible && !reduced && frames.length > 1;

	let mounted = false;
	let visible = false;
	let elapsed = 0;
	let startedAt = 0;
	let timer: ReturnType<typeof setInterval> | null = null;

	function observeVisible(node: HTMLElement) {
		if (typeof IntersectionObserver === 'undefined') {
			visible = true;
			return;
		}
		const io = new IntersectionObserver(
			(entries) => {
				visible = entries[0]?.isIntersecting ?? false;
			},
			{ threshold: 0.35 }
		);
		io.observe(node);
		return {
			destroy() {
				io.disconnect();
			}
		};
	}

	function stop(): void {
		if (timer !== null) {
			clearInterval(timer);
			timer = null;
		}
		elapsed = 0;
	}

	function start(): void {
		if (timer !== null) return;
		startedAt = Date.now();
		elapsed = 0;
		timer = setInterval(() => {
			elapsed = Date.now() - startedAt;
		}, 80);
	}

	$: if (mounted) {
		if (cycling) start();
		else stop();
	}

	$: frameIndex = cycling ? shotCycleIndex(elapsed, frames.length) : 0;
	$: frame = frames[frameIndex] ?? frames[0];
	$: frameId = frame?.id ?? '';
	$: chrome = frame?.chrome ?? '';

	onMount(() => {
		mounted = true;
	});

	onDestroy(stop);
</script>

<div
	use:observeVisible
	class="float"
	class:reduce={reduced}
	class:bob={active && !reduced}
	data-kind={kind}
	data-frame={frameId}
	data-chrome={chrome}
	data-visible={visible ? '1' : '0'}
	aria-hidden="true"
>
	<div class="shot-scale">
	<div class="shot">
	<div class="chrome">
		<span></span><span></span><span></span>
		<em>{chrome}</em>
	</div>

	<div class="stage">
			<div class="frame">
				{#if frameId === 'today'}
					<div class="today">
						<div class="band">
							<header>
								<h3>For you</h3>
								<small>3</small>
							</header>
							<ul>
								<li>
									<div class="copy">
										<b>Reply to Priya</b>
										<span>Visa window · Thursday</span>
									</div>
									<em class="cta pri">Reply</em>
								</li>
								<li>
									<div class="copy">
										<b>Approve the Kyoto hold</b>
										<span>Fare dropped 12% · under your cap</span>
									</div>
									<em class="cta pri">Approve</em>
								</li>
								<li>
									<div class="copy">
										<b>Sign the tenancy pack</b>
										<span>Due Friday · waiting on you</span>
									</div>
									<em class="cta">Review</em>
								</li>
							</ul>
						</div>
						<div class="band muted">
							<header>
								<h3>Worth a look</h3>
								<small>2</small>
							</header>
							<ul>
								<li>
									<div class="copy">
										<b>Standup notes filed</b>
										<span>Yesterday · from Chat</span>
									</div>
									<em class="cta">Useful</em>
								</li>
								<li>
									<div class="copy">
										<b>NRT opened a cheaper cabin</b>
										<span>Still under $900</span>
									</div>
									<em class="cta">Open</em>
								</li>
							</ul>
						</div>
					</div>
				{:else if frameId === 'chat'}
					<div class="chat">
						<div class="thread">
							<div class="bubble you">Hold the Kyoto fare if it drops again.</div>
							<div class="bubble bot">
								Watching ANA — 12% under your cap. I’ll ping before I buy.
							</div>
							<div class="bubble you">And draft Priya in my tone.</div>
							<div class="bubble bot">Draft ready — short, same as last Thursday.</div>
						</div>
						<div class="composer">
							<div class="composer-row">
								<span>Message or describe a task</span>
								<svg class="mic" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
									<rect x="9" y="2" width="6" height="11" rx="3"></rect>
									<path d="M5 10a7 7 0 0 0 14 0"></path>
									<line x1="12" y1="17" x2="12" y2="22"></line>
								</svg>
							</div>
							<div class="composer-tools">
								<span class="seg"><b>Do</b> Accept Plan</span>
								<svg class="clip" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
									<path d="M21.44 11.05l-8.49 8.49a6 6 0 0 1-8.49-8.49l8.49-8.48a4 4 0 0 1 5.66 5.65l-8.49 8.49a2 2 0 0 1-2.83-2.83l7.78-7.78"></path>
								</svg>
								<em>Pi</em>
							</div>
						</div>
					</div>
				{:else if frameId === 'tasks'}
					<div class="list">
						<div class="row run">
							<b>Watch Kyoto fare</b>
							<em>Running</em>
						</div>
						<div class="row">
							<b>Draft Priya a reply</b>
							<em>Ready</em>
						</div>
						<div class="row wait">
							<b>Tenancy pack</b>
							<em>Needs you</em>
						</div>
						<div class="row done">
							<b>File visa notes</b>
							<em>Done</em>
						</div>
					</div>
				{:else if frameId === 'tiers'}
					<div class="tiers">
						<article>
							<small>User</small>
							<b>Fare cap $900 · short replies</b>
							<span>Stated · on this Mac</span>
						</article>
						<article>
							<small>Agent · Travel</small>
							<b>Watch ANA, ping before buying</b>
							<span>Working style · this agent only</span>
						</article>
						<article class="on">
							<small>Task episode</small>
							<b>Kyoto fare watch · Thursday</b>
							<span>One run, then synthesized up</span>
						</article>
					</div>
				{:else if frameId === 'synthesize'}
					<div class="synth">
						<div class="node">
							<small>Completed task</small>
							<b>Kyoto fare watch</b>
						</div>
						<div class="arrow">synthesized on this Mac</div>
						<div class="node on">
							<small>Task episode</small>
							<b>Thursday · fare dropped 12%</b>
						</div>
						<div class="sinks">
							<div class="node">
								<small>Agent</small>
								<b>travel_style</b>
							</div>
							<div class="node">
								<small>User</small>
								<b>preferences</b>
							</div>
						</div>
						<p class="note">Nothing sensitive left this machine.</p>
					</div>
				{:else if frameId === 'password'}
					<div class="vault">
						<div class="row">
							<span>gmail</span>
							<code>you@…</code>
						</div>
						<div class="row">
							<span>password</span>
							<code class="secret">••••••••</code>
						</div>
						<div class="row">
							<span>stored in</span>
							<code>OS keychain</code>
						</div>
						<div class="tag">the model never saw this</div>
					</div>
				{:else if frameId === 'card'}
					<div class="card">
						<div class="plastic">
							<small>Visa · sealed</small>
							<b>•••• •••• •••• 4242</b>
							<span>08 / 28 &nbsp; CVC •••</span>
						</div>
						<div class="row">
							<span>injected at</span>
							<code>checkout</code>
						</div>
						<div class="tag">the model never saw this</div>
					</div>
				{:else if frameId === 'ceilings'}
					<div class="authority">
						<header>
							<h3>System ceilings</h3>
							<small>Live</small>
						</header>
						<div class="ceil">
							<div class="meta">
								<b>USD</b>
								<span>$152 / $400 · monthly</span>
							</div>
							<div class="bar"><i style="width: 38%"></i></div>
						</div>
						<div class="ceil">
							<div class="meta">
								<b>Tokens</b>
								<span>2.4M / 8M · monthly</span>
							</div>
							<div class="bar"><i style="width: 30%"></i></div>
						</div>
						<div class="ceil">
							<div class="meta">
								<b>Browser</b>
								<span>12 / 40 hrs · monthly</span>
							</div>
							<div class="bar"><i style="width: 30%"></i></div>
						</div>
					</div>
				{:else if frameId === 'tokens'}
					<div class="authority">
						<header>
							<h3>Active tokens</h3>
							<small>3</small>
						</header>
						<div class="tok">
							<b>travel-agent</b>
							<span>$80 remaining</span>
						</div>
						<div class="tok">
							<b>home-agent</b>
							<span>$40 remaining</span>
						</div>
						<div class="tok">
							<b>research-agent</b>
							<span>1.1M tokens remaining</span>
						</div>
						<div class="freeze">Freeze all</div>
					</div>
				{:else if frameId === 'roster'}
					<div class="roster">
						<div class="mate run">
							<b>Travel</b>
							<span>Watching fares</span>
							<em>Running</em>
						</div>
						<div class="mate">
							<b>Mail</b>
							<span>Drafts in your tone</span>
							<em>Idle</em>
						</div>
						<div class="mate run">
							<b>Research</b>
							<span>Visa pack</span>
							<em>Running</em>
						</div>
						<div class="mate">
							<b>Home</b>
							<span>Bills &amp; school calendar</span>
							<em>Idle</em>
						</div>
					</div>
				{:else if frameId === 'working'}
					<div class="working">
						<header>
							<h3>Travel</h3>
							<em>On its own</em>
						</header>
						<p>Watching ANA against your $900 cap. Will ping before it buys.</p>
						<ul>
							<li>Cycle 14 · no spend yet</li>
							<li>Delegates to Research if the visa pack blocks</li>
							<li>You can pause or take over any time</li>
						</ul>
					</div>
				{/if}
			</div>
	</div>
	</div>
	</div>

	{#if frames.length > 1}
		<div class="carousel">
			<Icon name="chevron-left" size={14} />
			{#each frames as f, i (f.id)}
				<span class="pip" class:on={i === frameIndex} data-pip={f.id} title={f.chrome}>
					<Icon name={f.icon} size={13} />
				</span>
			{/each}
			<Icon name="chevron-right" size={14} />
		</div>
	{/if}
</div>

<style>
	.float {
		--shot-w: 30rem;
		--shot-h: 25.2rem;
		width: min(100%, var(--shot-w));
		display: flex;
		flex-direction: column;
		align-items: stretch;
		gap: 0.55rem;
		color: var(--text-primary, #1a1612);
		text-align: left;
	}

	.float.bob {
		animation: bob 5.8s ease-in-out infinite;
		will-change: transform;
	}

	@keyframes bob {
		0%,
		100% {
			transform: translate3d(0, 0, 0) rotate(-0.55deg);
		}
		50% {
			transform: translate3d(0, -0.7rem, 0) rotate(0.7deg);
		}
	}

	.shot-scale {
		width: 100%;
	}

	.shot {
		height: var(--shot-h);
		border: 1px solid rgba(26, 22, 18, 0.16);
		background: color-mix(in srgb, var(--bg-elevated, #faf3e0) 92%, white);
		border-radius: 14px;
		box-shadow: 0 2px 0 rgba(26, 22, 18, 0.05), 0 18px 40px -18px rgba(26, 22, 18, 0.28);
		overflow: hidden;
		display: flex;
		flex-direction: column;
	}

	.chrome {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.62rem 0.85rem;
		border-bottom: 1px solid rgba(26, 22, 18, 0.08);
		flex: none;
	}

	.chrome span {
		width: 0.48rem;
		height: 0.48rem;
		border-radius: 99px;
		background: rgba(26, 22, 18, 0.16);
	}

	.chrome span:nth-child(1) {
		background: #c9846a;
	}
	.chrome span:nth-child(2) {
		background: #c4b07a;
	}
	.chrome span:nth-child(3) {
		background: #7fa08a;
	}

	.chrome em {
		margin-left: 0.45rem;
		font-style: normal;
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.68rem;
		letter-spacing: 0.04em;
		opacity: 0.55;
	}

	.carousel {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 0.38rem;
		opacity: 0.72;
	}

	.pip {
		width: 1.7rem;
		height: 1.7rem;
		border-radius: 8px;
		border: 1px solid rgba(26, 22, 18, 0.14);
		background: color-mix(in srgb, var(--bg-elevated, #faf3e0) 88%, white);
		display: grid;
		place-items: center;
		opacity: 0.42;
		transition: opacity 0.25s ease, border-color 0.25s ease, background 0.25s ease;
	}

	.pip.on {
		opacity: 1;
		border-color: color-mix(in srgb, var(--accent-primary, #28437a) 45%, transparent);
		background: color-mix(in srgb, var(--accent-primary, #28437a) 10%, white);
		color: var(--accent-primary, #28437a);
	}

	.stage {
		position: relative;
		flex: 1;
		min-height: 0;
		overflow: hidden;
	}

	.frame {
		position: absolute;
		inset: 0;
		padding: 0.72rem 0.85rem 0.8rem;
		overflow: hidden;
	}

	.today {
		display: grid;
		grid-template-rows: minmax(0, 1.25fr) minmax(0, 0.9fr);
		gap: 0.4rem;
		height: 100%;
	}

	.band {
		border: 1px solid rgba(26, 22, 18, 0.1);
		border-radius: 10px;
		padding: 0.5rem 0.65rem 0.45rem;
		min-height: 0;
		overflow: hidden;
	}

	.band.muted {
		background: rgba(26, 22, 18, 0.03);
	}

	.band header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		margin-bottom: 0.28rem;
	}

	.band h3,
	.authority h3,
	.working h3 {
		margin: 0;
		font-size: 0.78rem;
		font-weight: 560;
		letter-spacing: -0.02em;
	}

	.band small,
	.authority small {
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.62rem;
		opacity: 0.5;
	}

	.band ul,
	.working ul {
		margin: 0;
		padding: 0;
		list-style: none;
		display: grid;
		gap: 0.28rem;
	}

	.band li,
	.list .row,
	.roster .mate,
	.tok {
		display: grid;
		grid-template-columns: 1fr auto;
		gap: 0.05rem 0.55rem;
		align-items: center;
	}

	.band li .copy {
		display: grid;
		gap: 0.04rem;
		min-width: 0;
	}

	.band li span {
		font-size: 0.68rem;
		opacity: 0.58;
	}

	.cta {
		font-style: normal;
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.56rem;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		border: 1px solid rgba(26, 22, 18, 0.16);
		border-radius: 6px;
		padding: 0.2rem 0.42rem;
		opacity: 0.78;
		white-space: nowrap;
	}

	.cta.pri {
		opacity: 1;
		color: var(--accent-primary, #28437a);
		border-color: color-mix(in srgb, var(--accent-primary, #28437a) 40%, transparent);
		background: color-mix(in srgb, var(--accent-primary, #28437a) 8%, transparent);
	}

	.band li b,
	.list b,
	.roster b,
	.tok b,
	.tiers b,
	.node b {
		font-size: 0.8rem;
		font-weight: 550;
	}

	.chat {
		display: flex;
		flex-direction: column;
		height: 100%;
		min-height: 0;
	}

	.thread {
		flex: 1;
		min-height: 0;
		display: flex;
		flex-direction: column;
		gap: 0.38rem;
		overflow: hidden;
	}

	.bubble {
		max-width: 88%;
		padding: 0.42rem 0.58rem;
		border-radius: 10px;
		font-size: 0.76rem;
		line-height: 1.35;
	}

	.bubble.you {
		align-self: flex-end;
		background: color-mix(in srgb, var(--accent-primary, #28437a) 12%, transparent);
	}

	.bubble.bot {
		align-self: flex-start;
		background: rgba(26, 22, 18, 0.05);
	}

	.composer {
		margin-top: auto;
		border: 1px solid rgba(26, 22, 18, 0.14);
		border-radius: 12px;
		background: color-mix(in srgb, var(--bg-elevated, #faf3e0) 70%, white);
		box-shadow: 0 8px 20px -14px rgba(26, 22, 18, 0.35);
		overflow: hidden;
	}

	.composer-row {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.55rem 0.65rem 0.4rem 0.75rem;
	}

	.composer-row span {
		flex: 1;
		font-size: 0.78rem;
		opacity: 0.42;
	}

	.mic,
	.clip {
		flex: none;
		opacity: 0.45;
	}

	.mic {
		width: 1.45rem;
		height: 1.45rem;
		padding: 0.22rem;
		border-radius: 99px;
		border: 1px solid rgba(26, 22, 18, 0.14);
		box-sizing: border-box;
	}

	.composer-tools {
		display: flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.12rem 0.55rem 0.4rem;
		border-top: 1px solid rgba(26, 22, 18, 0.06);
	}

	.seg {
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.56rem;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		opacity: 0.55;
		background: rgba(26, 22, 18, 0.05);
		border-radius: 5px;
		padding: 0.16rem 0.35rem;
	}

	.seg b {
		font-weight: 550;
		opacity: 1;
		background: var(--text-primary, #1a1612);
		color: var(--bg-elevated, #faf3e0);
		border-radius: 3px;
		padding: 0.08rem 0.28rem;
		margin-right: 0.2rem;
	}

	.composer-tools em {
		margin-left: auto;
		font-style: normal;
		font-size: 0.62rem;
		opacity: 0.45;
	}

	.list,
	.roster,
	.authority,
	.vault,
	.synth,
	.tiers {
		display: grid;
		gap: 0.42rem;
		height: 100%;
		align-content: start;
	}

	.list .row,
	.roster .mate,
	.tok {
		border: 1px solid rgba(26, 22, 18, 0.1);
		border-radius: 10px;
		padding: 0.5rem 0.65rem;
	}

	.list em,
	.roster em,
	.working em {
		font-style: normal;
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.62rem;
		letter-spacing: 0.04em;
		text-transform: uppercase;
		opacity: 0.55;
	}

	.row.run,
	.mate.run {
		border-color: color-mix(in srgb, var(--accent-primary, #28437a) 35%, transparent);
		background: color-mix(in srgb, var(--accent-primary, #28437a) 7%, transparent);
	}

	.row.wait {
		border-color: color-mix(in srgb, #7c2d12 28%, transparent);
	}

	.row.done {
		opacity: 0.7;
	}

	.tiers article,
	.node {
		border: 1px solid rgba(26, 22, 18, 0.1);
		border-radius: 10px;
		padding: 0.48rem 0.65rem;
		display: grid;
		gap: 0.08rem;
	}

	.tiers article.on,
	.node.on {
		border-color: color-mix(in srgb, var(--accent-primary, #28437a) 35%, transparent);
		background: color-mix(in srgb, var(--accent-primary, #28437a) 7%, transparent);
	}

	.tiers small,
	.node small,
	.card small {
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.6rem;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		opacity: 0.5;
	}

	.tiers span,
	.roster span,
	.tok span,
	.meta span {
		font-size: 0.7rem;
		opacity: 0.6;
	}

	.arrow {
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.64rem;
		letter-spacing: 0.06em;
		text-transform: uppercase;
		text-align: center;
		opacity: 0.5;
	}

	.sinks {
		display: grid;
		grid-template-columns: 1fr 1fr;
		gap: 0.42rem;
	}

	.note {
		margin: 0.15rem 0 0;
		font-size: 0.76rem;
		opacity: 0.62;
	}

	.vault,
	.card {
		display: grid;
		gap: 0.42rem;
		align-content: start;
	}

	.vault .row,
	.card .row {
		display: flex;
		justify-content: space-between;
		font-size: 0.85rem;
		padding: 0.45rem 0.55rem;
		border-radius: 8px;
		background: rgba(26, 22, 18, 0.04);
	}

	code {
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.8em;
	}

	.secret {
		letter-spacing: 0.14em;
		color: var(--accent-primary, #28437a);
	}

	.tag {
		font-family: var(--lp-mono, ui-monospace, monospace);
		font-size: 0.66rem;
		color: var(--accent-primary, #28437a);
		padding-top: 0.15rem;
	}

	.plastic {
		border-radius: 12px;
		padding: 0.9rem 0.85rem 0.8rem;
		background: linear-gradient(
			135deg,
			color-mix(in srgb, var(--accent-primary, #28437a) 82%, #1a1612),
			color-mix(in srgb, var(--accent-primary, #28437a) 45%, #7c2d12)
		);
		color: #faf3e0;
		display: grid;
		gap: 0.45rem;
		min-height: 7.2rem;
		align-content: end;
	}

	.plastic small {
		opacity: 0.7;
		color: inherit;
	}

	.plastic b {
		font-size: 1.05rem;
		font-weight: 500;
		letter-spacing: 0.12em;
	}

	.plastic span {
		font-size: 0.78rem;
		opacity: 0.82;
		letter-spacing: 0.08em;
	}

	.authority header,
	.working header {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
	}

	.ceil {
		display: grid;
		gap: 0.28rem;
		padding: 0.5rem 0.15rem 0.15rem;
	}

	.meta {
		display: flex;
		justify-content: space-between;
		gap: 0.6rem;
		align-items: baseline;
	}

	.meta b {
		font-size: 0.82rem;
		font-weight: 550;
	}

	.bar {
		height: 0.38rem;
		border-radius: 99px;
		background: rgba(26, 22, 18, 0.08);
		overflow: hidden;
	}

	.bar i {
		display: block;
		height: 100%;
		background: var(--accent-primary, #28437a);
		border-radius: inherit;
	}

	.freeze {
		margin-top: 0.2rem;
		border: 1px solid rgba(124, 45, 18, 0.35);
		color: #7c2d12;
		border-radius: 8px;
		padding: 0.45rem 0.6rem;
		font-size: 0.78rem;
		text-align: center;
	}

	.roster .mate {
		grid-template-columns: 1fr auto;
	}

	.roster .mate span {
		grid-column: 1;
	}

	.working {
		display: grid;
		align-content: start;
		gap: 0.55rem;
	}

	.working p {
		margin: 0;
		font-size: 0.88rem;
		line-height: 1.4;
	}

	.working li {
		font-size: 0.78rem;
		opacity: 0.7;
		padding: 0.18rem 0;
		border-bottom: 1px solid rgba(26, 22, 18, 0.08);
	}

	@media (max-width: 820px), (max-height: 560px) {
		.float {
			width: 100%;
			max-width: var(--shot-w);
		}

		/* Keep the desktop chrome layout (30 × 25.2rem) and scale the whole
		   window to the available box. Shrinking the box used to reflow
		   the inner UI into overflow:hidden, so most of each frame vanished. */
		.shot-scale {
			position: relative;
			aspect-ratio: 30 / 25.2;
			container-type: inline-size;
			overflow: hidden;
		}

		.shot {
			position: absolute;
			top: 0;
			left: 0;
			width: var(--shot-w);
			height: var(--shot-h);
			transform-origin: top left;
			transform: scale(calc(100cqi / var(--shot-w)));
		}

		.float.bob {
			animation: none;
		}
	}

	@media (max-height: 560px) {
		.carousel {
			display: none;
		}

		.shot-scale {
			width: min(100%, var(--shot-w));
			height: min(var(--shot-h), calc(100svh - 5.5rem));
			max-width: 100%;
			container-type: size;
		}

		.shot {
			transform: scale(
				min(calc(100cqi / var(--shot-w)), calc(100cqb / var(--shot-h)))
			);
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.float.bob {
			animation: none;
		}
	}
</style>
