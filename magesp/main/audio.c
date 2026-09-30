#include "audio.h"

#include <stdio.h>
#include <string.h>

#include "bsp/esp-bsp.h"
#include "esp_codec_dev.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/stream_buffer.h"
#include "freertos/task.h"

static const char *TAG = "audio";

#define SAMPLE_RATE   16000        /* what the transcription endpoint wants */
#define BITS          16
#define CHANNELS      1
#define CHUNK         2048
/* 32 dB pinned every sample at 32767 -- clipped speech transcribes badly. */
#define MIC_GAIN_DB   18.0f
/* Hands-free listens across the desk, not from the hand.
 *
 * 18 dB was set for push-to-talk, where the talker holds the device and speaks
 * into it. At arm's length the same voice measured a 7035 peak with only 2 of
 * 31 frames clearing any usable threshold -- the microphone heard it and the
 * endpointer could not, and no threshold fixes a signal that is under the
 * noise. The gain is the knob, not the comparator.
 *
 * Close speech already clipped at 18 dB, so raising it costs nothing there and
 * buys the distance case ~2.8x. */
#define HF_MIC_GAIN_DB 27.0f
#define SPK_VOL       82.0f
/* ~0.75s of 22kHz mono audio held ahead of the codec.
 *
 * Deepening the I2S DMA alone could not fix the remaining stutter: the HTTP
 * read and the codec write were serialised, so every blocking write was time
 * spent not reading. A drain task decouples them -- the network fills this
 * buffer whenever it can, and playback empties it at a constant rate, so a
 * stall only matters if it outlasts the whole cushion. */
#define PCM_RING      (24 * 1024)
#define DRAIN_CHUNK   1024
#define WAV_HDR       44
/* 12 s, not 20. A 20 s capture is 640 KB and the TLS handshake for that upload
   fails outright with an allocation error on this heap -- observed, twice. The
   cap is supposed to bound a runaway, not convert it into a lost turn. */
#define MAX_SECONDS   12
/* Hands-free gets a much shorter leash than a held button.
 *
 * Nobody is watching an endpointer that mis-fires, and a 20 s capture is
 * 640 KB -- an upload that big fails at the TLS handshake with an allocation
 * error on this heap, so the runaway does not merely waste time, it takes the
 * turn down with it. Ten seconds is longer than anything anyone says to a desk
 * terminal in one breath. */
/* Raised from 10 s. The original cap was set when a 20 s capture failed its
   TLS handshake outright -- but that failure was heap exhaustion at handshake
   time (74 KB free), not payload size: the body is streamed from the file
   against a real Content-Length, so a longer recording costs no more heap.
   Releasing the audio hardware when idle bought back ~35 KB, and 10 s is
   simply too short for a spoken instruction. */
#define HF_MAX_SECONDS 15
#define SECS_TO_PCM(s) ((size_t)(SAMPLE_RATE) * (BITS / 8) * (CHANNELS) * (s))
#define MAX_PCM       SECS_TO_PCM(MAX_SECONDS)

/* Hands-free endpointing, decided here on the samples.
 *
 * One CHUNK is 64 ms at 16 kHz mono, which is a serviceable VAD frame. The
 * open and close thresholds are deliberately apart: a single threshold
 * chatters on the tail of every word. PREROLL frames of audio are kept while
 * the gate is shut so the syllable that opened it is still in the recording --
 * without it every hands-free turn starts mid-word. */
#define CHUNK_MS      (CHUNK * 1000 / (SAMPLE_RATE * CHANNELS * (BITS / 8)))
/* Measured on this board with the meter logged frame by frame, which is the
   only way this was ever going to be right.
 *
 * An earlier pass set the floor at 6000 on the reasoning that "speech pegs the
 * meter at 32767". That figure came from push-to-talk, where the talker holds
 * the device and speaks into it. At the desk distance hands-free is actually
 * used at, ordinary speech measures 2000-5000, so a floor of 6000 sat ABOVE
 * the signal and the device heard nothing.
 *
 * The debounce, not the threshold, is what rejects impulses: a click can hit
 * 20000 in a single frame and still fail three consecutive ones. */
