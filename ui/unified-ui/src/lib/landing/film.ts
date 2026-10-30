// The intro film (unlisted on YouTube). Opened from the hero's "Watch the film"
// link and from magican.ai/#film; played from youtube-nocookie.com only after a
// visitor asks for it.
export const FILM_ID = 'aCqym-rcVu0';
export const FILM_TITLE = 'Magican: Life assistant for work, play, and everything else, the movie';
export const FILM_HASH = '#film';
export const filmEmbedUrl = (id = FILM_ID): string =>
	`https://www.youtube-nocookie.com/embed/${id}?autoplay=1&rel=0&playsinline=0&modestbranding=1`;
