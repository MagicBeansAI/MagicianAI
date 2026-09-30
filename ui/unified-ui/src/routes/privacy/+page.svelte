<script lang="ts">
	// Long-form policy, same cream reading surface as /manifesto and /terms.
	// Prose lives in the markup rather than a data module: nothing else
	// renders it, so there is no second consumer to justify the indirection.
	//
	// Deliberately modest. An earlier draft made guarantees the runtime does
	// not yet enforce — redaction, nothing-leaves-the-machine, delete-and-it-
	// is-gone. A privacy page that overstates is worse than a short one, so
	// this describes what happens and leaves the promises to the product.
	import DocShell from '$lib/landing/DocShell.svelte';
</script>

<svelte:head>
	<title>Privacy — Magican</title>
	<meta
		name="description"
		content="What Magican stores, what it sends out, and how to have it removed."
	/>
</svelte:head>

<DocShell title="Privacy" date="Last updated · 1 September 2026" legal>
	<p class="lede">
		Magican is a personal AI in private beta. This page describes what it stores, what it sends out,
		and how to have it removed. It is short because the setup is small.
	</p>

	<h2>What it stores</h2>
	<p>Only what you connect or tell it:</p>
	<ul>
		<li>
			Content from the accounts you link — mail, calendar, files, notes, messages — as far as the
			permissions you grant allow.
		</li>
		<li>What it records about you over time, so that it is useful later.</li>
		<li>A log of the actions it takes.</li>
		<li>Your email address and your settings.</li>
	</ul>

	<h2>Where it is kept</h2>
	<p>
		On the machine or instance running your Magican. If I am hosting that for you, it sits on
		infrastructure I control and I can technically reach it. I look when you ask me to help with
		something, or when I have to in order to keep the thing working.
	</p>

	<h2>What is sent out</h2>
	<ul>
		<li>
			<strong>To model providers.</strong> Doing anything useful means sending context to a model. Where
			it goes depends on which models you have configured — some can run on your own machine, most run
			elsewhere. Assume that what you put in front of Magican may reach the provider handling that task,
			under their terms.
		</li>
		<li>
			<strong>To the services you connect.</strong> When you ask it to act — send the mail, add the event,
			place the order — it talks to that service on your behalf.
		</li>
	</ul>
	<p>
		I do not sell your data. There is no advertising business and no analytics business behind this.
	</p>

	<h2>Credentials</h2>
	<p>
		Magican holds the credentials you give it so that it can act as you. It is built to keep them
		out of what the model sees, but treat any credential you hand to software as exposed to that
		software. Use scoped or revocable access wherever the service offers it.
	</p>

	<h2>What you control</h2>
	<ul>
		<li>What you connect, and what you disconnect.</li>
		<li>Which models it is allowed to use.</li>
		<li>What it may do on its own, and what has to wait for your approval.</li>
		<li>What it has stored — remove it yourself where the interface allows, or ask me and I will.</li>
	</ul>

	<h2>Diagnostics</h2>
	<p>
		Errors and usage get logged so that I can fix things, and those logs can contain content. If you
		would rather they did not, say so and I will turn it off for your instance.
	</p>

	<h2>Third parties</h2>
	<p>
		Model providers and the services you connect handle your data under their own terms and privacy
		policies, not this one. Which ones you use is your choice.
	</p>

	<h2>Age</h2>
	<p>Magican is not intended for people under 16.</p>

	<h2>Changes</h2>
	<p>This page will change as the product does. The date at the top is when it last did.</p>

	<h2>Reach me</h2>
	<p><a href="mailto:reach.magican@gmail.com">reach.magican@gmail.com</a></p>
</DocShell>
