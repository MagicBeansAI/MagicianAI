---
name: comic-strip
version: 0.1.1
description: |
  Procedure skill — orchestrate a multi-panel comic strip from a story:
  pick a style, plan N panels, render each via image-generation
  (with image-generation-via-minimax as the safety/quota fallback) and
  character-consistency refs, compose them into an HTML page (optionally
  PDF via headless Chrome). Use when the user asks for a comic, manga,
  bande-dessinée, storyboard, or any multi-panel illustration sequence. No
  Python script — the orchestration runs in the agent's outer loop using
  the image-generation tool + files + shell.
metadata:
  magician:
    skill_type: procedure
    requires:
      python_packages: ["Pillow", "fpdf2"]
---

# Comic Strip — orchestration playbook

> **Lifecycle (read first):** Activate the moment you start planning the comic — not while still clarifying the story. Call `deactivate_skill` the same turn you finish (final artifact saved). Body re-injects every turn while active and costs prompt context.

You're about to turn a short story into a multi-panel comic. The work is orchestrated in your outer loop — there is no single tool that does this end-to-end. You'll plan panels, render each via the image-generation tool, then compose them into a page.

## Pre-flight

You need:
- `image-generation` tool granted (scope-layer skill — confirm via your catalog; if missing, the system has no image-gen credential and you should stop and tell the user).
- `image-generation-via-minimax` tool granted (optional but recommended fallback — see Step 3a below). If only nanobanana is granted, proceed without a fallback and surface the failure cleanly when it happens.
- `files` tool (always available).
- `shell` (only needed if the user wants PDF output — HTML can ship without it).

If image-generation isn't available, **don't** write a markdown spec instead. Stop and tell the user the image-gen credential is missing.

## Step 1 — Pick a style

First infer the sharing context. If the user explicitly names a style, use it.
Otherwise pick a format + style from the audience cues below so the user does
not need to remember style names.

### Audience presets

| Cue in user request | Recommended story format | Panel count | Style defaults | Output defaults |
|---|---|---:|---|---|
| Office colleagues, team, Slack, work update, internal memo | Polished workplace micro-story: setup → tension → insight → payoff. Keep humor gentle and non-embarrassing. For explainers, use problem → discovery → solution → impact. | 4 or 6 | `western-bd` for polished/professional, `tintin` for clean process/explainer, `calvin-hobbes` only for informal warm team culture | HTML + shareable PDF |
| Family, parents, siblings, spouse, relatives, WhatsApp family group | Warm slice-of-life memory: scene → small conflict/funny beat → emotion → affectionate resolution. Local/cultural detail is welcome. | 4 or 6 | `calvin-hobbes` for warm/funny, `ghibli` for sentimental, `chota-bheem` for Indian family-friendly energy | Image/PDF; keep captions short |
| Children, kids, bedtime, school, classroom, nephew/niece | Simple moral/adventure: character wants something → tries → learns → resolves. Use clear facial expressions, bright scenes, no sarcasm. | 4 or 6; 8 for storybook arc | `doraemon` for gentle child-friendly, `chota-bheem` for energetic Indian kids, `ghibli` for quiet wonder | PDF or image pages; very short dialog |
| Teen/YA audience | Character-driven mini-arc with a relatable dilemma and visual momentum. Avoid babyish tone. | 6 or 8 | `manga` for expressive/dramatic, `western-bd` for grounded modern story | HTML/PDF |
| Public social post, LinkedIn, announcement | Clear, visually polished story with one takeaway. Avoid private jokes. | 4 or 6 | `western-bd`, `tintin`, or custom brand style if supplied | HTML + PDF |
| Personal joke / meme-like ask | Punchy 4-beat comic: setup / escalation / twist / tag. | 4 | `calvin-hobbes`, `asterix`, or custom funny style | Image/PDF |

### Format chooser

- `4-panel comic`: fastest and safest default for jokes, observations, office
  sharing, family moments, and children’s morals.
- `6-panel comic`: default when the user gives a story with a beginning,
  middle, and end.
- `8-12 panel storyboard`: use only for a richer narrative, presentation
  storyboard, lesson, or longer children’s story.
- `single-page A4 grid`: best for office/family sharing and printable PDFs.
- `sequential page PDF`: best when each panel should be read slowly like a
  storyboard or children’s picture story.

