/**
 * True solar & lunar positions for an observer (SunCalc-style equations,
 * after Astronomy Answers / Meeus simplifications — plenty accurate for a
 * game sky). Returns altitude (rad above horizon) and azimuth (rad, measured
 * from SOUTH, positive westward — the classic convention; the environment
 * converts to scene axes).
 */

const RAD = Math.PI / 180;
const OBLIQUITY = 23.4397 * RAD; // Earth's axial tilt

/** Days since J2000.0 (2000-01-01 12:00 UTC). */
function toDays(ms: number): number {
	return ms / 86_400_000 - 10957.5;
}

function rightAscension(eclLon: number, eclLat: number): number {
	return Math.atan2(
		Math.sin(eclLon) * Math.cos(OBLIQUITY) - Math.tan(eclLat) * Math.sin(OBLIQUITY),
		Math.cos(eclLon)
	);
}

function declination(eclLon: number, eclLat: number): number {
	return Math.asin(
		Math.sin(eclLat) * Math.cos(OBLIQUITY) +
			Math.cos(eclLat) * Math.sin(OBLIQUITY) * Math.sin(eclLon)
	);
}

function siderealTime(d: number, lw: number): number {
	return RAD * (280.16 + 360.9856235 * d) - lw;
}

function altitudeOf(H: number, phi: number, dec: number): number {
	return Math.asin(
		Math.sin(phi) * Math.sin(dec) + Math.cos(phi) * Math.cos(dec) * Math.cos(H)
	);
}

function azimuthOf(H: number, phi: number, dec: number): number {
	return Math.atan2(
		Math.sin(H),
		Math.cos(H) * Math.sin(phi) - Math.tan(dec) * Math.cos(phi)
	);
}

export interface SkyPosition {
	/** Radians above the horizon (negative = below). */
	altitude: number;
	/** Radians from SOUTH, positive westward. */
	azimuth: number;
}

export function sunPosition(ms: number, lat: number, lon: number): SkyPosition {
	const lw = -lon * RAD;
	const phi = lat * RAD;
	const d = toDays(ms);

	const M = RAD * (357.5291 + 0.98560028 * d); // solar mean anomaly
	const C = RAD * (1.9148 * Math.sin(M) + 0.02 * Math.sin(2 * M) + 0.0003 * Math.sin(3 * M));
	const L = M + C + RAD * 102.9372 + Math.PI; // ecliptic longitude

	const dec = declination(L, 0);
	const ra = rightAscension(L, 0);
	const H = siderealTime(d, lw) - ra;
	return { altitude: altitudeOf(H, phi, dec), azimuth: azimuthOf(H, phi, dec) };
}

export function moonPosition(ms: number, lat: number, lon: number): SkyPosition {
	const lw = -lon * RAD;
	const phi = lat * RAD;
	const d = toDays(ms);

	const L = RAD * (218.316 + 13.176396 * d); // ecliptic longitude
	const M = RAD * (134.963 + 13.064993 * d); // mean anomaly
	const F = RAD * (93.272 + 13.22935 * d); // mean distance argument

	const eclLon = L + RAD * 6.289 * Math.sin(M);
	const eclLat = RAD * 5.128 * Math.sin(F);

	const dec = declination(eclLon, eclLat);
	const ra = rightAscension(eclLon, eclLat);
	const H = siderealTime(d, lw) - ra;
	return { altitude: altitudeOf(H, phi, dec), azimuth: azimuthOf(H, phi, dec) };
}
