---
name: "screen-draw"
version: 0.1.0
description: "Tool skill for drawing temporary Personal Tutor and App Copilot overlays on the user's macOS screen. Treat @tutor/@tutur/hey tutor/hey tutur as concept-tutor invoke words and @copilot/@app-copilot/hey copilot as app-help invoke words; use only when the current screen/HUD prompt includes one of those markers and visual guidance would help. Draw highlights and arrows through the desktop host gateway at /host/overlay/draw; the desktop renders them on a separate inert, click-through draw overlay so the chat HUD can be dismissed. Do not use this for ordinary chat answers or background observation."
metadata:
  magician:
    skill_type: tool
    requires:
      bins: ["curl"]
      host_gateway: true
    install_hint:
      docs: "Requires the Magican Desktop host gateway on http://127.0.0.1:3017 and the HUD overlay window. The tool posts JSON shapes to /host/overlay/draw."
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          it draws over the operator's live display; any call disrupts
          whatever the machine is being used for.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins: [curl]
      runtime:
        protocol: cli
        command_prefix:
          - -s
          - -X
          - POST
          - http://127.0.0.1:3017/host/overlay/draw
          - -H
          - "Content-Type: application/json"
          - -d
        interaction: batch
        stdin: {mode: denied, sensitivity: public}
        working_directory: {mode: denied}
        limits:
          timeout_secs: 5
          stdout_bytes: 1048576
          stderr_bytes: 1048576
      auth: {kind: none, requirement: none}
      policy_floor:
        approval: native_ui_control
        resource_scopes: [native_ui]
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      actions:
        call:
          description: Draw one validated shape/storyboard payload through the local desktop overlay.
          parameters:
            shape_json:
              type: string
              description: JSON-encoded overlay shape or storyboard group.
              required: true
              min_length: 2
              max_length: 65536
            tutor_run_id:
              type: string
              description: Optional tutor/copilot run identifier used by the chat controller.
              max_length: 4096
            tutor_step_label:
              type: string
              description: Optional Rust tutor-run label used by the chat controller.
              max_length: 4096
            tutor_target:
              type: string
              description: Optional UI or lesson target used by the chat controller.
              max_length: 4096
            tutor_lesson_plan:
              type: json_array
              description: Optional bounded milestone plan used by the tutor controller.
              max_json_bytes: 65536
              max_depth: 12
              max_nodes: 2048
              max_items: 64
            canvas_mode:
              type: string
              description: Optional screen-overlay or blackboard presentation mode.
              enum_values: [screen_overlay, blackboard]
              max_length: 32
          mappings:
            - {type: runtime_control, parameter: shape_json}
            - {type: runtime_control, parameter: tutor_run_id}
            - {type: runtime_control, parameter: tutor_step_label}
            - {type: runtime_control, parameter: tutor_target}
            - {type: runtime_control, parameter: tutor_lesson_plan}
            - {type: runtime_control, parameter: canvas_mode}
          timeout_secs: 5
    runtime_catalog:
      categories: [ui_automation, desktop_operations]
      composition_category: tools
      chat_inline_adapter: tutor_screen_draw
      expose_timeout_control: false
---

# Screen Draw

Draw temporary visual guidance and educational concept overlays on the user's
macOS screen using the Personal Tutor draw overlay. This is separate from the
chat HUD, so the user can dismiss the HUD after entering a tutor prompt and
still see marks that arrive later.

## Use Only When Explicit

Treat `@tutor`, `@tutur`, `hey tutor`, and `hey tutur` as concept-tutor invoke
words. Treat `@copilot`, `@app-copilot`, `@appcopilot`, `hey copilot`, and
`hey app copilot` as app-help invoke words. Use this tool only when the current
user turn includes one of those markers and is asking for visual guidance, such
as:

- `@tutor explain the visible graph`
- `@tutur explain recursion from scratch`
- `@copilot point to the search box`
- `hey copilot show me where to enter the requested text`

Do not draw for normal chat, passive screen observation, or generic
explanations that do not include one of the draw invoke words.

For action-oriented App Copilot prompts that ask the agent to perform or demonstrate
a reversible workflow, drawing is only the preview. After the overlay, the chat
agent must continue with the guided action through `mac-operator` and then
observe/verify. Do not treat the draw as the final answer for guided-action
copilot runs.

