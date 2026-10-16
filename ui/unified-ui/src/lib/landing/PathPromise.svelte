<script lang="ts">
	// Road A — "Knows you · Acts for you · Belongs to you".
	//
	// TrustReveal states these three as a card row of one sentence each. That
	// row is the TEASER; this road is what is under it, and it must not read as
	// the same three sentences again. So every beat here shows a mechanism the
	// teaser never mentions: the actual memory tiers and their temperature, the
	// redacted shape a watched tab is reduced to, the two-speed resurfacing
	// engine, Set of Marks on a real site, the roster as YAML, the placeholder
	// literal the model is handed instead of a password, and the retention
	// windows on the stores you can purge yourself.
	//
	// Every number and identifier below is from docs/features/README.md and the
	// component docs it links to. Nothing here is aspirational: the autopilot
	// "close your laptop and it keeps coding" claim is deliberately absent, and
	// what stands in its place is what actually runs unattended — scheduled
	// work and recurring monitors.
	import RoadTrack from './RoadTrack.svelte';
	import PathBeat from './PathBeat.svelte';

	export let still = false;
	/** Passed straight through — see RoadTrack.progress. */
	export let progress: number | null = null;
</script>

<!-- No `revealBeats` here any more: RoadTrack drives every station's
     `--local` from the camera's own proximity, and two writers on one
     custom property is a fight, not a fallback. -->
<!-- No act signs. Each act used to open with a PathActSign — "ACT ONE",
     the act's name again in olive, and a line of its own — directly above
     stations that LandingChrome is already titling in gold from the fixed
     layer. Two headings for one act, in two colours, one of them scrolling
     past while the other stayed. The gold one now does the whole job: it
     arrives big at centre and settles up, which is the entrance the sign
     used to provide. -->
