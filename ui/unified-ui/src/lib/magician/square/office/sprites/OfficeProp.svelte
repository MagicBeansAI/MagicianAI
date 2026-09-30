<script lang="ts">
	/**
	 * One prop, dispatched by kind.
	 *
	 * The larger pieces live in their own files (they carry real geometry and
	 * are worth reading on their own); the small flat ones — whiteboard,
	 * cabinet, printer, stacked chairs, cartons, the lounge table — are drawn
	 * here, because a file each would be more ceremony than shape.
	 *
	 * Origin is the prop centre. `w`/`h` come from the plan so a prop stretches
	 * to the space the room actually gave it rather than being placed at a size
	 * the layout never agreed to.
	 */
	import type { Prop } from '../floorPlan';
	import CoffeeMachine from './CoffeeMachine.svelte';
	import MeetingTable from './MeetingTable.svelte';
	import PantryCounter from './PantryCounter.svelte';
	import Plant from './Plant.svelte';
	import Sofa from './Sofa.svelte';
	import WaterCooler from './WaterCooler.svelte';

	export let prop: Prop;
</script>

<g transform="translate({prop.x}, {prop.y}){prop.flip ? ' scale(-1,1)' : ''}">
	{#if prop.kind === 'rug'}
		<!-- Flat tint, NO outline. Outlined it stopped being floor and became a
		     card drawn around the person and their desk, which is the single
		     loudest cue that a figure and their furniture are one object. A rug
		     is something people sit on top of, so it is drawn as ground. -->
		<rect
			x={-prop.w / 2}
			y={-prop.h / 2}
			width={prop.w}
			height={prop.h}
			rx="4"
			fill="var(--office-rug)"
			opacity="0.15"
		/>
	{:else if prop.kind === 'plant'}
		<Plant />
	{:else if prop.kind === 'plant-tall'}
		<Plant tall />
	{:else if prop.kind === 'cooler'}
		<WaterCooler />
	{:else if prop.kind === 'coffee'}
		<CoffeeMachine />
	{:else if prop.kind === 'counter'}
		<PantryCounter width={prop.w} />
	{:else if prop.kind === 'sofa'}
		<Sofa width={prop.w} />
	{:else if prop.kind === 'meeting-table'}
		<MeetingTable width={prop.w} height={prop.h} />
	{:else if prop.kind === 'lounge-table'}
		<ellipse cx="0" cy="2" rx={prop.w / 2} ry={prop.h / 2 - 4} fill="var(--office-desk-edge)" />
		<ellipse cx="0" cy="-2" rx={prop.w / 2} ry={prop.h / 2 - 4} fill="var(--office-desk)" />
		<circle cx="-6" cy="-4" r="3.6" fill="var(--office-paper)" />
		<rect x="2" y="-8" width="14" height="10" rx="1.5" fill="var(--office-paper)" opacity="0.9" />
	{:else if prop.kind === 'whiteboard'}
		<rect x={-prop.w / 2} y={-prop.h / 2} width={prop.w} height={prop.h} rx="2" fill="var(--office-board)" />
		<rect
			x={-prop.w / 2}
			y={prop.h / 2 - 4}
			width={prop.w}
			height="4"
			rx="1.5"
			fill="var(--office-metal-dark)"
		/>
		<g stroke="var(--office-screen)" stroke-width="1.4" stroke-linecap="round" opacity="0.5">
			<path d="M{-prop.w / 2 + 12},-4 L{-prop.w / 2 + 52},-4" />
			<path d="M{-prop.w / 2 + 12},1 L{-prop.w / 2 + 38},1" />
			<path d="M{prop.w / 2 - 54},-3 L{prop.w / 2 - 22},-3" />
		</g>
		<rect x="-8" y={prop.h / 2 - 3.5} width="10" height="2.5" rx="1" fill="#c4564f" />
	{:else if prop.kind === 'cabinet'}
		<rect x={-prop.w / 2} y={-prop.h / 2} width={prop.w} height={prop.h} rx="2" fill="var(--office-desk-edge)" />
		<rect
			x={-prop.w / 2}
			y={-prop.h / 2}
			width={prop.w}
			height={prop.h / 2}
			rx="2"
			fill="var(--office-desk)"
		/>
		{#each Array.from({ length: Math.max(2, Math.round(prop.w / 40)) }, (_, i) => i) as i (i)}
			<rect
				x={-prop.w / 2 + 5 + (i * (prop.w - 10)) / Math.max(2, Math.round(prop.w / 40))}
				y={-2}
				width={(prop.w - 10) / Math.max(2, Math.round(prop.w / 40)) - 4}
				height="3"
				rx="1.5"
				fill="var(--office-metal)"
			/>
		{/each}
	{:else if prop.kind === 'printer'}
		<rect x={-prop.w / 2} y={-prop.h / 2 + 4} width={prop.w} height={prop.h - 4} rx="2.5" fill="var(--office-metal-dark)" />
		<rect x={-prop.w / 2 + 3} y={-prop.h / 2} width={prop.w - 6} height="7" rx="1.5" fill="var(--office-paper)" />
		<rect x={-prop.w / 2 + 5} y={-1} width={prop.w - 10} height="3" rx="1.5" fill="var(--office-screen)" />
		<circle cx={prop.w / 2 - 6} cy={prop.h / 2 - 5} r="1.8" fill="var(--office-screen-glow)" />
	{:else if prop.kind === 'stacked-chairs'}
		<g opacity="0.9">
			<rect x={-prop.w / 2 + 2} y="6" width={prop.w - 4} height="7" rx="3" fill="var(--office-chair-dark)" />
			<rect x={-prop.w / 2 + 4} y="-2" width={prop.w - 8} height="7" rx="3" fill="var(--office-chair)" />
			<rect x={-prop.w / 2 + 6} y="-10" width={prop.w - 12} height="7" rx="3" fill="var(--office-chair-dark)" />
			<rect x={-prop.w / 2 + 8} y={-prop.h / 2} width="4" height="20" rx="2" fill="var(--office-chair)" />
			<rect x={prop.w / 2 - 12} y={-prop.h / 2} width="4" height="20" rx="2" fill="var(--office-chair)" />
		</g>
	{:else if prop.kind === 'boxes'}
		<rect x={-prop.w / 2} y={-prop.h / 2 + 6} width={prop.w * 0.55} height={prop.h - 6} rx="1.5" fill="var(--office-carton)" />
		<rect x={-prop.w / 2} y={-prop.h / 2 + 6} width={prop.w * 0.55} height="4" fill="var(--office-carton-dark)" />
		<rect x={prop.w / 2 - prop.w * 0.42} y={-prop.h / 2} width={prop.w * 0.42} height={prop.h} rx="1.5" fill="var(--office-carton-dark)" />
		<rect x={prop.w / 2 - prop.w * 0.42} y={-prop.h / 2} width={prop.w * 0.42} height="4" fill="var(--office-carton)" />
	{/if}
</g>
