// The Meetings surface was folded into /observe (one page for everything
// Presto's senses do — listen / join / watch screen). This index redirects
// so muscle memory and deep links survive; the /meetings/[thread]
// transcript detail page stays where it is.
import { redirect } from '@sveltejs/kit';

export function load(): never {
	redirect(308, '/observe');
}
