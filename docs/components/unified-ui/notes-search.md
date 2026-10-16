# Notes on `/notes`

One page for the notes folder, with search in the page toolbar. Dictation
transcripts are Markdown files under `Audio Notes/`, marked with a voice icon,
so they need no second tab.

## The toolbar

The header is one row: title, search field, keep-recordings control, refresh.
The field searches as you type after a 200 ms pause (Enter runs at once) and has
no search button; a clear mark empties the query and results. In flight, the
results box says "Searching notes…"; when done it shows the match count and
time, for example `4 results · 38 ms`; every search asks for the 50-hit
maximum, and the count reads `50+` when more matched. The list has a
fixed height and scrolls, so the folder and reading panes stay in place. Each
hit is one row: note title above the path, the file name as a link that opens
the note in the reading pane, and matching lines under it with line numbers in
a fixed column. The folder list is not filtered. On Android and iOS, typing in
the header search closes the folder panel, and using that panel leaves the
search field so the keyboard closes; the tapped folder stays marked with the
theme accent.

Hits are labeled Notes folder or Local Markdown only when one search mixes the
two providers.

Search runs against the on-disk notes LanceDB table and its full-text index.
Every word must appear somewhere in the note: `Curated links` finds a note
containing both words even when they are not in its title. A hit under
`Audio Notes/` carries the voice icon.

## The notes folder

Beneath the results, `/notes` lists the notes root as folders and Markdown
files. The folder pane sticks to the top of the page and scrolls inside itself.
A folder expands to its children; a file opens in the reading pane, where a find
box marks matches. The pane renders Markdown (headings, lists, quotes, tables,
fenced code). A wiki link (`[[Inbox]]` or `[[Page|label]]`) or a Markdown link
to another `.md` file opens that note in the same pane, and Backlinks lists the
notes that point at it. The page fetches one directory at a time
(`GET /notes/tree`), one file (`GET /notes/file`), and the backlinks of the
open file (`GET /notes/backlinks`).

The `Audio Notes` folder and its transcripts show a voice icon. When a saved
recording exists for the open transcript, the pane can play it. Recordings not
yet uploaded stay in the pending list on this page.

## What a result carries

Each hit shows the note's title, path, provider, and the matching lines with
query terms marked. Clicking a hit opens the note in the reading pane.

## What the page refuses to smooth over

Three states are stated plainly, because a quiet UI would tell the owner
something false:

- **A truncated scan** (`scan_truncated`) says the search stopped before the end
  of the space and a missing note may still exist.
- **A capped result set** (`more_available`) shows the count as `50+`, so the
  owner knows more notes matched than were returned.
- **An empty result** says nothing matched that wording, not that the owner
  never wrote it down. Keyword hits require every term; a term of 3–5
  characters still matches within one edit and a longer one within two
  (`term_in_text` in `magician/src/magician_v2/notes.rs`); related notes are
  included when the embedding model is available.

## Implementation notes

Matched terms are marked by splitting each line into plain and matched segments
(`highlightSegments`), never by building an HTML string, so note content is
never hand-escaped. Overlapping terms (`note` inside `notes`) merge into one
span.

A newer search aborts the one in flight, so a slow earlier response cannot land
on top; an abort is not shown as an error.

Backend contract: [notes provider](../magician/notes-provider.md).
