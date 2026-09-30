/**
 * 24×24 stroke icon paths (Lucide-sourced, ISC license). Rendered via
 * Icon.svelte with currentColor, 1.5px stroke, round caps/joins — works in
 * every theme for free. Data model: each icon is an array of SVG path `d`
 * strings on a 24×24 grid. Lucide primitives (circle/rect/line/polyline)
 * are converted to exact path equivalents (arcs for circles, rounded-corner
 * arc paths for rects) so a single `<path>` renderer covers everything.
 *
 * Icon path data © Lucide Contributors — ISC license,
 * https://github.com/lucide-icons/lucide/blob/main/LICENSE
 */
export const ICON_PATHS = {
	calendar: [
		'M8 2v4',
		'M16 2v4',
		// rect x=3 y=4 w=18 h=18 rx=2
		'M5 4h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z',
		'M3 10h18'
	],
	flag: ['M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z', 'M4 22v-7'],
	'dots-vertical': [
		// three r=1 circles at (12,5) (12,12) (12,19)
		'M13 5a1 1 0 1 1-2 0 1 1 0 1 1 2 0',
		'M13 12a1 1 0 1 1-2 0 1 1 0 1 1 2 0',
		'M13 19a1 1 0 1 1-2 0 1 1 0 1 1 2 0'
	],
	'dots-horizontal': [
		// three r=1 circles at (5,12) (12,12) (19,12)
		'M6 12a1 1 0 1 1-2 0 1 1 0 1 1 2 0',
		'M13 12a1 1 0 1 1-2 0 1 1 0 1 1 2 0',
		'M20 12a1 1 0 1 1-2 0 1 1 0 1 1 2 0'
	],
	sparkle: [
		'M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z'
	],
	clock: [
		// circle cx=12 cy=12 r=10
		'M22 12a10 10 0 1 1-20 0 10 10 0 1 1 20 0',
		'M12 6v6l4 2'
	],
	check: ['M20 6 9 17l-5-5'],
	x: ['M18 6 6 18', 'M6 6l12 12'],
	alert: [
		'M21.73 18l-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3',
		'M12 9v4',
		'M12 17h.01'
	],
	info: [
		// circle cx=12 cy=12 r=10
		'M22 12a10 10 0 1 1-20 0 10 10 0 1 1 20 0',
		'M12 16v-4',
		'M12 8h.01'
	],
	'arrow-right': ['M5 12h14', 'M12 5l7 7-7 7'],
	'arrow-up-right': ['M7 7h10v10', 'M7 17 17 7'],
	'chevron-down': ['M6 9l6 6 6-6'],
	'chevron-up': ['M18 15l-6-6-6 6'],
	'chevron-left': ['M15 18l-6-6 6-6'],
	'chevron-right': ['M9 18l6-6-6-6'],
	'chevrons-left': ['M11 17l-5-5 5-5', 'M18 17l-5-5 5-5'],
	'chevrons-right': ['M6 17l5-5-5-5', 'M13 17l5-5-5-5'],
	play: [
		// polygon 6,3 20,12 6,21
		'M6 3l14 9-14 9z'
	],
	pause: ['M8 5v14', 'M16 5v14'],
	send: ['M22 2 11 13', 'M22 2 15 22l-4-9-9-4z'],
	square: [
		// rect x=3 y=3 w=18 h=18 rx=2 (stop)
		'M5 3h14a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z'
	],
	'rotate-ccw': ['M3 12a9 9 0 1 0 9-9 9.75 9.75 0 0 0-6.74 2.74L3 8', 'M3 3v5h5'],
	plus: ['M5 12h14', 'M12 5v14'],
	search: [
		// circle cx=11 cy=11 r=8
		'M19 11a8 8 0 1 1-16 0 8 8 0 1 1 16 0',
		'M21 21l-4.3-4.3'
	],
	settings: [
		'M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z',
		// circle cx=12 cy=12 r=3
		'M15 12a3 3 0 1 1-6 0 3 3 0 1 1 6 0'
	],
	eye: [
		'M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7z',
		// circle cx=12 cy=12 r=3
		'M15 12a3 3 0 1 1-6 0 3 3 0 1 1 6 0'
	],
	pencil: ['M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5z'],
	'file-text': [
		'M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7z',
		'M14 2v4a2 2 0 0 0 2 2h4',
		'M10 9H8',
		'M16 13H8',
		'M16 17H8'
	],
	'git-branch': [
		'M6 3v12',
		// circle cx=18 cy=6 r=3
		'M21 6a3 3 0 1 1-6 0 3 3 0 1 1 6 0',
		// circle cx=6 cy=18 r=3
		'M9 18a3 3 0 1 1-6 0 3 3 0 1 1 6 0',
		'M18 9a9 9 0 0 1-9 9'
	],
	zap: [
		// polygon 13,2 3,14 12,14 11,22 21,10 12,10 (live)
		'M13 2 3 14h9l-1 8 10-12h-9l1-8z'
	],
	inbox: [
		'M22 12h-6l-2 3h-4l-2-3H2',
		'M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z'
	],
	moon: [
		// snooze
		'M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9z'
	],
	archive: [
		// rect x=2 y=3 w=20 h=5 rx=1 (dismiss/hide)
		'M3 3h18a1 1 0 0 1 1 1v3a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z',
		'M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8',
		'M10 12h4'
	],
	message: [
		// message-square
		'M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z'
	],
	monitor: [
		// rect x=2 y=3 w=20 h=14 rx=2
		'M4 3h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z',
		'M8 21h8',
		'M12 17v4'
	],
	tablet: [
		// rect x=4 y=2 w=16 h=20 rx=2
		'M6 2h12a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z',
		'M12 18h.01'
	],
	smartphone: [
		// rect x=5 y=2 w=14 h=20 rx=2
		'M7 2h10a2 2 0 0 1 2 2v16a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2z',
		'M12 18h.01'
	],
	mic: [
		'M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3z',
		'M19 10v2a7 7 0 0 1-14 0v-2',
		'M12 19v3',
		'M8 22h8'
	],
	sliders: [
		'M4 21v-7',
		'M4 10V3',
		'M12 21v-9',
		'M12 8V3',
		'M20 21v-5',
		'M20 12V3',
		'M1 14h6',
		'M9 8h6',
		'M17 16h6'
	]
} as const;

export type IconName = keyof typeof ICON_PATHS;