Tone rules:
- Office: never mock a real colleague, manager, client, or team by name unless
  the user explicitly wants a private inside joke; keep it safe for forwarding.
- Family: preserve warmth; jokes should land on situations, not personal flaws.
- Children: avoid cynicism, fear-heavy scenes, violence, romance, or complex
  irony; prefer curiosity, kindness, courage, honesty, and teamwork.

After choosing the preset, browse the bundled styles by name and read only the
specific YAML you'll use.

**Available styles** (bundled in `{this_skill_dir}/styles/<name>.yaml`):
- `manga` — Japanese manga (B&W or muted, screen-tones, sharp inks, expressive eyes)
- `doraemon` — soft round shapes, bright simple colors, gentle outlines
- `chota-bheem` — Indian TV-cartoon look, bold flat colors, expressive faces
- `tintin` — Hergé ligne-claire, even line weight, flat fills, detailed backgrounds
- `asterix` — Uderzo-style French BD, exaggerated body proportions, bright palette
- `ghibli` — Studio-Ghibli-inspired watercolor, atmospheric backgrounds, soft expressions
- `calvin-hobbes` — Bill-Watterson-ish loose ink, dynamic poses, conversational warmth
- `western-bd` — modern western graphic-novel style, mid-detail line + flat color

For the chosen style, read the YAML: `files(action=read, path={this_skill_dir}/styles/<name>.yaml)`. Extract `prompt_fragment` (style guidance you splice into every panel prompt) and respect `ip_safe_note` (no named IP characters; only generic style).

If the user gave a custom style description, skip the YAML — use their words as the prompt fragment, and apply the universal IP-safety rule yourself.

## Step 1a — Build the character bible (IP-safe paraphrase)

Before planning panels, lock the cast. This step matters because image-gen
safety classifiers (Gemini, MiniMax, DALL-E, Imagen, Midjourney) do a
**fast token-presence scan** on the prompt — they don't parse negation
semantically. Lines like *"do NOT depict Spider-Man"* in the panel prompt
actively make refusal more likely; the words "Spider-Man" / "Marvel" /
"Peter Parker" in any context score as IP-infringement risk and the
model declines (returns `parts=None` / `finish_reason=SAFETY`).

So the rule is: **the panel-render prompt MUST NOT name any copyrighted
character, franchise, studio, brand, named celebrity, or named band.**
Ever. Not in disclaimers. Not in "inspired by" lines. Not anywhere.

The character bible is where you do the translation once, up front:

### When the user names a copyrighted character

Build a bible entry that captures the *vibe* using "like / inspired by"
language for your own reasoning, plus the **pure-visual attributes** the
image model will actually see:

```
{
  "characters": [
    {
      "name": "Miko Webstar",              # your invented name
      "vibe": "like a friendly teen web-slinger hero, classic comic
               archetype — agile, masked, wisecracking",  # bible only
      "visual": "slim masked teenage boy, unique red-and-cyan suit with
                 smooth geometric stripe patterns, large friendly white
                 eye lenses, wrist web launchers, no logos, no spider
                 emblem, no brand markings"               # → prompt body
    },
    {
      "name": "Shadow Slime",
      "vibe": "like an alien-symbiote antagonist, inky and expressive",
      "visual": "glossy black blob creature with two large white expressive
                 eyes, fluid liquid-metal texture, not menacing"
    }
  ]
}
```

