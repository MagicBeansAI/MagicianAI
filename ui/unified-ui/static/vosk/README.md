# Vosk wake-word model

The browser wake-word listener (`src/lib/media/voice/wakeWord.ts`) loads a Vosk
model from `/vosk/model.tar.gz` (this folder, served at the site root).

It is **not** committed (the model is tens of MB). Add it once:

1. Download a small model from <https://alphacephei.com/vosk/models>
   (e.g. `vosk-model-small-en-us-0.15`, ~40 MB).
2. Produce a gzipped tar and save it here as `model.tar.gz`:
   ```bash
   # from the unzipped model directory's parent:
   tar czf model.tar.gz vosk-model-small-en-us-0.15
   mv model.tar.gz ui/unified-ui/static/vosk/model.tar.gz
   ```
   `vosk-browser` accepts the tarball directly.

Override the path at runtime via `wakeModelUrlStore` (e.g. to load from a CDN).

See `docs/archive/plans/2026-06-08-wake-word-in-app-voice.md`.