For concept tutor prompts about visible content, draw on top of the visible
screen content without mutating the underlying app. PDFs, images, videos,
browser pages, IDEs, native apps, and question papers are all just visible
screen content for this tool.

For source-free concept tutor prompts, use blackboard mode instead of forcing a
screen observation. Set `canvas_mode` to `"blackboard"` on the tool call and on
the top-level `shape_json` group. Draw synthetic diagrams in overlay/model
coordinates, do not cite `source_entity_ids`, and do not claim the diagram was
observed on screen.

## Shape Payloads

Call the tool with one JSON string.

The tool is a renderer only. It does not inspect apps, screenshots, or the
accessibility tree. The calling agent must use the current screen image/HUD
context to decide the target element and pass resolved coordinates in
`shape_json`. By default those coordinates are in the latest full-screen
screenshot / overlay model coordinate space. Use `coordinate_space: "screen"`
only when the turn explicitly provides real macOS screen coordinates.

If the active screen attachment is a rect-aware cropped region, derive
coordinates in the cropped image and set `coordinate_space` to `"capture"` in
`shape_json`. The runtime will use the attachment's crop resolution and
top-left real screen coordinate to map those image-local coordinates onto the
live overlay. Do not use live drawing from an interactive crop that is marked
crop-local or missing a real screen rect; request/use a full-screen or
rect-aware capture first.

For Personal Tutor and App Copilot flows, also pass the optional runtime metadata whenever
available:

- `tutor_run_id`: the run id returned by `start_tutor_run`
- `tutor_step_label`: short label for this draw step
- `tutor_target`: the UI element or object being pointed at
- `tutor_lesson_plan`: complete milestone plan for the first draw of a
  Personal Tutor lesson (progressive for normal Tutor, condensed for Tutor Quick)
- `canvas_mode`: `"screen_overlay"` for visible-screen overlays, or
  `"blackboard"` for synthetic source-free concept diagrams

The chat runtime records that metadata into the Rust tutor run before it
posts the shape to the desktop overlay. Top-level `tutor_step_label` is only
run metadata; it does not satisfy storyboard validation. Put
`tutor_step_label` or `step_label` plus `narration` inside `shape_json` itself.

For every Personal Tutor lesson, include `tutor_lesson_plan` on the first draw.
It is an array of `{ "milestone_id", "objective" }` objects. For normal Tutor,
the runtime-provided 1/2/3 depth is a minimum floor rather than a fixed length:
add every prerequisite, reasoning link, worked example, or conclusion the
concept genuinely needs. For Tutor Quick, select only the essential conceptual
spine within the returned condensed range: focused 1, standard 1-2, and
deep/concept 2-4. The first small Quick milestone and the complete Quick plan
belong in the same `screen-draw` call, so planning does not add another model
turn. Keep each Quick narration at most 240 characters. Each narrated visual
`storyboard_step_id`/`reveal_id` must match its planned `milestone_id`.
Subsequent draws may omit the plan. If teaching reveals a genuinely new
objective, resend the entire existing plan with the new milestone appended;
existing scope cannot be removed, and a Quick plan cannot exceed its returned
maximum. Screen-overlay and blackboard Tutor use the same pacing contract. Do
not send this plan for App Copilot.

For tutor and App Copilot explanations, treat the draw payload as the source
storyboard for the visible and spoken explanation. Every meaningful mark/reveal,
including a single app UI highlight, must carry `tutor_step_label` or
`step_label` plus `narration` on the group or reveal shape. The label names the
mark; `narration` is the short sentence that should be spoken while that mark is
visible. Use `storyboard_step_id` or `reveal_id` as the stable step id when
multiple child shapes belong to one spoken explanation. Keep the final chat
answer to a recap/status of the same drawn and narrated steps, not a separate
explanation. The runtime rejects tutor/copilot draws that are missing explicit
storyboard label or narration metadata.
When unsure, wrap the visual mark or marks in one `group` that carries
`storyboard_step_id`, `tutor_step_label`, and `narration`, then put the actual
highlight/arrow/formula/etc. in `shapes`.

In guided App Copilot runs, each UI-changing `tutor_action` delegated to
`mac-operator` must copy the `storyboard_step_id` or `tutor_step_label` from
the immediately preceding preview draw. The runtime rejects actions that are
not bound to a successful screen-draw storyboard step in the same run.

Highlight a region:

```json
{"type":"highlight","storyboard_step_id":"click-target","x":200,"y":200,"w":400,"h":120,"color":"orange","tutor_step_label":"Click target","narration":"This highlighted region is the next place to click."}
```