/* The threshold is measured against THIS room, not compiled in.
 *
 * Every fixed number tried here was wrong in one direction or the other: 2600
 * answered the codec's own power-on pop, 6000 sat above ordinary speech at
 * desk distance and the device heard nothing at all. A constant cannot know
 * the room, the gain or the distance, and each reflash to guess again cost a
 * round trip.
 *
 * So track the floor instead. Frames that do not clear the current threshold
 * feed a slow average; the threshold is a fixed multiple of that average,
 * clamped so a silent room cannot make it hair-trigger and a loud one cannot
 * make it deaf. */
#define VAD_MULT      6
#define VAD_FLOOR_MIN 1200      /* quiet room: do not trigger on nothing     */
#define VAD_FLOOR_MAX 8000      /* loud room: do not go deaf either          */
#define VAD_RELEASE   40        /* percent of the open threshold to close at */
/* Scored, not counted. A far-field speech envelope dips between syllables, so
   requiring N *consecutive* loud frames missed real sentences -- measured at
   "longest run 2, need 3" while someone was plainly talking. Each loud frame
   adds 2 and each quiet one subtracts 1, so speech accumulates through its own
   gaps while a lone click decays away. */
#define ONSET_HIT     2
#define ONSET_SCORE   5         /* two clean frames, or three ragged ones     */
/* The ES8311 pops when it powers up, and that pop is louder than speech. It
   opened the gate 557 ms after every arm, so the device answered its own
   codec and showed "didn't catch that" before anyone spoke. */
#define SETTLE_FRAMES 6         /* ~380 ms of samples discarded after open   */
/* 1300 ms, not 900.
 *
 * People pause mid-sentence -- for breath, for a word, for thought -- and 900
 * ms sits inside the range of an ordinary pause, so the endpointer was cutting
 * talkers off. The cost of waiting too long is a beat of latency; the cost of
 * closing too early is a truncated instruction and a wrong answer, which is
 * strictly worse. Err long. */
#define HANGOVER_MS   1300      /* silence that ends the turn                */
/* Four frames, not two: the ring must hold the ONSET_FRAMES that opened the
   gate AND real lead-in before them, or the recording starts mid-syllable. */
#define PREROLL       4

#define REC_PATH      BSP_SPIFFS_MOUNT_POINT "/utterance.wav"

static esp_codec_dev_handle_t s_mic;
static esp_codec_dev_handle_t s_spk;
static bool s_spk_open;
static StreamBufferHandle_t s_pcm;
static bool     s_ready;
static volatile bool s_run, s_live;
static volatile size_t s_bytes;
static volatile int    s_peak;
static TaskHandle_t s_task;
static FILE *s_fp;
/* Hands-free state. s_hf says this capture endpoints itself; s_gated says the
   mic is open but nothing is being kept yet; s_done says the endpointer has
   decided the turn is over. Push-to-talk sets none of them -- a held button
   must not be cut off by a pause for breath. */
static volatile bool s_hf, s_gated, s_done;
static volatile int  s_settle;   /* frames of codec pop still to discard */
static bool s_up;          /* the audio hardware is powered and configured */
static bool s_fresh;       /* the codec was just powered; discard its pop     */
static bool s_lean;        /* shallow DMA: a realtime socket owns most of the heap */
static volatile bool s_stream_run;   /* realtime capture owns the mic */
static volatile bool s_hold;   /* a turn owns the hardware; do not release it */
static volatile int  s_underruns;  /* times playback ran dry mid-reply        */
static int64_t s_last_use;  /* us; when the hardware was last needed          */

bool audio_ready(void) { return s_ready; }
bool audio_live(void)  { return s_live; }
size_t audio_bytes(void) { return s_bytes; }
int audio_peak(void)   { return s_peak; }
const char *audio_path(void) { return REC_PATH; }
bool audio_capturing(void)      { return s_run; }
bool audio_speech_started(void) { return s_run && !s_gated; }
bool audio_utterance_done(void) { return s_done; }
bool audio_speaker_busy(void)   { return s_spk_open; }

