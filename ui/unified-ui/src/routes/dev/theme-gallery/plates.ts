import type { VALID_THEMES } from '$lib/shared/stores/themeStore';

/** One theme, as the specimen sheet documents it. */
export interface Plate {
	id: (typeof VALID_THEMES)[number];
	name: string;
	description: string;
	/** Family names as set in app.css, so the plate documents its own voices. */
	fonts: { brand: string; display: string; primary: string; mono: string };
}

/** A light plate and its dark sibling. */
export interface Family {
	name: string;
	plates: [Plate, Plate];
}

const outfit = { brand: 'Outfit', display: 'Outfit', primary: 'Manrope', mono: 'Geist Mono' };

export const families: Family[] = [
	{
		name: 'Longhand',
		plates: [
			{ id: 'longhand', name: 'Longhand', description: 'Hand-printed cream & ink', fonts: outfit },
			{ id: 'longhand-dark', name: 'Longhand Dark', description: 'Warm graphite paper, cream ink', fonts: outfit }
		]
	},
	{
		name: 'Soft Machine',
		plates: [
			{ id: 'soft-machine', name: 'Soft Machine', description: 'Warm & approachable', fonts: { brand: 'Outfit', display: 'Space Grotesk', primary: 'Quicksand', mono: 'JetBrains Mono' } },
			{ id: 'soft-machine-dark', name: 'Soft Machine Dark', description: 'Warm after dark', fonts: { brand: 'Outfit', display: 'Fredoka', primary: 'Quicksand', mono: 'JetBrains Mono' } }
		]
	},
	{
		name: 'Bubbly',
		plates: [
			{ id: 'bubbly', name: 'Bubbly', description: 'Cream canvas, coral bubbles, teal float', fonts: { brand: 'Outfit', display: 'Fredoka', primary: 'Quicksand', mono: 'JetBrains Mono' } },
			{ id: 'bubbly-dark', name: 'Bubbly Dark', description: 'Midnight cream ink with coral + teal glow', fonts: { brand: 'Outfit', display: 'Fredoka', primary: 'Quicksand', mono: 'JetBrains Mono' } }
		]
	},
	{
		name: 'Arcane Terminal',
		plates: [
			{ id: 'arcane-terminal-light', name: 'Arcane Terminal Light', description: 'Day-mode developer notebook', fonts: { brand: 'Outfit', display: 'Fira Code', primary: 'IBM Plex Mono', mono: 'Fira Code' } },
			{ id: 'arcane-terminal', name: 'Arcane Terminal', description: 'Dark & mystical', fonts: { brand: 'Outfit', display: 'Fira Code', primary: 'IBM Plex Mono', mono: 'Fira Code' } }
		]
	},
	{
		name: 'Retro 16-bit',
		plates: [
			{ id: 'retro-16bit-light', name: 'Retro Light', description: 'Paper ASCII', fonts: { brand: 'Outfit', display: 'IBM Plex Mono', primary: 'JetBrains Mono', mono: 'JetBrains Mono' } },
			{ id: 'retro-16bit', name: 'Retro Dark', description: 'Amber phosphor on warm black', fonts: { brand: 'Outfit', display: 'IBM Plex Mono', primary: 'JetBrains Mono', mono: 'IBM Plex Mono' } }
		]
	},
	{
		name: 'Mario 8-bit',
		plates: [
			{ id: 'mario-8bit', name: 'Mario 8-bit', description: 'NES sky overworld', fonts: { brand: 'Outfit', display: 'Press Start 2P', primary: 'Pixelify Sans', mono: 'Press Start 2P' } },
			{ id: 'mario-8bit-dark', name: 'Mario Underground', description: 'Castle dungeon, coin glow', fonts: { brand: 'Outfit', display: 'Press Start 2P', primary: 'Pixelify Sans', mono: 'Press Start 2P' } }
		]
	},
	{
		name: 'Risograph',
		plates: [
			{ id: 'risograph', name: 'Risograph', description: 'Indie spot-colour print, fluorescent pink', fonts: { brand: 'Outfit', display: 'Bricolage Grotesque', primary: 'Manrope', mono: 'JetBrains Mono' } },
			{ id: 'risograph-dark', name: 'Risograph Dark', description: 'Fluo pink on warm graphite', fonts: { brand: 'Outfit', display: 'Bricolage Grotesque', primary: 'Manrope', mono: 'JetBrains Mono' } }
		]
	},
	{
		name: 'Mixtape',
		plates: [
			{ id: 'mixtape', name: 'Mixtape', description: 'Mustard label, sharpie marker', fonts: { brand: 'Outfit', display: 'Permanent Marker', primary: 'Special Elite', mono: 'IBM Plex Mono' } },
			{ id: 'mixtape-dark', name: 'Mixtape (Side B)', description: 'Cassette body, tape oxide', fonts: { brand: 'Outfit', display: 'Permanent Marker', primary: 'Special Elite', mono: 'IBM Plex Mono' } }
		]
	},
	{
		name: 'Mono',
		plates: [
			{ id: 'mono', name: 'Mono', description: 'Pure grayscale, black on white', fonts: { brand: 'Outfit', display: 'Space Grotesk', primary: 'Inter', mono: 'JetBrains Mono' } },
			{ id: 'mono-dark', name: 'Mono Dark', description: 'Pure grayscale, white on black', fonts: { brand: 'Outfit', display: 'Space Grotesk', primary: 'Inter', mono: 'JetBrains Mono' } }
		]
	},
	{
		name: '2D Cartoon',
		plates: [
			{ id: 'cartoon', name: '2D Cartoon', description: 'Sky-blue clouds, sticker outlines', fonts: { brand: 'Outfit', display: 'Lilita One', primary: 'Fredoka', mono: 'JetBrains Mono' } },
			{ id: 'cartoon-dark', name: '2D Cartoon Night', description: 'Starry purple sky, cream cel-ink', fonts: { brand: 'Outfit', display: 'Lilita One', primary: 'Fredoka', mono: 'JetBrains Mono' } }
		]
	},
	{
		name: 'Jarvis',
		plates: [
			{ id: 'jarvis-light', name: 'Jarvis Light', description: 'HUD aesthetic on icy white', fonts: { brand: 'Outfit', display: 'Rajdhani', primary: 'Manrope', mono: 'JetBrains Mono' } },
			{ id: 'jarvis', name: 'Jarvis', description: 'Iron Man HUD: glass + cyan glow', fonts: { brand: 'Outfit', display: 'Rajdhani', primary: 'Manrope', mono: 'JetBrains Mono' } }
		]
	}
];

/** Every VALID_THEMES entry must have a plate; the test enforces it. */
export const plateIds = families.flatMap((family) => family.plates.map((plate) => plate.id));

export function plateNumber(familyIndex: number, plateIndex: number): string {
	return `${String(familyIndex + 1).padStart(2, '0')}${plateIndex === 0 ? 'a' : 'b'}`;
}

export function isPlateId(value: string | null): value is Plate['id'] {
	return value !== null && (plateIds as string[]).includes(value);
}
