/**
 * Pure time-of-day greeting for the Today header kicker.
 *
 * Dayparts (local time):
 * - 05:00-11:59 → "Good morning"
 * - 12:00-17:59 → "Good afternoon"
 * - 18:00-04:59 → "Good evening" (late-night hours read as evening, not
 *   morning — nobody wants "Good morning" at 2 AM)
 *
 * `now` is injected — no `Date.now()` inside — so callers and tests control
 * the clock. The full local weekday and date are rendered separately below
 * the Today title, so the kicker intentionally contains only the daypart.
 */
export function greetingFor(now: Date): string {
	const hour = now.getHours();
	const daypart = hour >= 5 && hour < 12 ? 'morning' : hour >= 12 && hour < 18 ? 'afternoon' : 'evening';
	return `Good ${daypart}`;
}