/* Canonical 44-byte PCM WAV header. Lengths are written as zero and patched
   on stop, because the size is not known until the button is released. */
static void wav_header(uint8_t *h, uint32_t data_len)
{
    const uint32_t rate = SAMPLE_RATE;
    const uint16_t ch = CHANNELS, bits = BITS;
    const uint32_t byte_rate = rate * ch * (bits / 8);
    const uint16_t block = ch * (bits / 8);
    memcpy(h, "RIFF", 4);
    const uint32_t riff = 36 + data_len;
    memcpy(h + 4, &riff, 4);
    memcpy(h + 8, "WAVEfmt ", 8);
    const uint32_t fmt_len = 16;
    memcpy(h + 16, &fmt_len, 4);
    const uint16_t pcm = 1;
    memcpy(h + 20, &pcm, 2);
    memcpy(h + 22, &ch, 2);
    memcpy(h + 24, &rate, 4);
    memcpy(h + 28, &byte_rate, 4);
    memcpy(h + 32, &block, 2);
    memcpy(h + 34, &bits, 2);
    memcpy(h + 36, "data", 4);
    memcpy(h + 40, &data_len, 4);
}

static void drain_task(void *arg);

/* Take the hardware back down once nothing has needed it for a while.
 *
 * Deliberately NOT done the moment a stream closes. speak() is called once per
 * sentence and opens and closes the speaker around each one, so tearing down on
 * close destroyed and rebuilt the whole I2S peripheral at every sentence
 * boundary -- which is audible, and is exactly how the reply started breaking
 * up again. The grace period spans those gaps and still comes down during the
 * long idle stretches that actually matter for temperature: thinking, and the
 * hours between turns.
 *
 * Refuses while either direction is open, so it cannot pull the peripheral out
 * from under a live capture or a reply still playing. */
#define AUDIO_GRACE_US (3 * 1000 * 1000)

static void audio_down(void)
{
    /* s_stream_run belongs here for the same reason s_run and s_spk_open do:
       the idle timer deleted the I2S peripheral underneath a running realtime
       capture, and the stream task then died with "read failed" and was
       restarted in a loop. Every user of the hardware must be able to hold
       it, or the release is not a release but a race. */
    if (!s_up || s_run || s_spk_open || s_hold || s_stream_run) return;

    if (esp_timer_get_time() - s_last_use < AUDIO_GRACE_US) return;

    if (s_mic) { esp_codec_dev_delete(s_mic); s_mic = NULL; }
    if (s_spk) { esp_codec_dev_delete(s_spk); s_spk = NULL; }
    const int64_t t0 = esp_timer_get_time();
    bsp_audio_deinit();
    s_up = false;
    ESP_LOGI(TAG, "audio down in %d ms; idle again",
             (int)((esp_timer_get_time() - t0) / 1000));
}

/* Bring the audio hardware up. Idempotent, and deliberately NOT called at boot.
 *
 * Measured on this board: with audio_init() never called the die settles near
 * 56 C and keeps falling; with the audio subsystem up and completely unused --
 * amplifier off, I2S clocks parked, codec handles created and idle -- it climbs
 * past 75 C and stays there. A desk terminal is idle almost all of its life, so
 * the subsystem now comes up for a turn and not before.
 *
 * The staging file, the playback ring and the drain task still exist from boot:
 * they cost nothing, and keeping them means a press does not have to allocate
 * before it can record. */
void audio_set_lean(bool lean)
{
    /* A realtime call has already spent ~54 KB on its socket, and the deep DMA
       the turn path needs then fails to allocate outright. Shallower buffers
       are the trade: less cushion against a stalled reader, which matters far
       less when the audio is arriving continuously rather than in HTTP bursts. */
    s_lean = lean;
}

