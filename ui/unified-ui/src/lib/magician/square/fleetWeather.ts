import type { WeatherState } from './engine/environment';

/**
 * Real local weather for the game's environment, from Open-Meteo (free, no
 * API key) at the browser's geolocation. Everything degrades gracefully:
 * geolocation denied / offline / API down -> null (the game keeps its last
 * state, defaulting to a mostly-clear sky). WMO weather codes map to the
 * engine's simple kinds.
 */

function currentPosition(timeoutMs = 8000): Promise<GeolocationPosition | null> {
	return new Promise((resolve) => {
		if (typeof navigator === 'undefined' || !navigator.geolocation) {
			resolve(null);
			return;
		}
		navigator.geolocation.getCurrentPosition(
			(pos) => resolve(pos),
			() => resolve(null),
			{ timeout: timeoutMs, maximumAge: 30 * 60_000 }
		);
	});
}

let coordsCache: Promise<{ lat: number; lon: number } | null> | null = null;

/** Observer coordinates, shared by the weather fetch and the environment's
 * TRUE solar/lunar positions — one geolocation prompt for both. */
export function getObserverCoords(): Promise<{ lat: number; lon: number } | null> {
	if (!coordsCache) {
		coordsCache = currentPosition().then((pos) =>
			pos ? { lat: pos.coords.latitude, lon: pos.coords.longitude } : null
		);
	}
	return coordsCache;
}

function kindFromWmo(code: number): WeatherState['kind'] {
	if ((code >= 51 && code <= 67) || (code >= 80 && code <= 82) || code >= 95) return 'rain';
	if ((code >= 71 && code <= 77) || code === 85 || code === 86) return 'snow';
	if (code >= 1) return 'clouds';
	return 'clear';
}

/** Precipitation strength 0..1 from the WMO code's own light/moderate/heavy
 * bands — a drizzle and a thunderstorm no longer look identical. */
function intensityFromWmo(code: number): number {
	const table: Record<number, number> = {
		51: 0.25, 53: 0.35, 55: 0.45, // drizzle: light/moderate/dense
		56: 0.3, 57: 0.45, // freezing drizzle
		61: 0.4, 63: 0.6, 65: 0.85, // rain: slight/moderate/heavy
		66: 0.5, 67: 0.8, // freezing rain
		80: 0.5, 81: 0.7, 82: 1.0, // rain showers: slight/moderate/violent
		95: 0.9, 96: 1.0, 99: 1.0, // thunderstorm (+hail)
		71: 0.35, 73: 0.55, 75: 0.8, // snowfall: slight/moderate/heavy
		77: 0.3, // snow grains
		85: 0.5, 86: 0.75 // snow showers
	};
	return table[code] ?? 0;
}

export async function fetchLocalWeather(): Promise<WeatherState | null> {
	try {
		const coords = await getObserverCoords();
		if (!coords) return null;
		const url =
			`https://api.open-meteo.com/v1/forecast?latitude=${coords.lat.toFixed(3)}` +
			`&longitude=${coords.lon.toFixed(3)}&current=weather_code,cloud_cover`;
		const res = await fetch(url, { signal: AbortSignal.timeout(10_000) });
		if (!res.ok) return null;
		const body = (await res.json()) as {
			current?: { weather_code?: number; cloud_cover?: number };
		};
		const code = body.current?.weather_code ?? 0;
		const cover = (body.current?.cloud_cover ?? 15) / 100;
		return {
			kind: kindFromWmo(code),
			cloudCover: Math.max(0.05, Math.min(1, cover)),
			intensity: intensityFromWmo(code)
		};
	} catch {
		return null;
	}
}