Highlight a rect-aware cropped capture:

```json
{"type":"highlight","coordinate_space":"capture","storyboard_step_id":"cropped-target","x":20,"y":30,"w":220,"h":80,"color":"orange","tutor_step_label":"Selected target","narration":"This highlighted target is inside the selected screen region."}
```

Draw an arrow:

```json
{"type":"arrow","storyboard_step_id":"open-control","from_x":180,"from_y":220,"to_x":360,"to_y":260,"color":"blue","tutor_step_label":"Open control","narration":"Follow this arrow to the control that opens the next view."}
```

Add formula text:

```json
{"type":"formula","storyboard_step_id":"slope-formula","x":420,"y":260,"text":"slope = rise / run","color":"orange","tutor_step_label":"Slope formula","narration":"This formula names slope as rise divided by run."}
```

Draw a blackboard concept diagram:

```json
{
  "type": "group",
  "id": "recursion-blackboard",
  "canvas_mode": "blackboard",
  "storyboard_step_id": "recursion-stack",
  "tutor_step_label": "Stack frames",
  "narration": "Each recursive call adds a stack frame until the base case returns.",
  "ttl_ms": 60000,
  "color": "cyan",
  "shapes": [
    {"type": "stack_frame", "x": 760, "y": 260, "w": 260, "h": 90, "label": "fact(3)"},
    {"type": "stack_frame", "x": 760, "y": 380, "w": 260, "h": 90, "label": "fact(2)"},
    {"type": "stack_frame", "x": 760, "y": 500, "w": 260, "h": 90, "label": "fact(1) base"},
    {"type": "arrow", "from_x": 1035, "from_y": 545, "to_x": 1160, "to_y": 430, "label": "return"}
  ]
}
```

Draw a geometric square on a visible segment:

```json
{"type":"square_on_segment","storyboard_step_id":"area-square","x1":120,"y1":320,"x2":260,"y2":320,"side":"left","label":"area","color":"orange","tutor_step_label":"Area square","narration":"This square shows the area built from that side length."}
```

Group a graph/slope teaching step:

For hand-drawn charts or trends, keep the horizontal axis, vertical axis, and
plotted `path`/`curve`/`freehand` series in the same group. The plotted series
must stay inside the axis rectangle; if the series needs more space, enlarge or
move the axes rather than letting the line escape the frame. Labels and callouts
can sit outside the axes after the plotted series is properly framed.

```json
{
  "type": "group",
  "id": "slope-step-1",
  "storyboard_step_id": "slope-setup",
  "tutor_step_label": "Slope setup",
  "narration": "Use the axes and the line together to read rise over run.",
  "ttl_ms": 12000,
  "color": "orange",
  "source_entity_ids": ["axis-x", "axis-y", "line-main"],
  "shapes": [
    {"type": "axis", "x1": 100, "y1": 500, "x2": 700, "y2": 500, "source_entity_ids": ["axis-x"]},
    {"type": "axis", "x1": 100, "y1": 500, "x2": 100, "y2": 120, "source_entity_ids": ["axis-y"]},
    {"type": "path", "d": "M 160 440 C 280 360 420 260 620 180", "source_entity_ids": ["line-main"]},
    {"type": "formula", "x": 430, "y": 145, "text": "slope = rise / run"}
  ]
}
```

Group a timed reveal sequence:

```json
{
  "type": "group",
  "id": "slope-reveal-sequence",
  "ttl_ms": 16000,
  "shapes": [
    {
      "type": "axis",
      "x1": 100,
      "y1": 500,
      "x2": 700,
      "y2": 500,
      "source_entity_ids": ["axis-x"],
      "reveal_id": "identify-axis",
      "reveal_order": 1,
      "tutor_step_label": "Identify the x-axis",
      "narration": "First, anchor the horizontal change on the x-axis.",
      "duration_ms": 900
    },
    {
      "type": "line",
      "x1": 160,
      "y1": 440,
      "x2": 620,
      "y2": 180,
      "source_entity_ids": ["line-main"],
      "reveal_id": "show-line",
      "reveal_order": 2,
      "delay_ms": 900,
      "wait_for_voice": true,
      "tutor_step_label": "Show the changing line",
      "narration": "Now connect the visual run to the line's rise."
    },
    {
      "type": "formula",
      "x": 430,
      "y": 145,
      "text": "slope = rise / run",
      "reveal_id": "connect-formula",
      "reveal_order": 3,
      "delay_ms": 1800,
      "clear_previous": true,
      "persist_until_step": "summary",
      "tutor_step_label": "Connect to the formula",
      "narration": "Finally, the formula names that ratio."
    }
  ]
}
```