static bool audio_up(void)
{
    if (s_up) return true;
    const int64_t t0 = esp_timer_get_time();
    bsp_audio_set_dma_depth(s_lean ? 4 : 8, s_lean ? 256 : 1024);

    /* The BSP's own duplex macro is private to its .c, so the config is built
       here from the pin defines the header does export. */
    const i2s_std_config_t cfg = {
        .clk_cfg  = I2S_STD_CLK_DEFAULT_CONFIG(SAMPLE_RATE),
        .slot_cfg = I2S_STD_PHILIP_SLOT_DEFAULT_CONFIG(I2S_DATA_BIT_WIDTH_16BIT,
                                                       I2S_SLOT_MODE_MONO),
        .gpio_cfg = {
            .mclk = BSP_I2S_MCLK,
            .bclk = BSP_I2S_SCLK,
            .ws   = BSP_I2S_LCLK,
            .dout = BSP_I2S_DOUT,
            .din  = BSP_I2S_DSIN,
            .invert_flags = {false, false, false},
        },
    };
    if (bsp_audio_init(&cfg) != ESP_OK) {
        ESP_LOGE(TAG, "i2s init failed");
        return false;
    }

    s_mic = bsp_audio_codec_microphone_init();
    if (s_mic == NULL) {
        ESP_LOGE(TAG, "ES8311 microphone init failed");
        return false;
    }
    s_spk = bsp_audio_codec_speaker_init();
    if (s_spk == NULL) ESP_LOGW(TAG, "speaker init failed -- replies will be text only");

    /* bsp_audio_init() switches the power amplifier ON, and it is a TCA9554
       output latch -- its own chip, its own supply -- so it also survives an
       MCU reset and a crash mid-reply would otherwise leave it driving the
       speaker until someone pulls the cable. */
    bsp_audio_poweramp_enable(false);

    s_up = true;
    s_fresh = true;            /* a newly powered ES8311 pops; the next
                                  capture, and only that one, must settle */
    s_last_use = esp_timer_get_time();
    ESP_LOGI(TAG, "audio up in %d ms: %d Hz mono %d-bit, speaker %s, amp off",
             (int)((esp_timer_get_time() - t0) / 1000), SAMPLE_RATE, BITS,
             s_spk ? "yes" : "no");
    return true;
}

bool audio_init(void)
{
    if (bsp_spiffs_mount() != ESP_OK) {
        ESP_LOGE(TAG, "spiffs mount failed");
        return false;
    }

    s_pcm = xStreamBufferCreate(PCM_RING, 1);
    if (s_pcm == NULL) {
        ESP_LOGE(TAG, "no memory for the playback ring");
        return false;
    }
    if (xTaskCreate(drain_task, "spk", 3072, NULL, 6, NULL) != pdPASS) {
        ESP_LOGE(TAG, "cannot start the playback task");
        return false;
    }

    s_ready = true;

    /* One up/down cycle at boot.
     *
     * Bringing the hardware up lazily quietly cost something worth keeping:
     * proof that the codec is actually there. Before, a missing ES8311 failed
     * loudly at boot; after, it would fail on the owner's first press. This
     * restores the early signal, and its timing is what a press will pay. */
    const bool ok = audio_up();
    s_last_use = 0;                 /* the self-test is not a use */
    audio_down();
    ESP_LOGI(TAG, "storage ready -> %s; audio hardware %s, releasing it",
             REC_PATH, ok ? "verified" : "MISSING");
    return true;
}

/* Where the gate opens, given what the room has been measuring. */
static int vad_threshold(int noise)
{
    const int t = noise * VAD_MULT;
    if (t < VAD_FLOOR_MIN) return VAD_FLOOR_MIN;
    if (t > VAD_FLOOR_MAX) return VAD_FLOOR_MAX;
    return t;
}

/* Open the staging file and reserve its header. The lengths are patched on
   stop, because the size is not known until the talker stops talking. */
static bool open_stage(void)
{
    s_fp = fopen(REC_PATH, "wb");
    if (s_fp == NULL) {
        ESP_LOGE(TAG, "cannot open %s", REC_PATH);
        return false;
    }
    uint8_t hdr[WAV_HDR];
    wav_header(hdr, 0);
    fwrite(hdr, 1, WAV_HDR, s_fp);
    return true;
}

