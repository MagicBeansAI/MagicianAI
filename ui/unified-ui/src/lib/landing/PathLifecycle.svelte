<script lang="ts">
	// Road B — "Started · Moving · Closed".
	//
	// The owner's own three words, arranged as an arc so it lands on
	// completion rather than momentum: one piece of work, from the second it
	// leaves your hands to the receipt that closes it. The third act is the
	// point of the whole road — Moving is a state anyone can demo, Closed is
	// the one nobody does.
	//
	// The receipt beat is the film's real payoff and its rows are quoted from
	// the shipped panel, values and all, including the rule that a row which
	// was not earned is ABSENT rather than zero. The night log is likewise the
	// real storyboard: scheduled work and recurring monitors, which do run
	// unattended — not "close your laptop and it keeps coding", which does not.
	import PathActSign from './PathActSign.svelte';
	import RoadTrack from './RoadTrack.svelte';
	import PathBeat from './PathBeat.svelte';
	import PathCount from './PathCount.svelte';

	export let still = false;
</script>

<!-- No `revealBeats` here any more: RoadTrack drives every station's
     `--local` from the camera's own proximity, and two writers on one
     custom property is a fight, not a fallback. -->
<div class="pl-road">
	<RoadTrack {still}>
	<!-- ═══ ACT ONE ═══════════════════════════════════════════════ -->
	<div class="pa-act pa-started">
		<PathActSign
			{still}
			ordinal="Act one"
			name="Started"
			tag="Knows you"
			line="The distance between thinking of it and handing it over should be one gesture."
		/>

		<PathBeat
			{still}
			num="01"
			act="Started"
			title="Say it where you already are."
			caption="The AI is summoned. It never hovers."
		>
			<div class="pb-card pb-card-wide pb-in" style="--i:0">
				<div class="pb-head">Four ways in<span class="pb-head-r">no new window</span></div>
				<div class="pl-surfaces">
					<div class="pl-surface">
						<span class="pl-surface-k">in the browser</span>
						<div class="pb-well">
							<span class="pl-sel">the paragraph you highlighted</span><br />
							<span class="pb-acc">✦ Ask Magican</span> · Rewrite · Draft reply
						</div>
					</div>
					<div class="pl-surface">
						<span class="pl-surface-k">in any Mac app</span>
						<div class="pb-well">
							<span class="pl-sel">the sentence you selected</span><br />
							hold <span class="pb-acc">⌥ Left Option</span>
						</div>
					</div>
					<div class="pl-surface">
						<span class="pl-surface-k">on your phone</span>
						<div class="pb-well">
							<span class="pb-acc">Magican ✦</span> — the spacebar<br />
							Write · Ask · Act
						</div>
					</div>
					<div class="pl-surface">
						<span class="pl-surface-k">out loud</span>
						<div class="pb-well">
							“<span class="pb-acc">Hey Presto</span>, book the Kyoto leg”<br />
							or <span class="pb-acc">⌥ Space</span>
						</div>
					</div>
				</div>
				<p class="pb-foot">
					hold the spacebar and it appears; drag it and you get the caret instead — movement wins,
					so the AI never fires because your thumb slipped
				</p>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="02"
			act="Started"
			title="It comes back with a plan, not a paragraph."
			caption="Nothing has run. This is a proposal, and it is legible."
		>
			<div class="pb-card pb-in" style="--i:0">
				<div class="pb-head">Plan<span class="pb-head-r"><span class="pb-chip">draft</span></span></div>
				<div class="pb-row"><span class="pb-mono pb-dim">1</span>read the fare table<span class="pb-row-r pb-mono pb-dim">browser</span></div>
				<div class="pb-row"><span class="pb-mono pb-dim">2</span>check it against your cap<span class="pb-row-r pb-mono pb-dim">budget</span></div>
				<div class="pb-row"><span class="pb-mono pb-dim">3</span>hold 14A for twenty minutes<span class="pb-row-r pb-mono pb-dim">browser</span></div>
				<div class="pb-row"><span class="pb-mono pb-dim">4</span>pay<span class="pb-row-r pb-mono pb-acc">asks you first</span></div>
				<p class="pb-foot">a plan appears when a plan is useful — a one-step ask stays one step</p>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="03"
			act="Started"
			title="And a draft is a draft until you say go."
			caption="You change the plan and the run changes. Never the other way round."
		>
			<div class="pb-card pb-in" style="--i:0">
				<div class="pb-head">Plan<span class="pb-head-r pb-acc">edited by you</span></div>
				<div class="pb-row"><span class="pb-mono pb-dim">1</span>read the fare table<span class="pb-row-r pb-mono pb-dim">browser</span></div>
				<div class="pb-row"><span class="pb-mono pb-dim">2</span><span class="pb-strike">check it against your cap</span><span class="pb-row-r pb-mono pb-dim">removed</span></div>
				<div class="pb-row"><span class="pb-mono pb-dim">3</span>hold 14A for twenty minutes<span class="pb-row-r pb-mono pb-dim">browser</span></div>
				<div class="pb-row">
					<span class="pb-mono pb-acc">+</span>
					<span class="pb-acc">morning departures only. no red-eyes.</span>
				</div>
				<div class="pb-chips">
					<span class="pb-btn pb-btn-go">Run it</span><span class="pb-btn">Keep editing</span>
				</div>
				<p class="pb-foot">the plan you approved is the plan that runs — it is not a summary of one</p>
			</div>
		</PathBeat>
	</div>

	<!-- ═══ ACT TWO ═══════════════════════════════════════════════ -->
	<div class="pa-act pa-moving">
		<PathActSign
			{still}
			ordinal="Act two"
			name="Moving"
			tag="Acts for you"
			line="The hours it works are mostly hours you are not watching."
		/>

		<PathBeat
			{still}
			num="04"
			act="Moving"
			title="It works the hours you don’t."
			caption="You slept. This is the part it did without you."
		>
			<div class="pb-card pb-card-wide pb-in" style="--i:0">
				<div class="pb-head">Overnight<span class="pb-head-r">while you were asleep</span></div>
				<div class="pb-row"><span class="pb-mono pb-acc pl-t">23:41</span>a monitor caught the Kyoto fare and drafted the rebooking</div>
				<div class="pb-row"><span class="pb-mono pb-acc pl-t">00:58</span>triaged 14 mails, held 2 back for the morning</div>
				<div class="pb-row"><span class="pb-mono pb-acc pl-t">02:30</span>filed the weekly expense report</div>
				<div class="pb-row"><span class="pb-mono pb-acc pl-t">05:50</span>assembled the brief you will read at 07:30</div>
				<p class="pb-foot">
					scheduled and recurring work, run unattended — every one of them with a history you can
					open and an owner you can point at
				</p>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="05"
			act="Moving"
			title="It only speaks when something changed."
			caption="Three silent mornings are the feature, not the outage."
		>
			<div class="pb-card pb-in" style="--i:0">
				<div class="pb-head">Monitor · runs<span class="pb-head-r">Kyoto fare</span></div>
				<div class="pb-row"><span class="pb-dot pb-dot-off"></span><span class="pb-dim">Mon — baseline</span><span class="pb-row-r pb-mono pb-dim">never notifies</span></div>
				<div class="pb-row"><span class="pb-dot pb-dot-off"></span><span class="pb-dim">Tue — nothing moved</span><span class="pb-row-r pb-mono pb-dim">silent</span></div>
				<div class="pb-row"><span class="pb-dot pb-dot-off"></span><span class="pb-dim">Wed — nothing moved</span><span class="pb-row-r pb-mono pb-dim">silent</span></div>
				<div class="pb-row"><span class="pb-dot"></span>Thu — down ₹2,400<span class="pb-row-r"><span class="pb-chip">in Today</span></span></div>
				<div class="pb-row"><span class="pb-dot pb-dot-off"></span><span class="pb-dim">Fri — the site refused to load</span><span class="pb-row-r pb-mono pb-dim">not a change</span></div>
				<p class="pb-foot">
					a source that will not open is a source failure, never a result · one blip stays quiet,
					the second one asks you for access — and clears itself the moment the site reads cleanly
				</p>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="06"
			act="Moving"
			title="It stops rather than guess."
			caption="It would rather wake you than spend your money on an assumption."
		>
			<div class="pb-pair">
				<div class="pb-card pb-in" style="--i:0">
					<div class="pb-head">Needs you<span class="pb-head-r">a question</span></div>
					<p class="pl-q">Two vendors match “Pune”. Which invoice did you mean?</p>
					<div class="pb-chips">
						<span class="pb-btn">Sharma &amp; Co · ₹84,000</span>
						<span class="pb-btn">Pune Logistics · ₹12,450</span>
					</div>
					<p class="pb-foot">it did not pick the likelier one and tell you afterwards</p>
				</div>
				<div class="pb-card pb-in" style="--i:1">
					<div class="pb-head">Held<span class="pb-head-r pb-acc">at the gate</span></div>
					<p class="pl-q">Paying this would go past the ceiling you set for today.</p>
					<div class="pb-chips">
						<span class="pb-btn pb-btn-go">Approve once</span><span class="pb-btn">Change the cap</span>
					</div>
					<p class="pb-foot">closed by default — when the gate cannot be reached, nothing is spent</p>
				</div>
			</div>
		</PathBeat>
	</div>

	<!-- ═══ ACT THREE ═════════════════════════════════════════════ -->
	<div class="pa-act pa-closed">
		<PathActSign
			{still}
			ordinal="Act three"
			name="Closed"
			tag="Belongs to you"
			line="Anyone can show you a thing that is busy. This is the part where it is over."
		/>

		<PathBeat
			{still}
			num="07"
			act="Closed"
			title="The receipt."
			caption="It stated its cost seventy times during the run and summed it nowhere. Here it is once."
		>
			<div class="pb-card pb-card-wide pb-in" style="--i:0">
				<div class="pb-head">What this run cost<span class="pb-head-r">3m 12s</span></div>
				<div class="pb-row"><span class="pb-dim">Cost</span><span class="pb-row-r pb-figure"><PathCount
							{still}
							from={100000}
							to={832625}
							pre="$0.01"
							at={0.34}
							span={0.34}
						/></span></div>
				<div class="pb-row"><span class="pb-dim">Tokens</span><span class="pb-row-r pb-mono">216k → 5.2k tok · 7 calls</span></div>
				<div class="pb-row"><span class="pb-dim">Prompt cache</span><span class="pb-row-r pb-mono"><PathCount {still} from={11} to={79} post="%" at={0.4} span={0.3}
						/> cached · 171k of 216k tok</span></div>
				<div class="pb-row"><span class="pb-dim">Model time</span><span class="pb-row-r pb-mono">1m 21s of 5m observed · 27%</span></div>
				<div class="pb-row"><span class="pb-dim">Failed calls</span><span class="pb-row-r pb-mono">3</span></div>
				<div class="pb-row"><span class="pb-dim">Models</span><span class="pb-row-r pb-mono">2 models · 3 calls</span></div>
				<p class="pb-foot">
					a row that was not earned is absent, not zero — no token row on a run that reported none,
					and never a dollar figure reverse-engineered from a model name
				</p>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="08"
			act="Closed"
			title="And the thing you actually asked for."
			caption="Take the whole run with you, or leave it here. Both are one click."
		>
			<div class="pb-card pb-in" style="--i:0">
				<div class="pb-head">Steps<span class="pb-head-r pb-mono"><PathCount {still} from={10} to={14} post=" steps" at={0.3} span={0.3}
						/> · 2 retries · 3m 12s</span></div>
				<div class="pb-row"><span class="pb-dot"></span>read the fare table<span class="pb-row-r pb-mono pb-dim">0.9s</span></div>
				<div class="pb-row pl-nest"><span class="pb-dim">↳ retried twice — the site was slow</span></div>
				<div class="pb-row"><span class="pb-dot"></span>hold 14A<span class="pb-row-r pb-mono pb-dim">2.4s</span></div>
				<div class="pb-row"><span class="pb-dot"></span>wrote <span class="pb-mono">itinerary.md</span> and 2 images</div>
				<div class="pb-chips">
					<span class="pb-btn">Open</span><span class="pb-btn">Reveal</span>
					<span class="pb-chip-quiet pb-chip">export · zip</span>
					<span class="pb-chip-quiet pb-chip">export · one html file</span>
				</div>
				<p class="pb-foot">
					retries nest under the step that had them, so “2 retries” reads as two marks on one step
					— not two more rows to scroll past
				</p>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="09"
			act="Closed"
			title="Then it asks before it keeps what it learned."
			caption="Closed means closed. The only thing carried forward is what you let through."
		>
			<div class="pb-pair">
				<div class="pb-card pb-in" style="--i:0">
					<div class="pb-head">Filed<span class="pb-head-r">one per finished run</span></div>
					<div class="pb-row"><span class="pb-dot"></span>outcome — booked, ₹38,400, 14A</div>
					<div class="pb-row"><span class="pb-dot"></span>still open — the return leg</div>
					<p class="pb-foot">published to your notes as a dated page you can read without this app</p>
				</div>
				<div class="pb-card pb-in" style="--i:1">
					<div class="pb-head">Worth remembering?<span class="pb-head-r pb-acc">your call</span></div>
					<p class="pl-q">“Prefers morning departures. Will pay more to avoid a red-eye.”</p>
					<div class="pb-chips">
						<span class="pb-btn pb-btn-go">Keep it</span><span class="pb-btn">Discard</span>
					</div>
					<p class="pb-foot">memory is review-gated — it does not promote itself for being confident</p>
				</div>
			</div>
		</PathBeat>
	</div>
</RoadTrack>
</div>

<style>
	/* Four ways in, so it reads as a two-by-two set rather than a row of
	   three with one straggler underneath. */
	.pl-surfaces {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
		gap: 0.6rem 1rem;
		padding-top: 0.4rem;
	}
	.pl-surface {
		display: flex;
		flex-direction: column;
		gap: 0.35rem;
	}
	.pl-surface-k {
		font-family: var(--lp-mono);
		font-size: 0.62rem;
		letter-spacing: 0.14em;
		text-transform: uppercase;
		color: var(--pf-dim);
	}
	/* The selection itself — the one thing on the page that is already yours
	   before anything is asked. */
	.pl-sel {
		background: var(--ah);
		box-shadow: 0 0 0 2px var(--ah);
		border-radius: 2px;
		color: var(--pf-ink);
	}

	.pl-t {
		flex: none;
		min-width: 3.4em;
	}
	.pl-nest {
		padding-left: 1.2rem;
		border-top-style: dashed;
		font-size: 0.82rem;
	}
	.pl-q {
		margin: 0.2rem 0 0.1rem;
		font-size: 0.95rem;
		line-height: 1.45;
		color: var(--pf-ink);
		text-wrap: balance;
	}
</style>
