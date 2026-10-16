import { writable } from 'svelte/store';

/**
 * Floating app chrome (currently the chat bubble) yields to
 * immersive surfaces: while the /square game hero is in the viewport (or
 * immersed) the chrome fades out so it never covers game HUD panels; it
 * fades back in when the surface scrolls away.
 *
 * Owned by the surface that wants the screen (set true/false); the (app)
 * layout consumes it. Surfaces MUST reset to false on destroy.
 */
export const floatingChromeHidden = writable(false);