static void capture_task(void *arg)
{
    uint8_t *buf = malloc(CHUNK);
    if (buf == NULL) {
        ESP_LOGE(TAG, "no memory for capture buffer");
        s_run = false;
        s_task = NULL;
        vTaskDelete(NULL);
        return;
    }

    /* Static, not stack: this task has a 4 KB stack and the ring is 8 KB. */
    static uint8_t preroll[PREROLL][CHUNK];
    int pr_head = 0, pr_n = 0, quiet_ms = 0, loud = 0, gate_pk = 0, gate_n = 0;
    int gate_hits = 0, gate_run = 0, gate_best = 0;
    int noise = 0, release = VAD_FLOOR_MIN;

    while (s_run) {
        if (esp_codec_dev_read(s_mic, buf, CHUNK) != ESP_OK) {
            ESP_LOGW(TAG, "codec read failed; stopping");
            break;
        }
        /* The screen is not allowed to claim a live microphone until the
           codec has actually handed over samples. */
        s_live = true;
        /* Hands-free keeps this loop alive indefinitely, and SPIFFS writes
           block with interrupts off. Without an explicit yield the task
           watchdog kills the device mid-utterance -- observed, not feared. */
        vTaskDelay(1);

        int pk = 0;
        const int16_t *pcm = (const int16_t *)buf;
        for (size_t i = 0; i < CHUNK / 2; i++) {
            int v = pcm[i];
            if (v < 0) v = -v;
            if (v > pk) pk = v;
        }
        if (pk > s_peak) s_peak = pk;

        if (s_gated && s_settle > 0) {
            s_settle--;
            continue;                    /* not even pre-roll: it is a pop */
        }

        if (s_gated) {
            /* Mic open, nothing kept. Hold the newest frames so the word that
               opens the gate is inside the recording, not before it. */
            memcpy(preroll[pr_head], buf, CHUNK);
            pr_head = (pr_head + 1) % PREROLL;
            if (pr_n < PREROLL) pr_n++;

            /* Speech sustains; a door slam, a keyboard or a cup on the desk
               does not. Requiring consecutive loud frames costs 64 ms of
               latency and removes most of what would otherwise become an
               upload, a transcription and an empty answer. */
            /* What the room actually measures, not what the threshold hopes.
               Without this the only visible symptom of a deaf microphone and
               of a threshold set too high is the same: nothing happens. */
            if (pk > gate_pk) gate_pk = pk;
            if (pk >= vad_threshold(noise)) gate_hits++;
            if (gate_run > gate_best) gate_best = gate_run;
            if (++gate_n >= 31) {                /* ~2 s */
                ESP_LOGI(TAG, "gate shut; peak %d, %d/%d frames over %d "
                              "(floor %d), best score %d of %d",
                         gate_pk, gate_hits, gate_n, vad_threshold(noise),
                         noise, gate_best, ONSET_SCORE);
                gate_n = gate_pk = gate_hits = gate_best = 0;
            }

            const int thr = vad_threshold(noise);
            /* Only quiet frames teach the floor, or speech would raise the bar
               against itself and the gate would never open. */
            if (pk < thr) noise = noise ? (noise * 15 + pk) / 16 : pk;

            if (pk >= thr)     loud += ONSET_HIT;
            else if (loud > 0) loud--;
            gate_run = loud;
            if (loud < ONSET_SCORE) continue;

            if (!open_stage()) break;
            for (int i = 0; i < pr_n; i++) {
                const int idx = (pr_head + PREROLL - pr_n + i) % PREROLL;
                fwrite(preroll[idx], 1, CHUNK, s_fp);
                s_bytes += CHUNK;
            }
            s_gated = false;
            quiet_ms = 0;
            loud = 0;
            release = vad_threshold(noise) * VAD_RELEASE / 100;
            ESP_LOGI(TAG, "speech starts (peak %d, floor %d, closes below %d)",
                     pk, noise, release);
        }

        if (s_fp != NULL && fwrite(buf, 1, CHUNK, s_fp) != CHUNK) {
            ESP_LOGE(TAG, "write failed at %u bytes; stopping", (unsigned)s_bytes);
            break;
        }
        s_bytes += CHUNK;

        /* Endpoint on silence, but only once the file is open: a gate that
           never opened has no turn to end. */
        if (s_hf && s_fp != NULL && !s_gated) {
            quiet_ms = (pk < release) ? quiet_ms + CHUNK_MS : 0;
            if (++gate_n >= 16) {                /* ~1 s */
                ESP_LOGI(TAG, "recording; peak %d, quiet %d ms of %d",
                         pk, quiet_ms, HANGOVER_MS);
                gate_n = 0;
            }
            if (quiet_ms >= HANGOVER_MS) {
                ESP_LOGI(TAG, "turn ended by SILENCE after %d ms quiet; "
                              "%u ms of speech kept (release level %d)",
                         quiet_ms,
                         (unsigned)(s_bytes * 1000ULL /
                                    (SAMPLE_RATE * CHANNELS * (BITS / 8))),
                         release);
                s_done = true;
                break;
            }
        }

        const size_t cap = s_hf ? SECS_TO_PCM(HF_MAX_SECONDS) : MAX_PCM;
        if (s_bytes >= cap) {
            ESP_LOGW(TAG, "turn ended by the %d second CAP -- the talker was cut "
                          "off mid-sentence, not endpointed",
                     s_hf ? HF_MAX_SECONDS : MAX_SECONDS);
            s_done = true;
            break;
        }
    }

    free(buf);
    s_run = false;
    s_task = NULL;
    vTaskDelete(NULL);
}