The `vibe` field is for YOU — it captures the user's request faithfully
so the story still feels right. The `visual` field is the only thing
that goes into the panel prompt. Surface the bible to the user before
rendering so they can adjust ("call him Miko — works for me" / "make
the suit blue instead of red").

### When the user names an original character or a vibe-only style

(e.g. "a Pixar-style robot painter" / "a brave village girl detective")
No translation needed. Drop a normal `visual` description in the bible
and skip the `vibe` field.

### Hard list — never mention these in panel prompts

Spider-Man, Venom, Marvel, DC, Pixar, Disney, Studio Ghibli, Doraemon,
Naruto, Pikachu, Pokemon, Mario, Sonic, Mickey Mouse, Hello Kitty,
Barbie, James Bond, Sherlock Holmes, Harry Potter, named celebrities,
named bands, named real CEOs — and any close paraphrase of these.

(Style names like `doraemon`, `ghibli`, `chota-bheem` in the STYLE FILE
are a different layer — `styles/<name>.yaml` already paraphrases those
into safe `prompt_fragment` text. Don't re-introduce the brand names
yourself when splicing the fragment.)

## Step 2 — Plan the panels

Decide panel count from story length: 4 (single beat), 6 (default), 8-12 (longer arc). Cap at 12 for sane render time.

For each panel write a structured object:
```
{
  "setting": "<where, time of day, weather>",
  "characters_present": ["<who>", ...],
  "action": "<what is happening in this frame>",
  "mood": "<emotional tone>",
  "camera": "<close-up | medium | wide | dramatic angle>",
  "dialog": "<short line, optional — see Step 5>"
}
```

Sequence rules:
- Panel 1 establishes setting + characters; panel N resolves
- Each panel advances the story; no two redundant beats
- For 4-panel comics, use the classic 4-koma rhythm: setup / development / twist / resolution

## Step 3 — Render the panels

For each panel, call `image-generation`. The prompt
body inlines the **`visual` field** from each character the panel
references — never the `name`, never the `vibe`, never any
copyrighted IP token (see Step 1a's hard list). No negation lines
either; the model never knew about the original IP, so there's
nothing to negate.

```
image-generation(
  prompt="""
  <style_prompt_fragment>

  Setting: <panel.setting>
  Characters in frame: <for each character in panel.characters_present,
                       emit the bible's `visual` field, comma-joined>
  Action: <panel.action — rephrase to use the same visual descriptors,
           not the bible names>
  Mood: <panel.mood>
  Camera: <panel.camera>

  Render rules:
  - No text, speech bubbles, captions, or sound effects in the image
    (those are added in post-composition).
  - No watermarks, signatures, UI elements, or panel borders.
  - Match the style fragment above exactly.
  """,
  aspect_ratio="3:4",
  resolution="1K",
  quality_tier="pro",
  output_path="/tmp/comic-<run-id>-panel-<N>.jpg",
  input_images="<character_refs joined by comma>"   # see Step 4
)
```

Self-check before each panel call: scan the assembled prompt for any
token in Step 1a's hard list (case-insensitive). If a match exists,
something leaked from the user's request — rewrite using the bible's
`visual` field and try again. Do NOT add a "do NOT depict X"
disclaimer to compensate — that's the exact anti-pattern Step 1a
exists to prevent.

Aspect: `3:4` (portrait) is the comic-strip default. Use `4:3` (landscape) for cinematic styles. Use `1:1` for Instagram-style.

Quality: `pro` for final art, `balanced` for drafts. Each `pro` panel costs ~$0.02-0.05 and takes 30-60s.

## Step 3a — Fallback when nanobanana declines

`image-generation` can return one of three failure shapes:

1. **Safety / decline**: response includes `warning: "Model returned no parts — likely declined (content safety, quota, or empty response)."` and a `finish_reason`. No image was written.
2. **Quota / 503 / network**: an `error` field with a status code or transient infra message.
3. **Hard validation**: an `error` like "prompt too long" or "output_path must be within …" — these are caller errors, NOT provider issues; fix the args and retry.

For (1) and (2), fall back to `image-generation-via-minimax` for that panel **only if both conditions hold**:

- The prompt does NOT name a copyrighted character / franchise (Spider-Man, Mario, Mickey, Pikachu, Doraemon-the-character, named bands, named celebrities, etc.). MiniMax will refuse for the same reason — switching providers does NOT bypass copyright restrictions. If the prompt names protected IP, rewrite it with an original character instead (e.g. the comic-strip style files already prescribe original characters inspired by a vibe).
- You've granted `image-generation-via-minimax` in your tools list (see Pre-flight).

Fallback call shape — same `output_path`, same prompt, mapped params:

```
image-generation-via-minimax(
  prompt=<same prompt that nanobanana saw>,
  aspect_ratio="3:4",     # same as the nanobanana call
  output_path="/tmp/comic-<run-id>-panel-<N>.jpg",
  n=1,
  prompt_optimizer=true,  # let MiniMax tighten the prompt on retry
  subject_ref_image="<path to panel-1 or strongest canonical ref>"
                          # see character-consistency note below
)
```

Notes:
- MiniMax `image-01` supports **one** character reference via
  `subject_ref_image` (CLI: `--subject-ref type=character,image=<path>`).
  Pass the panel-1 image (or whichever earlier panel best captures the
  protagonist's canonical look) so the fallback panel preserves the
  recurring character's face — single ref only, unlike nanobanana's
  up-to-4-ref `input_images`. Acceptable trade-off for an emergency
  rescue; secondary characters in the same panel may drift.
- Do NOT loop the fallback. If MiniMax also returns a decline / error
  for the same prompt, surface BOTH provider reasons to the user and
  stop the panel render. Don't keep retrying with the same provider.
- MiniMax `image-01` lineage is different from Nano Banana — visual
  style WILL differ. That's the price of a safety-decline rescue;
  acknowledge it in your turn-end summary if you used it
  (e.g. "Panel 5 declined by Nano Banana; rendered via MiniMax with
  panel-1 as character ref — slightly different rendering style.").

## Step 4 — Character consistency across panels

After panel 1 and 2 render, pass their file paths as `input_images` to panels 3+ (up to 4 refs). Nano Banana 2 anchors character look from refs.

```
panel_1 = image-generation(prompt=..., output_path=".../panel-1.jpg")
panel_2 = image-generation(prompt=..., output_path=".../panel-2.jpg",
                                           input_images=".../panel-1.jpg")
panel_3 = image-generation(prompt=..., output_path=".../panel-3.jpg",
                                           input_images=".../panel-1.jpg,.../panel-2.jpg")
# panels 4+ keep using panels 1+2 as canonical refs
```

Don't use ALL prior panels as refs — too many dilute character locking. Stick to the strongest first two.

## Step 5 — Dialog / speech bubbles

Two approaches; pick by output format:

**Approach A — let nano-banana render text (simpler):**
For panels with dialog, add to the prompt: `Render a clean speech bubble at <position>: "<dialog line>". Comic-style bubble with thick black border.` Nano Banana 2 Pro renders text well in 2026; the old "no text" rule was for earlier models. Reliable for short lines (<10 words).

**Approach B — SVG overlay in HTML composition (precise):**
Leave panels text-free. In Step 6, overlay `<svg>` text on each `<img>` with absolute positioning. Pixel-perfect but more work. Use when typography matters.

Default to A for casual/brief; B for production.

## Step 6 — Compose the final artifact

Pick output format based on the user's ask. In priority order:

### Option A — Python PDF via Pillow / fpdf2 (RECOMMENDED for PDF output)

The skillshub venv ships with `Pillow` and `fpdf2` pre-installed. The runtime prepends `skillshub/.venv/bin` to your subprocess PATH, so `python3` already resolves to the venv interpreter — just call it directly. (If you see `ModuleNotFoundError: No module named 'fpdf'`, the venv wasn't populated; the backend logs a startup warning when this happens — ask the user to run `make -C skillshub setup-python`.)

**A1 — simplest, images as PDF pages (3 lines):**
```
shell(command="python3 -c \"
from PIL import Image
paths = ['/tmp/.../panel-1.jpg', '/tmp/.../panel-2.jpg', ...]
imgs = [Image.open(p).convert('RGB') for p in paths]
imgs[0].save('/tmp/comic-<run-id>.pdf', save_all=True, append_images=imgs[1:])
\"")
```
Each panel becomes a full page. Good for storyboard-style sequential reading. No layout.

**A2 — fpdf2 with title + grid layout:**
```
shell(command="python3 -c \"
from fpdf import FPDF
panels = ['/tmp/.../panel-1.jpg', '/tmp/.../panel-2.jpg', '/tmp/.../panel-3.jpg', '/tmp/.../panel-4.jpg']
title = 'My Comic'
pdf = FPDF(orientation='P', unit='mm', format='A4')
pdf.add_page()
pdf.set_font('Helvetica', 'B', 18)
pdf.cell(0, 12, title, ln=1, align='C')
for i, p in enumerate(panels):
    row, col = i // 2, i % 2
    x = 10 + col * 95
    y = 30 + row * 130
    pdf.image(p, x=x, y=y, w=90)
pdf.output('/tmp/comic-<run-id>.pdf')
\"")
```
Yields A4 portrait, title on top, 2×2 grid of panels. Customize the grid for other panel counts (e.g., 3×2 for 6 panels, 3×4 for 12).

**A3 — Pillow grid composite (single image PDF, full creative control):**
```
shell(command="python3 -c \"
from PIL import Image
panels = [Image.open(p) for p in ['/tmp/.../panel-1.jpg', ...]]
cell_w, cell_h = 800, 1000
cols, rows = 2, 2
page = Image.new('RGB', (cell_w*cols + 30, cell_h*rows + 30), 'white')
for i, panel in enumerate(panels):
    panel.thumbnail((cell_w, cell_h))
    x = (i % cols) * (cell_w + 10) + 10
    y = (i // cols) * (cell_h + 10) + 10
    page.paste(panel, (x, y))
page.save('/tmp/comic-<run-id>.pdf')
\"")
```
For speech bubble overlays via Pillow, see Step 5 Approach B — same library; add `ImageDraw` text on each panel before paste.

### Option B — HTML (when PDF isn't required)

Read the bundled template: `files(action=read, path={this_skill_dir}/templates/comic_page.html.j2)`. It uses `{{ title }}`, `{{ subtitle }}`, `{{ panels }}` placeholders — substitute by string-replace, no Jinja runtime needed.

Or generate HTML inline:
```html
<!doctype html>
<html><head><meta charset="utf-8"><title>{title}</title>
<style>
  body { font-family: Georgia, serif; max-width: 800px; margin: 2rem auto; }
  h1 { text-align: center; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
  .panel img { width: 100%; border: 3px solid #111; }
  .panel .caption { font-size: 14px; padding: 6px 0; }
</style></head>
<body>
<h1>{title}</h1>
<div class="grid">
  <div class="panel"><img src="file:///tmp/.../panel-1.jpg"><div class="caption">Panel 1: ...</div></div>
  ...
</div>
</body></html>
```
Write via `files(action=write, path=/tmp/comic-<run-id>.html, content=...)`.

### Option C — HTML + Chrome→PDF (fallback)

If A* fails and you still need PDF:
```
shell(command="/Applications/Google\\ Chrome.app/Contents/MacOS/Google\\ Chrome --headless --disable-gpu --no-pdf-header-footer --print-to-pdf=/tmp/comic-<run-id>.pdf file:///tmp/comic-<run-id>.html")
```
On linux: `google-chrome` or `chromium`. Slower and more brittle than Option A.

## Step 7 — Return the artifact

Hand back the path(s):
- Primary: HTML file (always)
- Optional: PDF file (when asked)
- Optional: individual panel JPGs (helpful for editing)

Mark goal achieved. Call `deactivate_skill` THIS turn.

## Common failure modes

- **Skipping image generation and writing a markdown spec.** This is the #1 failure mode when the image-gen tool isn't in your catalog. STOP and tell the user the credential is missing. Never substitute prose for actual images — the user asked for a comic, not a description of a comic.
- **Mixing styles across panels.** Reuse the same `prompt_fragment` for every panel. Don't paraphrase it differently each time.
- **Dropping character refs.** Without `input_images` referring back to panels 1+2, characters drift across panels. Always pass refs from panel 3 onward.
- **Asking nano-banana to render long dialog.** Bubble text over ~15 words renders poorly. Trim dialog or use SVG overlay.
- **Skipping the IP-safety note.** Every style YAML has an `ip_safe_note` — respect it. No "Naruto in manga style" — describe characters by appearance, not by name.

## Reference files bundled with this skill

This skill ships with reference assets the orchestration reads via the `files` tool:

| Path | Contents | When to read |
|---|---|---|
| `{this_skill_dir}/styles/<name>.yaml` | Per-style prompt fragment + IP-safety note | Step 1 — pick the style YAML matching your choice |
| `{this_skill_dir}/templates/comic_page.html.j2` | HTML layout template | Step 6 — if you want the bundled layout instead of inline HTML |
| `{this_skill_dir}/fonts/` | Reserved for SVG overlay typography (currently empty) | Step 5 Approach B — use system fonts if empty |

`{this_skill_dir}` resolves at runtime to `<scope>/skills/comic-strip/` (scope-installed) or the system-shared install path. Both are valid; use `files(action=read, path=...)` to fetch any reference.

---

After rendering the final artifact, call `deactivate_skill` THIS turn. If anything fails partway, deactivate before reporting failure — don't leave the playbook hanging.