<div class="pa-road">
	<RoadTrack {progress} {still} signs={false}>
	<!-- ═══ ACT ONE ═══════════════════════════════════════════════ -->
	<div class="pa-act pa-knows">

		<PathBeat
			{still}
			num="01"
			act="Knows you"
			title="It keeps what you would have had to remember."
			caption="Facts heat with evidence. Cold ones go stale on their own after sixty days."
		>
			<div class="pb-card pb-in" style="--i:0">
				<div class="pb-head">Memory · tiers<span class="pb-head-r">your scope</span></div>
				<div class="pb-row">
					<span class="pb-dot"></span>entities
					<span class="pb-dim">people, accounts, the places you buy from</span>
					<span class="pb-row-r pb-mono pb-acc">T0</span>
				</div>
				<div class="pb-row">
					<span class="pb-dot"></span>user.preferences
					<span class="pb-dim">aisle seat · no dairy · replies before noon</span>
					<span class="pb-row-r pb-mono pb-acc">T0</span>
				</div>
				<div class="pb-row">
					<span class="pb-dot pb-dot-off"></span>insights
					<span class="pb-dim">what it worked out, not what you said</span>
					<span class="pb-row-r pb-mono pb-dim">T1 · 0.62</span>
				</div>
				<div class="pb-row">
					<span class="pb-dot pb-dot-off"></span>user.research_findings
					<span class="pb-dim">merged out of meetings</span>
					<span class="pb-row-r pb-mono pb-dim">T1</span>
				</div>
				<div class="pb-row">
					<span class="pb-dot pb-dot-off"></span>archive
					<span class="pb-dim">cold, still searchable</span>
					<span class="pb-row-r pb-mono pb-dim">stale 60d</span>
				</div>
				<p class="pb-foot">
					retrieval is hybrid — keyword plus embeddings computed locally. if the local embedder is
					down it fails loudly rather than writing fake vectors.
				</p>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="02"
			act="Knows you"
			title="It was in the tab, and in the room."
			caption="It kept the shape of the page, not the page."
		>
			<div class="pb-pair">
				<div class="pb-card pb-in" style="--i:0">
					<div class="pb-head">Tabs — today<span class="pb-head-r">1 run</span></div>
					<div class="pb-row">
						<span class="pb-dot"></span><span class="pb-mono">origin · /holdings</span>
						<span class="pb-row-r pb-mono pb-dim">page key 8f21</span>
					</div>
					<div class="pb-row">
						<span class="pb-dot"></span><span class="pb-mono">origin · article</span>
						<span class="pb-row-r pb-mono pb-dim">text/html</span>
					</div>
					<div class="pb-row">
						<span class="pb-dot pb-dot-off"></span>
						<span class="pb-dim pb-mono">page contents — not stored</span>
					</div>
					<p class="pb-foot pb-foot-acc">distilled on your machine · 0 tokens sent · $0.00</p>
				</div>
				<div class="pb-card pb-in" style="--i:1">
					<div class="pb-head">Meeting · listening<span class="pb-head-r">not a guest</span></div>
					<div class="pb-well">
						<span class="pb-dim">speaker 1</span> …so we hold the Pune vendor until Q3.<br />
						<span class="pb-dim">speaker 2</span> I’ll send the revised quote Friday.
					</div>
					<div class="pb-row"><span class="pb-dot"></span>decision — hold Pune until Q3</div>
					<div class="pb-row"><span class="pb-dot"></span>owed to you — revised quote, Friday</div>
					<p class="pb-foot">filed into user.research_findings when the call ends</p>
				</div>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="03"
			act="Knows you"
			title="Six weeks later, it says the thing you had forgotten."
			caption="Nothing you thought gets deleted. It gets marked."
		>
			<div class="pb-pair">
				<div class="pb-card pb-in" style="--i:0">
					<div class="pb-head">Worth a look<span class="pb-head-r">today</span></div>
					<div class="pb-row">
						<span class="pb-dot"></span>
						<span>The vendor quote you compared expires Friday.</span>
					</div>
					<div class="pb-row"><span class="pb-dot pb-dot-off"></span><span class="pb-dim">21 Jun · you had three tabs open on it</span></div>
					<p class="pb-foot">
						scored every hour with no model at all · read once a day by one that costs something ·
						a cap, then silence
					</p>
				</div>
				<div class="pb-card pb-in" style="--i:1">
					<div class="pb-head">Thinking map · branch<span class="pb-head-r">kept</span></div>
					<div class="pb-row"><span class="pb-mono pb-acc">asserted</span><span>ship in two phases</span></div>
					<div class="pb-row"><span class="pb-mono pb-dim">contradicted</span><span class="pb-dim">one launch, one date</span></div>
					<div class="pb-row"><span class="pb-mono pb-dim">superseded</span><span class="pb-strike">wait for the Q3 numbers</span></div>
					<p class="pb-foot">a deletion here is a tombstone — the branch stays, and can be replayed</p>
				</div>
			</div>
		</PathBeat>
	</div>

	<!-- ═══ ACT TWO ═══════════════════════════════════════════════ -->
	<div class="pa-act pa-acts">

		<PathBeat
			{still}
			num="04"
			act="Acts for you"
			title="Hands on the actual site."
			caption="The same site you would have used. The same clicks."
		>
			<div class="pb-card pb-card-wide pb-in" style="--i:0">
				<div class="pa-brow">
					<span class="pa-brow-dots" aria-hidden="true"><i></i><i></i><i></i></span>
					<span class="pa-brow-url pb-mono">airline · /booking/seats</span>
					<span class="pb-chip">your window</span>
				</div>
				<div class="pa-page">
					<span class="pa-mark">1</span><span class="pb-mono">seat map</span>
					<span class="pa-mark">2</span><span class="pb-mono">passenger</span>
					<span class="pa-mark">3</span><span class="pb-mono">pay</span>
				</div>
				<div class="pb-row"><span class="pb-dot"></span>read the fare table<span class="pb-row-r pb-mono pb-dim">done</span></div>
				<div class="pb-row"><span class="pb-dot"></span>pick 14A — the aisle it knows you take<span class="pb-row-r pb-mono pb-dim">done</span></div>
				<div class="pb-row"><span class="pb-dot pb-dot-off"></span><span class="pb-dim">pay</span><span class="pb-row-r pb-mono pb-acc">waits for you</span></div>
				<div class="pb-chips">
					<span class="pb-chip-quiet pb-chip">shadow DOM</span>
					<span class="pb-chip-quiet pb-chip">cross-origin iframe</span>
					<span class="pb-chip-quiet pb-chip">drag &amp; drop</span>
					<span class="pb-chip-quiet pb-chip">canvas</span>
				</div>
				<p class="pb-foot">
					marks are numbered on the page only when the layout needs looking at · no restarting your
					browser in debugging mode
				</p>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="05"
			act="Acts for you"
			title="Not a chatbot. A roster."
			caption="Open any of them and change what it is allowed to touch."
		>
			<div class="pb-card pb-card-wide pb-in" style="--i:0">
				<div class="pb-head">Your crew<span class="pb-head-r">defined as data</span></div>
				<div class="pa-crew">
					<div class="pa-crew-cell"><b>personal-assistant</b><span class="pb-mono pb-dim">calendar · mail · notes</span></div>
					<div class="pa-crew-cell"><b>web-researcher</b><span class="pb-mono pb-dim">browser · api-mining</span></div>
					<div class="pa-crew-cell"><b>mac-operator</b><span class="pb-mono pb-dim">macos-ui · imessage · shortcuts</span></div>
					<div class="pa-crew-cell"><b>wealth-manager</b><span class="pb-mono pb-dim">sheets · market reads</span></div>
					<div class="pa-crew-cell"><b>writing-assistant</b><span class="pb-mono pb-dim">draft · rewrite</span></div>
					<div class="pa-crew-cell pa-crew-new"><b>+ your own</b><span class="pb-mono pb-dim">a double of you</span></div>
				</div>
				<p class="pb-foot">
					26 templates ship as YAML — the persona, the tools, the memory tiers and the limits are
					all data you can open
				</p>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="06"
			act="Acts for you"
			title="It keeps working when you are not there."
			caption="Silence is a result here. It never animates to fill one."
		>
			<div class="pb-card pb-in" style="--i:0">
				<div class="pb-head">Monitor · review before it starts<span class="pb-head-r">draft</span></div>
				<div class="pb-row"><span class="pb-dim pb-mono">watching</span><span class="pb-row-r">the fare on your Kyoto leg</span></div>
				<div class="pb-row"><span class="pb-dim pb-mono">sources</span><span class="pb-row-r">2 airlines, 1 aggregator</span></div>
				<div class="pb-row"><span class="pb-dim pb-mono">tell me</span><span class="pb-row-r">only if it drops more than ₹2,000</span></div>
				<div class="pb-row"><span class="pb-dim pb-mono">schedule</span><span class="pb-row-r pb-mono">every day, 06:00 IST</span></div>
				<div class="pb-chips">
					<span class="pb-chip">activate</span><span class="pb-chip-quiet pb-chip">edit</span>
				</div>
				<p class="pb-foot">
					the first run is a baseline and never notifies · a run that finds nothing new surfaces
					nothing at all — no card, no badge, no ping
				</p>
			</div>
		</PathBeat>
	</div>

	<!-- ═══ ACT THREE ═════════════════════════════════════════════ -->
	<div class="pa-act pa-belongs">

		<PathBeat
			{still}
			num="07"
			act="Belongs to you"
			title="There is no model anywhere in the secret path."
			caption="The password goes in after the thinking is over."
		>
			<div class="pb-pair">
				<div class="pb-card pb-in" style="--i:0">
					<div class="pb-head">What the model is handed</div>
					<div class="pb-well">
						POST /session<br />
						&nbsp;&nbsp;user: <span class="pb-acc">you@…</span><br />
						&nbsp;&nbsp;pass: <span class="pb-acc">[REDACTED:input-2]</span><br />
						&nbsp;&nbsp;card: <span class="pb-acc">[REF:9f2c…]</span>
					</div>
					<p class="pb-foot">it cannot leak a value it never had</p>
				</div>
				<div class="pb-card pb-in" style="--i:1">
					<div class="pb-head">What the runtime does</div>
					<div class="pb-row"><span class="pb-dot"></span>resolve<span class="pb-row-r pb-mono pb-dim">vault</span></div>
					<div class="pb-row"><span class="pb-dot"></span>check the policy<span class="pb-row-r pb-mono pb-dim">this origin only</span></div>
					<div class="pb-row"><span class="pb-dot"></span>inject at the keystroke</div>
					<div class="pb-row"><span class="pb-dot"></span>scrub the result<span class="pb-row-r pb-mono pb-dim">by value, not by name</span></div>
					<p class="pb-foot">AES-256-GCM · the master key lives in your OS keychain</p>
				</div>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="08"
			act="Belongs to you"
			title="The private part never leaves the machine."
			caption="Local is not a setting you go and find. It is where the sensitive work already lives."
		>
			<div class="pb-card pb-in" style="--i:0">
				<div class="pb-head">Where each piece ran<span class="pb-head-r">this hour</span></div>
				<div class="pb-row"><span class="pb-dot"></span>hearing its name<span class="pb-row-r pb-mono pb-acc">on device</span></div>
				<div class="pb-row"><span class="pb-dot"></span>reading your open tabs<span class="pb-row-r pb-mono pb-acc">on device · $0.00</span></div>
				<div class="pb-row"><span class="pb-dot"></span>fixing your typing<span class="pb-row-r pb-mono pb-acc">on device</span></div>
				<div class="pb-row"><span class="pb-dot pb-dot-off"></span>searching the fare<span class="pb-row-r pb-mono pb-dim">sent out · redacted</span></div>
				<p class="pb-foot">
					what leaves is the least a task can run on, and only in the seconds a task needs it
				</p>
			</div>
		</PathBeat>

		<PathBeat
			{still}
			num="09"
			act="Belongs to you"
			title="Yours to read, yours to take, yours to delete."
			caption="The door out is a feature, not a support ticket."
		>
			<div class="pb-card pb-card-wide pb-in" style="--i:0">
				<div class="pb-head">Storage<span class="pb-head-r">what it holds, and for how long</span></div>
				<div class="pb-row">
					<span class="pb-mono">attention</span>
					<span class="pb-dim">what asked for you</span>
					<span class="pb-row-r pb-mono pb-dim">30 days · purge</span>
				</div>
				<div class="pb-row">
					<span class="pb-mono">resurfacing</span>
					<span class="pb-dim">what it might raise</span>
					<span class="pb-row-r pb-mono pb-dim">90 days · purge</span>
				</div>
				<div class="pb-row">
					<span class="pb-mono">browser use</span>
					<span class="pb-dim">where its hands have been</span>
					<span class="pb-row-r pb-mono pb-dim">90 days / 50,000 rows · purge</span>
				</div>
				<div class="pb-row">
					<span class="pb-mono">a finished task</span>
					<span class="pb-dim">the whole run, offline</span>
					<span class="pb-row-r"><span class="pb-chip">zip</span> <span class="pb-chip">single html</span></span>
				</div>
				<p class="pb-foot">
					a sweep runs at startup and every six hours whether you ask or not · an export carries the
					work, never the agent’s own scratch
				</p>
			</div>
		</PathBeat>
	</div>