/* Shared by push-to-talk and hands-free. `gated` decides whether the file
   opens now or when someone speaks. */
static void capture_begin(bool gated)
{
    if (!s_ready || s_run) return;
    if (!audio_up()) return;

    s_bytes = 0;
    s_peak = 0;
    s_live = false;
    s_hf = gated;
    s_gated = gated;
    s_done = false;
    /* Settle only when the codec was actually just powered up.
     *
     * Hands free re-arms after every reply, and the grace timer means the
     * codec is usually still powered from the turn before -- so there is no
     * pop to discard, and discarding anyway threw away the first ~380 ms of
     * whatever the owner said next. Speaking straight after an answer lost the
     * start of the sentence. */
    s_settle = s_fresh;
    s_fresh = false;

    if (!gated && !open_stage()) return;

    esp_codec_dev_sample_info_t fs = {
        .bits_per_sample = BITS,
        .channel = CHANNELS,
        .channel_mask = 0,
        .sample_rate = SAMPLE_RATE,
    };
    if (esp_codec_dev_open(s_mic, &fs) != ESP_OK) {
        ESP_LOGE(TAG, "codec open failed");
        if (s_fp) { fclose(s_fp); s_fp = NULL; }
        return;
    }
    esp_codec_dev_set_in_gain(s_mic, gated ? HF_MIC_GAIN_DB : MIC_GAIN_DB);

    s_run = true;
    if (xTaskCreate(capture_task, "mic", 4096, NULL, 5, &s_task) != pdPASS) {
        ESP_LOGE(TAG, "cannot start capture task");
        s_run = false;
        esp_codec_dev_close(s_mic);
        if (s_fp) { fclose(s_fp); s_fp = NULL; }
    }
}

void audio_start(void) { capture_begin(false); }

/* ---- streaming capture ------------------------------------------------- */

static audio_chunk_cb   s_stream_cb;
static TaskHandle_t     s_stream_task;

bool audio_streaming(void) { return s_stream_run; }

static void stream_task(void *arg)
{
    (void)arg;
    uint8_t *buf = malloc(CHUNK);
    if (buf == NULL) {
        ESP_LOGE(TAG, "no memory for the stream buffer");
        s_stream_run = false;
        s_stream_task = NULL;
        vTaskDelete(NULL);
        return;
    }
    while (s_stream_run) {
        if (esp_codec_dev_read(s_mic, buf, CHUNK) != ESP_OK) {
            ESP_LOGW(TAG, "stream read failed; stopping");
            break;
        }
        vTaskDelay(1);            /* the watchdog lesson, same as capture */
        s_live = true;
        s_last_use = esp_timer_get_time();
        if (s_stream_cb) s_stream_cb(buf, CHUNK);
    }
    free(buf);
    s_live = false;
    s_stream_run = false;
    s_stream_task = NULL;
    vTaskDelete(NULL);
}

