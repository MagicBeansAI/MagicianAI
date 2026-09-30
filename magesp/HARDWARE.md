# Hardware notes — Waveshare ESP32-C6-Touch-AMOLED-1.8

Measured on the development unit, 2026-08-13. Full record: §12 of
`docs/archive/plans/2026-08-13-esp32-voice-terminal-design.md`.

## Silicon

| | |
|---|---|
| Chip | ESP32-C6 (QFN40) rev **v0.2**, single core + LP core @ 160 MHz |
| Flash | 16 MB (mfr `0x20`, dev `0x4018`) |
| PSRAM | **none** — the C6 does not support it |
| Display | 368 × 448 QSPI AMOLED, **SH8601 (V1)** |
| Touch | FT5x06 family @ `0x38` (V1). **Working** |
| Board variant | **V1 (SH8601 + FT5x06)**, confirmed by `bsp_board_detect()` |

## Pin map (from the BSP)

| Bus | Pins |
|---|---|
| I2C0 (shared) | SCL **GPIO7**, SDA **GPIO8** |
| I2S | MCLK 19, SCLK 20, WS 22, DOUT 23, DSIN 21 |
| SD (SPI) | CS 6, MOSI 10, SCK 11, MISO 18 |
| LCD | PCLK GPIO0 (QSPI) |
| Button | BOOT **GPIO9**, idles HIGH, active LOW. **Strapping pin** |

Via the TCA9554 expander: power amp = pin 7, `LCD_RST` = pin 4,
`LCD_TOUCH_RST` = pin 5.

## I2C devices (all on one bus)

`0x18` ES8311 codec · `0x20` TCA9554 · `0x34` AXP2101 PMU · `0x51` PCF85063 RTC
· `0x6B` QMI8658 IMU · `0x38` FT5x06 touch.

The ES8311 shares this bus, which is why GPIO9 was chosen for PTT over the
AXP2101 power key — no I2C traffic while recording.

## Buttons — both usable

| Button | Evidence |
|---|---|
| BOOT (GPIO9) | 30 clean press transitions, idles high. **Chosen as PTT** |
| PWR (AXP2101) | 54 IRQ events, sticky `reg 0x49 = 0x0F`: press + release edges, short + long press |

## Heap budget

| Phase | Free | Largest block |
|---|---|---|
| boot | 366 KB | 336 KB |
| WiFi up (no AP) | 316 KB | 296 KB |
| display + LVGL live | 207 KB | 192 KB |

WiFi costs ~41 KB; mbedTLS is another 30–50 KB and fits comfortably.

## Power

No battery fitted. **AXP2101** PMU with an **MX1.25 header** for a
user-supplied 3.7 V LiPo. With no cell attached the PMU reads the USB rail, so
any battery indicator is meaningless. A light-themed UI lights every AMOLED
pixel and costs materially more power than a dark one.

## The panel has ROUNDED CORNERS — design to a safe area

The 368 × 448 glass is a rounded rectangle, so **anything drawn into a screen
corner is physically hidden**. This is not a driver offset and no software
setting changes it.

Consequences for every screen:

- **Never place text or controls in a corner.** Centre them horizontally, or
  keep them well inside the corner radius. A top-left readout came back clipped
  to a single character before this was understood.
- **The game's playfield is a circle, not the screen rectangle.** The ball is
  bounced off an arena boundary at **r = 176 px** from centre, so it can never
  roll somewhere it cannot be seen. Rectangular screen-bounds clamping is wrong
  on this hardware and loses the ball in the corners.
- Backgrounds may stay rectangular — the glass simply crops them.

## Audio, as measured

| | |
|---|---|
| Capture | 16 kHz mono PCM16, ES8311, **18 dB gain** (32 dB clipped every sample at 32767) |
| Playback | whatever the WAV header states — observed float32 mono 22050 Hz |
| Staging | `/spiffs/utterance.wav`, 12 s cap (15 s hands free) |

## Heap budget, as measured

| Phase | Free | Largest block |
|---|---|---|
| boot | 366 KB | 336 KB |
| wifi up | 316 KB | 296 KB |
| display + LVGL @ buf height 100 | ~91 KB | ~76 KB |
| **display + LVGL @ buf height 40** | **179 KB** | **164 KB** |

A TLS handshake needs tens of KB contiguous. At 91 KB free it returned
`ALLOC_FAILED` intermittently — and the failure surfaced as an upload error, a
connection error, or a certificate error depending on which allocation came
next. **Heap exhaustion does not announce itself; it wears the mask of
whatever fails first.**

## Traps

- **Touch is NOT intermittent.** Early boots reported it missing and that was
  recorded as a hardware fault for several revisions of these notes. It was
  not: `bsp_board_detect()` reports `Detected board variant: V1 (SH8601 +
  FT5x06)` consistently. Two wrong calls in a row on this one -- first "the
  panel is absent, RMA it", then "it is intermittent" -- both from inferring
  hardware state from a symptom rather than the vendor probe.
- **A bare I2C scan cannot see the touch controller.** `bsp_board_detect()`
  resets `LCD_RST` and `TOUCH_RST` together through an initialised TCA9554
  before probing. Concluding "the part is missing" from a hand-rolled scan is
  wrong — this happened, see the retraction in design §12.
- **The factory image boot-loops** whenever touch does not answer, because it
  `ESP_ERROR_CHECK`s touch init. Reflashing the vendor binary does not help.
- **USB-Serial/JTAG re-enumerates on reset** — serial readers must reconnect.
