# Magician macOS speech helpers

Swift Package Manager package with the two short-lived native helpers that
Magician's macOS host surfaces spawn. They were extracted unchanged from the
retired `native/macos-presence-host` package when the Orb mascot moved into
the Tauri frontend: the mascot went, these did not.

- `magician-macos-speech-helper` — used by the desktop tray's host gateway
  and the media rails. `status` / `authorize` drive the Speech Recognition
  permission shown in Desktop Settings, `transcribe` backs
  `POST /host/speech/transcribe` (Apple on-device recognition), and
  `synthesize` backs the `macos_tts` provider (AVFoundation speech
  synthesis, WAV out).
- `magician-macos-meet-audio` — taps a target app's audio through
  ScreenCaptureKit and streams PCM16 / on-device transcripts; spawned by
  the meeting bridge and the `macos_speech_stt` media provider in
  `--mode transcribe`.

Each executable embeds its own `Info.plist` (`SpeechHelper.Info.plist`,
`MeetAudio.Info.plist`) so the macOS Privacy prompts for Speech Recognition,
Microphone and Screen Recording name the helper rather than the shell that
launched it. Keep the usage strings there when a prompt changes.

## Build and stage

From the repository root:

```sh
make build-macos-speech-helper          # debug; alias of -debug
make build-macos-speech-helper-release
```

Both stage `./magician-macos-speech-helper.bin` and
`./magician-macos-meet-audio.bin` at the repo root beside `magician.bin`,
where the tray launch targets and the media providers look for them.
`make build-all-debug`, `make build-all-release` and `make dev-desktop-build`
include the build. The scratch path is `$(CARGO_TARGET_DIR)/macos-speech-helper`
(SSD1 when mounted), never `.build/` — see `.gitignore`.

Runtime resolution order for the speech helper is documented in
`docs/components/desktop/README.md` (env `MAGICIAN_MACOS_SPEECH_HELPER_BIN`,
then `host_gateway.macos_speech_helper_bin`, an exe-sibling `.bin`, then the
scratch path, then PATH).

## Smoke check

```sh
./magician-macos-speech-helper.bin status     # JSON: Speech authorization state
./magician-macos-speech-helper.bin authorize  # triggers the TCC prompt once
```
