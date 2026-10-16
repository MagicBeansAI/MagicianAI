import type { WorldPalette } from './types';

/**
 * Read the world palette from the --fleet-* game-theme tokens resolved on the
 * hero element (which carries data-game-theme). Values are plain hex so
 * THREE.Color can parse them directly.
 */
export function readWorldPalette(host: HTMLElement): WorldPalette {
	const cs = getComputedStyle(host);
	const g = (name: string, fb: string): string => cs.getPropertyValue(name).trim() || fb;
	return {
		skyTop: g('--fleet-sky-top', '#95c1dc'),
		skyBottom: g('--fleet-sky-bottom', '#cae0ed'),
		ground: g('--fleet-ground', '#a0ce7e'),
		groundAlt: g('--fleet-ground-alt', '#81b45e'),
		path: g('--fleet-path', '#e4dcd0'),
		plaza: g('--fleet-plaza', '#e4dcd0'),
		foliage: g('--fleet-foliage', '#2f8b46'),
		trunk: g('--fleet-trunk', '#7d5b46'),
		rock: g('--fleet-rock', '#7e8e95'),
		wall: g('--fleet-wall', '#fbfaf6'),
		roof: g('--fleet-roof', '#c77919'),
		roofHall: g('--fleet-roof-hall', '#2f6fb8'),
		ink: g('--fleet-ink', '#1f2b33'),
		handoff: g('--fleet-handoff', '#ff89cb'),
		status: {
			working: g('--fleet-working', '#20773d'),
			needs: g('--fleet-needs', '#ae6500'),
			paused: g('--fleet-paused', '#8b5cf6'),
			idle: g('--fleet-idle', '#0078a4'),
			offline: g('--fleet-offline', '#4f5860')
		}
	};
}

/**
 * Guild banner hues (roofs, banners, citizen tunics). These identify one guild
 * against another, so they vary by HUE at a similar value — unlike the --fleet-*
 * tokens, which have to separate from the terrain by value. There is one world
 * now, so there is nothing to key a per-theme list off; a fixed set is the whole
 * design.
 */
export const GUILD_HUES = ['#b03a48', '#3d6fb4', '#3e8e5a', '#c9a227', '#7c4fa0', '#2f8f8f'];

export function guildHue(index: number): string {
	return GUILD_HUES[index % GUILD_HUES.length];
}
