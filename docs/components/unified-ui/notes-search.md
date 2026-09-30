# Notes on `/notes`

One page for the notes folder. Search sits in the page toolbar. Dictation
transcripts are Markdown files under `Audio Notes/`, marked with a voice icon,
so they do not need a second tab.

## The toolbar

The header is one short row: the title, the search field, the keep-recordings
control, and refresh. The field searches as you type, after a short pause, and
has no search button. Once it contains text, a clear mark inside the field
empties the query and the results. While the request is in flight the results box says "Searching notes…".
When it finishes, that box shows how many notes matched and how long the
search took, for example `4 results · 38 ms`. More matches than the cap
are shown as `50+`. The list itself stays a fixed height and scrolls, so
the folder pane and the reading pane are not pushed down the page. Each hit
is one row, separated from the next by a divider. The note title sits above
the path. The file name is a link, and that link opens the note in the
reading pane. Matching lines sit under the path, with the line number in a
fixed column so the snippets start at the same place. The folder list is not filtered. On Android and iOS, typing in the header
search closes the folder panel. Opening that panel, or using it, leaves the
search field so the keyboard closes. The folder that was tapped stays marked with
the theme accent.

A hit from the notes folder is labeled Notes folder, and a hit from Local
Markdown is labeled Local Markdown. The label is shown only when one search
mixes the two. A library that uses one provider does not repeat the name on
every row.

The search runs against the on-disk notes LanceDB table and the full-text
index stored in it. Every word has to appear
somewhere in the note. A search for `Curated links`
finds a note that contains both words, such as the line "The curated links
above…", even when those words are not the note's title. Clicking the result
opens that note.

A hit whose path is under `Audio Notes/` carries the same voice
icon as that folder.

## The notes folder

Beneath the results, `/notes` lists the notes root as folders and Markdown
files. The folder pane stays on screen while the note scrolls: it sticks to
the top of the page and scrolls inside itself when the list is taller than
the viewport. A folder expands to its own children. A file opens in the reading pane,
where a find box marks matches in the rendered note. The pane renders
Markdown: headings, lists, quotes, tables, and fenced code. A wiki link
(`[[Inbox]]` or `[[Page|label]]`) and a Markdown link to another `.md` file
open that note in the same pane. Under the note, Backlinks lists the other
notes that point at it. The page asks the server for
one directory at a time (`GET /notes/tree`), one file (`GET /notes/file`),
and the notes that link to the open file (`GET /notes/backlinks`).

The `Audio Notes` folder and every note inside it show a voice icon. Those
files are the transcripts. When a saved recording exists for the open
transcript, the reading pane can play it. Recordings that have not uploaded
yet stay in the pending list on this page.

## What a result carries

Each hit shows the note's title, its path, which provider it lives in, and the
lines that matched with the query terms marked. That is usually enough to answer
without opening anything, which is the point — a search that only returns
filenames makes the owner open five notes to find one sentence.

Clicking a hit opens that note in the reading pane on this page.

## What the page refuses to smooth over

Three states are stated plainly rather than hidden, because in each case a quiet
UI would tell the owner something false:

- **A truncated scan** (`scan_truncated`) says the search stopped before the end
  of the space and that a missing note may still exist. A partial answer
  presented as a complete one is worse than no answer.
- **A capped result set** (`more_available`) says more notes matched than were
  returned, and offers to raise the cap.
- **An empty result** says nothing matched that wording. Keyword hits still
  require every term, and a typo can still match. Related notes are included
  when embeddings are stored. "Nothing matched" and "you never wrote this
  down" are different claims, and only the first one is true.

## Implementation notes

Matched terms are marked by splitting each line into plain and matched segments
(`highlightSegments`) rather than assembling an HTML string. Building markup here
would mean hand-escaping note content, and a note is exactly the place where
someone's angle brackets are their own text. Overlapping terms — `note` inside
`notes` — merge into one span instead of nesting.

A search in flight is aborted when a newer one starts, so a slow earlier response
cannot land on top of a newer one. An abort is not surfaced as an error; it means
a newer search superseded this one. Typing waits out a short pause before the
request, so a fast typist does not send one search per character. Enter runs the
current text immediately.

Backend contract: [notes provider](../magician/notes-provider.md).
