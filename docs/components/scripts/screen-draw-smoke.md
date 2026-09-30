# Screen Draw Smoke Scripts

The repository includes small host-gateway smoke scripts for Personal Tutor draw
overlay validation:

- `scripts/test-screen-draw-smoke.sh` clears the overlay, draws a highlight, and
  draws an arrow.
- `scripts/test-screen-draw-notes.sh` clears the overlay and draws a configurable
  highlight around the Notes "new note" button area.
- `scripts/test-screen-draw-clear.sh` clears all active draw marks.

All scripts call:

```text
${MAGICIAN_HOST_GATEWAY_URL:-http://127.0.0.1:3017}/host/overlay/draw
```

Run them only after the desktop host gateway is running. They are intended for
manual smoke validation of transparency, coordinate alignment, missed-event
replay, and clear behavior.