bool audio_stream_start(int rate, audio_chunk_cb cb)
{
    if (!s_ready || s_stream_run) return s_stream_run;
    /* One microphone. A turn recording and a realtime stream cannot both own
       it, and quietly letting the second one win would corrupt both. */
    if (s_run) {
        ESP_LOGW(TAG, "a turn recording owns the mic; not streaming");
        return false;
    }
    if (!audio_up()) return false;
    if (s_mic == NULL) return false;

    esp_codec_dev_sample_info_t fs = {
        .bits_per_sample = BITS,
        .channel = CHANNELS,
        .channel_mask = 0,
        .sample_rate = (uint32_t)rate,
    };
    if (esp_codec_dev_open(s_mic, &fs) != ESP_OK) {
        ESP_LOGE(TAG, "stream: codec open failed at %d Hz", rate);
        return false;
    }
    esp_codec_dev_set_in_gain(s_mic, HF_MIC_GAIN_DB);

    s_stream_cb = cb;
    s_stream_run = true;
    if (xTaskCreate(stream_task, "mic-stream", 4096, NULL, 5, &s_stream_task) != pdPASS) {
        ESP_LOGE(TAG, "cannot start the stream task");
        s_stream_run = false;
        esp_codec_dev_close(s_mic);
        return false;
    }
    ESP_LOGI(TAG, "streaming mic at %d Hz, %d-byte chunks", rate, CHUNK);
    return true;
}

void audio_stream_stop(void)
{
    if (!s_stream_run) return;
    s_stream_run = false;
    for (int i = 0; i < 100 && s_stream_task != NULL; i++) vTaskDelay(pdMS_TO_TICKS(10));
    if (s_mic) esp_codec_dev_close(s_mic);
    s_stream_cb = NULL;
    s_last_use = esp_timer_get_time();
    ESP_LOGI(TAG, "mic stream stopped");
}

/* Called from the UI tick. Everything else only stamps s_last_use. */
void audio_idle_tick(void) { audio_down(); }

/* Hold the hardware for the whole of a reply.
 *
 * A reply is spoken one sentence at a time, and each sentence is a separate
 * TTS request -- so the gap between two sentences is a network round trip,
 * which is routinely longer than any idle grace worth setting. Timing out
 * inside that gap tore the peripheral down and rebuilt it mid-reply. The turn
 * says when it is finished; a timer should not have to guess. */
void audio_hold(bool on)
{
    s_hold = on;
    if (!on) s_last_use = esp_timer_get_time();
}

bool audio_arm(void)
{
    if (!s_ready || s_run) return s_run;
    if (!audio_up()) return false;
    /* One ES8311 serves both directions. Opening the microphone while the
       reply is coming out of it is how this crashed -- and even when it
       survives, the device hears its own answer and replies to itself. The
       interlock belongs here, next to the codec, not in whichever caller
       happens to be watching a turn-state variable. */
    if (s_spk_open) return false;
    capture_begin(true);
    if (s_run) ESP_LOGI(TAG, "hands free: mic armed, gate shut");
    return s_run;
}

/* A button press outranks the gate: the user asked to talk, so keep what the
   microphone hears from this instant whether or not it clears VAD_ON. */
void audio_open_gate(void)
{
    if (!s_run || !s_gated) return;
    if (!open_stage()) return;
    /* The distance gain came from an assumption that just stopped being true:
       a hand is on the device. Leaving 27 dB on clipped every sample and the
       transcriber returned nothing at all. */
    esp_codec_dev_set_in_gain(s_mic, MIC_GAIN_DB);
    s_hf = false;                 /* held button ends the turn, not silence */
    s_gated = false;
    ESP_LOGI(TAG, "hands free: gate forced open by the button, gain back to %.0f dB",
             (double)MIC_GAIN_DB);
}

void audio_disarm(void)
{
    if (!s_run || !s_gated) return;
    audio_stop();
}

