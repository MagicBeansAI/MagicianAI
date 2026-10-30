import type { Reroute } from '@sveltejs/kit';
import { embeddedSettingsReroute } from '$lib/shell/embeddedSettings';

// Embedded Settings keeps its URL but is served without the app shell.
export const reroute: Reroute = ({ url }) => embeddedSettingsReroute(url);
