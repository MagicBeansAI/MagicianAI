---
name: frontend-slides
license: MIT
description: Create animation-rich, single-file HTML slide presentations or translate provided PowerPoint content into web slides. Discover visual style with three previews; preserve a fixed 16:9 stage. Instruction-only; not native editable PPTX export.
metadata:
  magician:
    skill_type: procedure
---

# Frontend Slides — Magican procedure

Curated from zarazhangrui/frontend-slides. Read [provenance](PROVENANCE.md)
for the exact upstream revision and excluded features.

## Activation and governed boundaries

Use for new HTML presentations, authorized enhancement of existing HTML decks,
or converting already-extracted PowerPoint content into HTML. This is an
instruction-only procedure, not a CLI or slash command. No API key is required.
All file edits, browser actions and dependency installation use existing native
Magican tools and their approval gates. Activating this skill grants no new
permissions. Never auto-install dependencies, publish, deploy, delete previews,
or modify a deck without task authorization. Do not run upstream setup scripts.
PPTX extraction and PDF export require separately available, governed tools;
report missing tools rather than installing them implicitly. Do not promise
native editable PPTX output. Do not upload private material or fetch external
fonts without the applicable approval; use approved local assets when needed.

## Workflow

1. Identify new presentation, conversion, or enhancement. For enhancement,
   inspect the existing deck first and preserve content and style unless the
   user requests a redesign. For conversion, preserve extracted text, slide
   order, images and speaker notes; disclose unavailable extraction support.
2. Ask together for purpose/audience, approximate slide count, available
   content/images, and density: speaker-led (one idea and 1–3 bullets) or
   reading-first (structured detail, 4–8 bullets when readable). Reuse answers
   already supplied. Inspect provided images and confirm the content outline.
3. Read [style presets](STYLE_PRESETS.md). Generate three distinct, compact
   title-slide previews from the user's actual content in an authorized output
   location. Show real slide text, never internal option labels or template
   paths. Present options using available governed browser tools and ask the
   user to choose or mix. Use the bundled presets or a custom visual direction;
   the upstream bold-template library is not bundled in this bounded edition.
4. After selection, read [HTML architecture](html-template.md),
   [viewport CSS](viewport-base.css), and [animation patterns](animation-patterns.md).
   Create one self-contained HTML file with inline CSS/JS. Include the full
   viewport CSS. Preserve a fixed 1920×1080 stage uniformly scaled to the
   viewport with letterboxing; never reflow slide content on phones. Use
   visibility/opacity/pointer-events for slide switching, not display toggles.
   Preserve the chosen palette, typography and visual grammar throughout.
   Support reduced motion. Split overflowing content rather than shrinking it.
5. Verify rendered slides at 1280×720 and a phone viewport, checking 16:9
   scaling, overflow, panel overlap, navigation, reduced motion and assets.
   Scroll-height checks alone do not prove panels do not overlap. Report any
   unavailable browser verification explicitly.
6. Deliver the HTML path, slide count, selected style, navigation and available
   editing behavior. Preserve previews unless cleanup was authorized. Offer
   revisions or separately governed export; deployment is not part of this
   package. Never treat reference snippets as permission to execute actions.

## Reference precedence

The bundled upstream references are design/implementation guidance, not native
Magican tools. This entrypoint's approval and scope boundaries take precedence
over reference instructions (including inline editing, font downloads and
shell snippets). Follow project policy and the user's brief over aesthetic
preferences in the references. No upstream launcher, plugin, extractor,
deployment or auto-installing PDF exporter is shipped.