void audio_stop(void)
{
    if (!s_ready) return;

    s_run = false;
    for (int i = 0; i < 100 && s_task != NULL; i++) vTaskDelay(pdMS_TO_TICKS(10));

    if (s_mic) esp_codec_dev_close(s_mic);

    if (s_fp != NULL) {
        /* Patch the two length fields now that the size is known. */
        uint8_t hdr[WAV_HDR];
        wav_header(hdr, (uint32_t)s_bytes);
        fseek(s_fp, 0, SEEK_SET);
        fwrite(hdr, 1, WAV_HDR, s_fp);
        fclose(s_fp);
        s_fp = NULL;
    }
    s_live = false;
    s_hf = s_gated = s_done = false;
    s_last_use = esp_timer_get_time();

    const unsigned ms = (unsigned)(s_bytes * 1000ULL /
                                   (SAMPLE_RATE * CHANNELS * (BITS / 8)));
    ESP_LOGI(TAG, "captured %u bytes (%u ms), peak %d/32767%s",
             (unsigned)s_bytes, ms, s_peak,
             s_peak < 200 ? "  <-- essentially silence" : "");
}


/* Sole consumer of the ring. Blocks when there is nothing to play, so it
   costs nothing between replies. */
static void drain_task(void *arg)
{
    static uint8_t buf[DRAIN_CHUNK];
    for (;;) {
        /* A short timeout rather than portMAX_DELAY, so an empty ring while
           the speaker is open is observable. That is exactly what a listener
           hears as the reply stopping and starting, and it was previously
           indistinguishable from a reply that had simply finished. */
        /* Poll only while a reply is actually playing. Between replies this
           blocks outright, so the underrun check costs nothing at idle -- a
           50 Hz wakeup for the life of the device is exactly the sort of
           always-on cost this firmware just spent a day removing. */
        const size_t n = xStreamBufferReceive(s_pcm, buf, sizeof buf,
                                              s_spk_open ? pdMS_TO_TICKS(20)
                                                         : portMAX_DELAY);
        if (n == 0) {
            if (s_spk_open) s_underruns++;
            continue;
        }
        if (s_spk_open) esp_codec_dev_write(s_spk, buf, (int)n);
    }
}

bool audio_speaker_open(int sample_rate)
{
    if (!audio_up()) return false;
    if (s_spk == NULL || s_spk_open) return false;

    esp_codec_dev_sample_info_t fs = {
        .bits_per_sample = 16,
        .channel = 1,
        .channel_mask = 0,
        .sample_rate = (uint32_t)sample_rate,
    };
    if (esp_codec_dev_open(s_spk, &fs) != ESP_OK) {
        ESP_LOGE(TAG, "speaker open failed at %d Hz", sample_rate);
        return false;
    }
    esp_codec_dev_set_out_vol(s_spk, SPK_VOL);
    bsp_audio_poweramp_enable(true);
    xStreamBufferReset(s_pcm);
    s_underruns = 0;
    s_spk_open = true;
    ESP_LOGI(TAG, "speaker open at %d Hz", sample_rate);
    return true;
}

int audio_speaker_write(const void *data, int len)
{
    if (!s_spk_open || len <= 0) return 0;
    /* Hands off to the ring. Blocks only when the cushion is genuinely full,
       which is still the backpressure that keeps a long reply from needing
       memory it cannot have. */
    return (int)xStreamBufferSend(s_pcm, data, (size_t)len, portMAX_DELAY);
}

void audio_speaker_close(void)
{
    if (!s_spk_open) return;
    /* Let the cushion play out; closing on the last write would clip the tail
       of every reply by up to three quarters of a second. */
    for (int i = 0; i < 400 && !xStreamBufferIsEmpty(s_pcm); i++)
        vTaskDelay(pdMS_TO_TICKS(10));
    vTaskDelay(pdMS_TO_TICKS(120));      /* and let the DMA itself finish */
    bsp_audio_poweramp_enable(false);
    esp_codec_dev_close(s_spk);
    s_spk_open = false;
    s_last_use = esp_timer_get_time();
    if (s_underruns > 0)
        ESP_LOGW(TAG, "playback ran dry %d time(s) -- the reply stuttered",
                 s_underruns);
}