Group an algebra teaching step:

```json
{
  "type": "group",
  "id": "linear-equation-step-1",
  "storyboard_step_id": "linear-equation-isolate",
  "tutor_step_label": "Isolate x",
  "narration": "Subtract three from both sides so the x term remains alone on the left.",
  "shapes": [
    {"type": "formula", "x": 180, "y": 220, "text": "2x + 3 = 11", "source_entity_ids": ["equation-main"]},
    {"type": "callout", "x": 180, "y": 260, "text": "subtract 3 from both sides", "source_entity_ids": ["equation-main"]},
    {"type": "formula", "x": 180, "y": 310, "text": "2x = 8"}
  ]
}
```

Group an area/label teaching step:

```json
{
  "type": "group",
  "id": "region-label-step-1",
  "storyboard_step_id": "area-under-line",
  "tutor_step_label": "Area under the line",
  "narration": "The shaded region marks the accumulated area under the visible line.",
  "source_entity_ids": ["region-main", "line-main"],
  "shapes": [
    {"type": "area_fill", "points": [[160, 440], [620, 180], [620, 500], [160, 500]], "color": "orange", "opacity": 0.24, "source_entity_ids": ["region-main"]},
    {"type": "label", "x": 360, "y": 360, "text": "area under the line", "source_entity_ids": ["region-main"]},
    {"type": "side_label", "x1": 160, "y1": 440, "x2": 620, "y2": 180, "text": "increasing line", "source_entity_ids": ["line-main"]}
  ]
}
```

Group a free-body physics step:

```json
{
  "type": "group",
  "id": "free-body-step-1",
  "storyboard_step_id": "free-body-forces",
  "tutor_step_label": "Forces on the body",
  "narration": "The body has weight downward, normal force upward, and a horizontal component.",
  "source_entity_ids": ["body-main", "force-weight", "force-normal", "axis-main"],
  "shapes": [
    {"type": "free_body_body", "x": 360, "y": 330, "w": 140, "h": 90, "label": "body", "source_entity_ids": ["body-main"]},
    {"type": "force_arrow", "from_x": 430, "from_y": 375, "to_x": 430, "to_y": 520, "label": "mg", "source_entity_ids": ["force-weight"]},
    {"type": "force_arrow", "from_x": 430, "from_y": 375, "to_x": 430, "to_y": 235, "label": "N", "source_entity_ids": ["force-normal"]},
    {"type": "component_vector", "from_x": 430, "from_y": 375, "to_x": 550, "to_y": 375, "label": "x component"},
    {"type": "axis", "x1": 260, "y1": 560, "x2": 600, "y2": 560, "source_entity_ids": ["axis-main"]},
    {"type": "unit_label", "x": 570, "y": 535, "text": "forces in N"}
  ]
}
```

Group a code/stack teaching step:

```json
{
  "type": "group",
  "id": "recursion-stack-step-1",
  "storyboard_step_id": "recursion-stack-reference",
  "tutor_step_label": "Stack and reference flow",
  "narration": "The highlighted call frame points to the heap object through this reference.",
  "source_entity_ids": ["code-loop", "stack-main", "heap-object", "pointer-main"],
  "shapes": [
    {"type": "code_highlight", "x": 120, "y": 180, "w": 460, "h": 44, "source_entity_ids": ["code-loop"]},
    {"type": "stack_frame", "x": 720, "y": 180, "w": 180, "h": 70, "label": "fact(3)", "source_entity_ids": ["stack-main"]},
    {"type": "heap_object", "x": 980, "y": 300, "w": 170, "h": 90, "label": "Node", "source_entity_ids": ["heap-object"]},
    {"type": "pointer_arrow", "from_x": 900, "from_y": 215, "to_x": 980, "to_y": 345, "label": "ref", "source_entity_ids": ["pointer-main"]},
    {"type": "flow_node", "x": 720, "y": 275, "w": 180, "h": 50, "label": "next call"},
    {"type": "flow_edge", "from_x": 810, "from_y": 250, "to_x": 810, "to_y": 275}
  ]
}
```

Clear existing overlay drawings:

```json
{"type":"clear"}
```

Supported primitives:

- Core: `clear`, `group`, `label`, `callout`, `line`, `arrow`, `rect`,
  `highlight`, `polygon`, `circle`, `arc`, `path`, `curve`, `freehand`,
  `handwriting`, `cursive_text`, `mask`, `spotlight`
- Math: `angle_marker`, `right_angle_marker`, `side_label`,
  `perpendicular_marker`, `parallel_marker`, `square_on_segment`, `area_fill`,
  `measurement_tick`, `formula`
- Physics: `vector_arrow`, `force_arrow`, `component_vector`, `axis`,
  `trajectory`, `field_line`, `free_body_body`, `unit_label`
- Computer science: `code_highlight`, `stack_frame`, `heap_object`,
  `pointer_arrow`, `state_box`, `flow_node`, `flow_edge`, `timeline_tick`,
  `memory_cell`

Every primitive may include `id`, `group_id`, `z_index`, `duration_ms`,
`delay_ms`, `animate`, `style`, `label`, `ttl_ms`, `persist`, and
`source_entity_ids`. Tutor/App Copilot storyboard steps may include
`storyboard_step_id`. Timed tutor/copilot reveals may also include `reveal_id`,
`reveal_order`, `tutor_step_label` or `step_label`, `narration`,
`wait_for_voice`, `clear_previous`, and `persist_until_step`. Groups accept
`shapes` or `children`; the desktop host flattens them and passes shared
metadata to child shapes. `persist_until_step` keeps a mark visible until a
later reveal/step with that id or label arrives, but the mark is still bounded
by its normal `ttl_ms` fallback unless `persist:true` is explicitly set.

For instructional cursive letters or word examples, prefer text-backed
`cursive_text` so the glyphs stay readable and use the selected tutor cursive
font:

- `cursive_text`: school-cursive-style text with `text`, `x`, `y`, optional
  `font_size`, and normal color/storyboard metadata.
- `handwriting`: casual handwritten-style text with the same fields.

Use curved primitives for stroke motion, smooth paths, organic curves, or
non-angular diagrams rather than many straight `line` segments:

- `path`: SVG path data in `d` or `path`, using normal `M`, `L`, `Q`, `C`,
  `S`, `T`, `A`, and `Z` commands.
- `curve`: a single Bezier from `from_x/from_y` to `to_x/to_y`, with
  `control_x/control_y` for quadratic curves or
  `control1_x/control1_y/control2_x/control2_y` for cubic curves.
- `freehand`: a `points` array rendered as a smoothed stroke.

Example legible cursive text:

```json
{"type":"cursive_text","storyboard_step_id":"cursive-we","x":180,"y":430,"text":"we","font_size":96,"color":"cyan","tutor_step_label":"Cursive we","narration":"This shows the connected cursive letters w and e as readable text."}
```

Example freehand stroke trajectory:

```json
{"type":"freehand","storyboard_step_id":"curved-flow","points":[[180,520],[220,470],[275,500],[330,455],[390,505]],"color":"pink","stroke_width":6,"tutor_step_label":"Smooth curved stroke","narration":"This shows the direction and rhythm of a curved handwriting stroke."}
```

Drawings auto-expire after a short default lifetime so stale marks do not
remain after the user or agent changes the app state. Use `ttl_ms` only when a
specific mark should live for a different bounded duration. Use `persist:true`
sparingly, and only when the next step will explicitly clear or replace the
mark.

Coordinates are screen logical points with origin at the top-left of the
main display. Estimate carefully from the latest screen image or HUD
context. Prefer one or two clear marks over many shapes.

## Tutor / App Copilot Flow

1. Understand the user's screen question.
2. If the prompt contains a tutor or copilot invoke word and a visual mark would
   help, draw one storyboard-valid highlight or arrow with `tutor_step_label` or
   `step_label` plus `narration` inside `shape_json`.
3. Put the step's spoken/visible explanation into `narration`; do not rely on a
   separate final answer to explain a mark that has no narration.
4. For multi-step concept tutoring or App Copilot previews, use a timed reveal
   sequence with short `narration` strings and bounded `delay_ms` values when
   that improves semantic sync.
5. Explain briefly by recapping what the current reveal points to and what the
   user should understand.
6. Clear stale drawings when they would confuse the next instruction.

If the host gateway or draw overlay is unavailable, answer normally and say
that the overlay could not be drawn.