</RoadTrack>
</div>

<style>
	/* The browser beat draws a browser, because the claim is that the hands
	   land in the window you already have open — not in a headless copy. */
	.pa-brow {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding-bottom: 0.7rem;
	}
	.pa-brow-dots {
		display: inline-flex;
		gap: 4px;
	}
	.pa-brow-dots i {
		width: 7px;
		height: 7px;
		border-radius: 50%;
		background: color-mix(in srgb, var(--pf-ink) 18%, transparent);
	}
	.pa-brow-url {
		flex: 1;
		min-width: 0;
		overflow-wrap: anywhere;
		border: 1px solid var(--pf-line);
		border-radius: 999px;
		padding: 0.24rem 0.7rem;
		font-size: 0.72rem;
		color: var(--pf-dim);
	}
	.pa-page {
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 0.4rem 0.5rem;
		border: 1px dashed var(--pf-line);
		border-radius: 10px;
		padding: 0.7rem;
		font-size: 0.78rem;
		color: var(--pf-dim);
	}
	.pa-page .pb-mono {
		margin-right: 0.8rem;
	}
	/* A mark: the numbered box the model is given when the layout is the
	   thing it has to reason about. */
	.pa-mark {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.35em;
		height: 1.35em;
		flex: none;
		border-radius: 4px;
		background: var(--a);
		color: var(--text-on-accent, #fff);
		font-family: var(--lp-mono);
		font-size: 0.68rem;
		font-weight: 600;
	}

	.pa-crew {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr));
		gap: 0.5rem;
		padding-top: 0.4rem;
	}
	.pa-crew-cell {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
		border: 1px solid var(--pf-line);
		border-left: 2px solid var(--a);
		border-radius: 10px;
		padding: 0.5rem 0.6rem;
		font-size: 0.84rem;
	}
	.pa-crew-cell b {
		font-weight: 560;
		letter-spacing: -0.01em;
	}
	.pa-crew-new {
		border-left-style: dashed;
		border-left-color: var(--a);
	}
	.pa-crew-new b {
		color: var(--a-ink);
	}
</style>
