#include "voice.h"

#include <stdio.h>
#include <string.h>
#include <sys/stat.h>

#include "audio.h"
#include "esp_crt_bundle.h"
#include "esp_http_client.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "esp_heap_caps.h"
#include "net.h"

static const char *TAG = "voice";

#define BOUNDARY   "----magesp7f3a91"
#define MIN_PCM    8000            /* half a second; below this it is a stray tap */
#define RESP_CAP   4096

/* The media routes are registered inside a web::scope("/api/magician/v2"), so
   the path in magician.rs is only the tail. Taking it at face value gave a
   404 that looked like a missing feature rather than a wrong URL. /health is
   genuinely at the root, which is what made the mistake plausible. */
#define API_PREFIX "/api/magician/v2"
#define PLAY_CHUNK 512
#define TTS_HDR_SCAN 5120

static turn_state_t s_state = TURN_IDLE;
static char s_transcript[320];
static char s_reply[1536];
static char s_error[160];
static char s_resp[RESP_CAP];
static int  s_resp_len;

/* One worker created at boot, parked on a semaphore.
 *
 * Creating a task per turn asked for an 8 KB stack at the single worst moment
 * -- after wifi, TLS, LVGL and the audio path are all resident -- and it
 * failed. Allocating once at boot, when memory is plentiful, removes the
 * failure mode rather than making it rarer. */
static SemaphoreHandle_t s_kick;

turn_state_t voice_state(void)      { return s_state; }
const char  *voice_transcript(void) { return s_transcript; }
const char  *voice_reply(void)      { return s_reply; }
const char  *voice_error(void)      { return s_error; }

static void speak(const char *text);
static bool json_str(const char *body, const char *key, char *out, size_t cap);

/* Walk the `assistant_speech_segments` array, handing back one segment's text
   per call.
 *
 * The reply is synthesised a segment at a time rather than as one string. That
 * is what makes the answer length unbounded: no buffer here has to hold the
 * whole reply, only the sentence being spoken. Magician already segments it
 * for exactly this, and speaking starts on the first sentence instead of
 * after the last.
 *
 * `cursor` is NULL to start; the return value is the next cursor, or NULL when
 * the array is exhausted. */
static const char *next_segment(const char *body, const char *cursor,
                                char *out, size_t cap)
{
    const char *p = cursor;
    if (p == NULL) {
        p = strstr(body, "\"assistant_speech_segments\"");
        if (p == NULL) return NULL;
        p = strchr(p, '[');
        if (p == NULL) return NULL;
        p++;
    }

    /* Find this segment's text, but stop at the array's own closing bracket
       so a later field with a "text" key cannot be mistaken for a segment. */
    int depth = 1;
    bool in_str = false;
    const char *scan = p, *found = NULL;
    for (; *scan; scan++) {
        if (in_str) {
            if (*scan == '\\' && scan[1]) { scan++; continue; }
            if (*scan == '"') in_str = false;
            continue;
        }
        if (*scan == '"') {
            if (found == NULL && strncmp(scan, "\"text\"", 6) == 0) found = scan;
            in_str = true;
            continue;
        }
        if (*scan == '[' || *scan == '{') depth++;
        else if (*scan == ']' || *scan == '}') {
            depth--;
            if (depth == 0) break;                 /* end of the array */
        }
    }
    if (found == NULL) return NULL;

    if (!json_str(found, "text", out, cap)) return NULL;
    /* Resume after this segment's object so the next call finds the next one. */
    const char *next = strchr(found, '}');
    return next ? next + 1 : NULL;
}

/* esp_http_client_write may write fewer bytes than asked -- a short write is
   normal over TLS, not an error. Treating one as failure is what produced
   "upload cut short" on a perfectly good connection. */
static bool write_all(esp_http_client_handle_t cl, const void *data, int len)
{
    const char *p = (const char *)data;
    int done = 0;
    while (done < len) {
        const int n = esp_http_client_write(cl, p + done, len - done);
        if (n < 0) {
            ESP_LOGE(TAG, "write failed after %d of %d bytes", done, len);
            return false;
        }
        if (n == 0) {
            ESP_LOGE(TAG, "write stalled at %d of %d bytes", done, len);
            return false;
        }
        done += n;
    }
    return true;
}

static void fail(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(s_error, sizeof s_error, fmt, ap);
    va_end(ap);
    s_state = TURN_ERROR;
    ESP_LOGE(TAG, "%s", s_error);
}

/* Pull one JSON string value out of the response.
 *
 * A full parser would cost more heap than the two fields needed here, and the
 * response shape is fixed and known. Handles the escapes that actually appear
 * in transcribed speech; anything else is copied through. */
static bool json_str(const char *body, const char *key, char *out, size_t cap)
{
    char pat[48];
    snprintf(pat, sizeof pat, "\"%s\"", key);
    const char *p = strstr(body, pat);
    if (p == NULL) return false;
    p = strchr(p + strlen(pat), ':');
    if (p == NULL) return false;
    p++;
    while (*p == ' ' || *p == '\t' || *p == '\n') p++;
    if (*p != '"') return false;
    p++;

    size_t n = 0;
    while (*p && *p != '"' && n + 1 < cap) {
        if (*p == '\\' && p[1]) {
            p++;
            switch (*p) {
            case 'n': out[n++] = '\n'; break;
            case 't': out[n++] = ' ';  break;
            case 'r': break;
            case 'u': /* skip the code point rather than mangle it */
                if (p[1] && p[2] && p[3] && p[4]) p += 4;
                break;
            default:  out[n++] = *p; break;
            }
            p++;
        } else {
            out[n++] = *p++;
        }
    }
    out[n] = 0;
    return true;
}

static void run_turn(void)
{
    const char *path = audio_path();
    struct stat st;
    if (stat(path, &st) != 0) {
        fail("no recording on disk");
        return;
    }
    const size_t wav_len = (size_t)st.st_size;

    char url[192];
    snprintf(url, sizeof url, "%s" API_PREFIX "/media/voice-notes", net_base_url());

    /* Multipart assembled by hand so the exact byte count is known up front:
       the staged file is what makes a real Content-Length possible, and a
       chunked body would give the server no length at all. */
    char pre[640];
    const int pre_len = snprintf(pre, sizeof pre,
        "--" BOUNDARY "\r\n"
        "Content-Disposition: form-data; name=\"source_surface\"\r\n\r\nesp_terminal\r\n"
        "--" BOUNDARY "\r\n"
        "Content-Disposition: form-data; name=\"thread_id\"\r\n\r\n%s\r\n"
        "--" BOUNDARY "\r\n"
        "Content-Disposition: form-data; name=\"file\"; filename=\"utterance.wav\"\r\n"
        "Content-Type: audio/wav\r\n\r\n",
        net_thread());
    const char *post = "\r\n--" BOUNDARY "--\r\n";
    const int post_len = (int)strlen(post);

    esp_http_client_config_t cfg = {
        .url = url,
        .method = HTTP_METHOD_POST,
        .timeout_ms = 45000,
        .crt_bundle_attach = esp_crt_bundle_attach,
    };
    esp_http_client_handle_t cl = esp_http_client_init(&cfg);
    if (cl == NULL) { fail("http client init failed"); vTaskDelete(NULL); return; }

    esp_http_client_set_header(cl, "Content-Type", "multipart/form-data; boundary=" BOUNDARY);
    if (net_has_cf_token()) {
        esp_http_client_set_header(cl, "CF-Access-Client-Id", net_cf_id());
        esp_http_client_set_header(cl, "CF-Access-Client-Secret", net_cf_secret());
    }
    if (net_has_device_token()) {
        char bearer[112];
        snprintf(bearer, sizeof bearer, "Bearer %s", net_device_token());
        esp_http_client_set_header(cl, "X-Magician-Device-Id", net_device_id());
        esp_http_client_set_header(cl, "Authorization", bearer);
    }

    const int total = pre_len + (int)wav_len + post_len;
    ESP_LOGI(TAG, "POST %s  (%d bytes, %u of audio)", url, total, (unsigned)wav_len);

    if (esp_http_client_open(cl, total) != ESP_OK) {
        fail("cannot reach %s", net_base_url());
        esp_http_client_cleanup(cl);
        return;
    }

    size_t sent = 0;
    bool ok = write_all(cl, pre, pre_len);
    if (ok) sent += pre_len;
    FILE *f = ok ? fopen(path, "rb") : NULL;
    if (f != NULL) {
        char buf[1024];
        size_t n;
        while (ok && (n = fread(buf, 1, sizeof buf, f)) > 0) {
            ok = write_all(cl, buf, (int)n);
            if (ok) sent += n;
        }
        fclose(f);
    } else {
        ok = false;
    }
    if (ok) ok = write_all(cl, post, post_len);
    if (!ok) {
        fail("upload cut short at %u of %d bytes", (unsigned)sent, total);
        esp_http_client_cleanup(cl);
        return;
    }

    s_state = TURN_THINKING;

    const int clen = esp_http_client_fetch_headers(cl);
    const int status = esp_http_client_get_status_code(cl);
    (void)clen;

    s_resp_len = esp_http_client_read_response(cl, s_resp, sizeof s_resp - 1);
    if (s_resp_len < 0) s_resp_len = 0;
    s_resp[s_resp_len] = 0;
    esp_http_client_cleanup(cl);
    if (s_resp_len >= (int)sizeof s_resp - 1)
        ESP_LOGW(TAG, "response filled the %u-byte buffer; later segments may be lost",
                 (unsigned)sizeof s_resp);

    if (status < 200 || status >= 300) {
        char why[96] = "";
        json_str(s_resp, "error", why, sizeof why);
        /* The one failure that is not a fault. Hands-free endpoints on energy,
           so a door or a cough will occasionally reach the transcriber; the
           machine name for it belongs in the log, not on the face. */
        if (strcmp(why, "empty_transcript") == 0) fail("didn't catch that");
        else fail("HTTP %d%s%s", status, why[0] ? ": " : "", why);
        return;
    }

    if (!json_str(s_resp, "transcript", s_transcript, sizeof s_transcript))
        s_transcript[0] = 0;
    if (!json_str(s_resp, "assistant_preview", s_reply, sizeof s_reply))
        s_reply[0] = 0;
    /* assistant_preview is truncated to 600 chars SERVER-SIDE by design; the
       full answer only exists in assistant_speech_segments. Report which of
       the two actually arrived. */
    ESP_LOGI(TAG, "response %d bytes, preview %u chars, segments key %s",
             s_resp_len, (unsigned)strlen(s_reply),
             strstr(s_resp, "assistant_speech_segments") ? "PRESENT" : "ABSENT");

    if (s_transcript[0] == 0) {
        /* Nothing was heard. Saying so beats replying to silence. */
        fail("nothing was transcribed");
        return;
    }

    ESP_LOGI(TAG, "heard: \"%s\"", s_transcript);
    ESP_LOGI(TAG, "reply: \"%s\"", s_reply[0] ? s_reply : "(none)");

    /* Screen first, then speech: the words are already useful, and a TTS
       failure must not cost the reply.
       SPEAKING, not DONE. Claiming the turn was finished before a word had
       been said let hands free re-open the microphone into the reply. */
    s_state = TURN_SPEAKING;

    static char seg[1024];
    const char *cur = NULL;
    int spoken = 0;
    size_t shown = 0;

    /* Own the audio hardware for the whole reply, not one sentence at a time.
       Each sentence is its own TTS request, and the round trip between them is
       long enough to trip any idle release. */
    audio_hold(true);

    while ((cur = next_segment(s_resp, cur, seg, sizeof seg)) != NULL) {
        if (seg[0] == 0) continue;
        /* Grow the on-screen answer as each sentence is spoken. Truncating
           HERE is honest -- it is a screen, and the label scrolls -- whereas
           truncating what gets synthesised would silence part of the reply. */
        if (spoken == 0) { s_reply[0] = 0; shown = 0; }
        const size_t need = strlen(seg);
        if (shown + need + 2 < sizeof s_reply) {
            if (shown) { s_reply[shown++] = ' '; s_reply[shown] = 0; }
            memcpy(s_reply + shown, seg, need + 1);
            shown += need;
        }
        speak(seg);
        spoken++;
    }
    if (spoken == 0) {
        /* No segments came back -- say the 600-char preview rather than
           nothing, and say so, because that IS the whole answer available. */
        ESP_LOGW(TAG, "no speech segments; speaking the 600-char preview only");
        speak(s_reply);
    } else {
        ESP_LOGI(TAG, "spoke %d segment(s), %u chars on screen",
                 spoken, (unsigned)shown);
    }
    audio_hold(false);
    s_state = TURN_DONE;
}

/* Escape a reply for a JSON string. Speech comes back with quotes and
   newlines in it often enough that skipping this produces a 400 that looks
   like a server fault. */
static int json_escape(const char *in, char *out, int cap)
{
    int n = 0;
    for (const char *p = in; *p && n + 8 < cap; p++) {
        unsigned char c = (unsigned char)*p;
        if (c == '"' || c == '\\')      { out[n++] = '\\'; out[n++] = (char)c; }
        else if (c == '\n')             { out[n++] = '\\'; out[n++] = 'n'; }
        else if (c == '\r' || c == '\t') { out[n++] = ' '; }
        else if (c < 0x20)              { /* drop control bytes */ }
        else                            { out[n++] = (char)c; }
    }
    out[n] = 0;
    return n;
}

/* Ask for the reply as speech and play it as it arrives.
 *
 * Request WAV so the existing streaming decoder can read the sample rate and
 * encoding from the response. Raw PCM has no header and cannot be fed to this
 * decoder. The codec write blocks, so HTTP reads follow playback without
 * buffering the complete reply on this part with no PSRAM. */
static void speak(const char *text)
{
    if (text == NULL || text[0] == 0) return;

    static char body[1700];
    static char esc[1400];
    json_escape(text, esc, sizeof esc);
    const int body_len = snprintf(body, sizeof body,
                                  "{\"text\":\"%s\",\"format\":\"wav\"}", esc);

    char url[192];
    snprintf(url, sizeof url, "%s" API_PREFIX "/media/tts/synthesize", net_base_url());

    esp_http_client_config_t cfg = {
        .url = url, .method = HTTP_METHOD_POST, .timeout_ms = 45000,
        .crt_bundle_attach = esp_crt_bundle_attach,
    };
    esp_http_client_handle_t cl = esp_http_client_init(&cfg);
    if (cl == NULL) return;

    esp_http_client_set_header(cl, "Content-Type", "application/json");
    if (net_has_cf_token()) {
        esp_http_client_set_header(cl, "CF-Access-Client-Id", net_cf_id());
        esp_http_client_set_header(cl, "CF-Access-Client-Secret", net_cf_secret());
    }
    if (net_has_device_token()) {
        char bearer[112];
        snprintf(bearer, sizeof bearer, "Bearer %s", net_device_token());
        esp_http_client_set_header(cl, "X-Magician-Device-Id", net_device_id());
        esp_http_client_set_header(cl, "Authorization", bearer);
    }

    if (esp_http_client_open(cl, body_len) != ESP_OK) {
        ESP_LOGW(TAG, "tts: cannot open");
        esp_http_client_cleanup(cl);
        return;
    }
    if (!write_all(cl, body, body_len)) {
        ESP_LOGW(TAG, "tts: request write failed");
        esp_http_client_cleanup(cl);
        return;
    }
    esp_http_client_fetch_headers(cl);
    const int status = esp_http_client_get_status_code(cl);
    if (status < 200 || status >= 300) {
        ESP_LOGW(TAG, "tts: HTTP %d -- reply stays on screen only", status);
        esp_http_client_cleanup(cl);
        return;
    }

    /* Providers can return PCM16 WAV or float32 WAV with non-canonical JUNK
       and FLLR padding. Read the actual header and rate, then convert float32
       to the int16 the codec wants. Requesting WAV explicitly also works with
       Linux providers that honor the format, unlike the former PCM request
       whose headerless response this decoder necessarily rejected. */
    /* STATIC, not on the stack. As a local this was an 8 KB array inside an
       8 KB task stack, so parsing the reply's header overflowed it and the
       device rebooted the moment speech began. Only one turn runs at a time,
       so a single shared buffer is correct as well as cheaper. */
    static uint8_t hdr[TTS_HDR_SCAN];
    int have = 0, data_off = -1, rate = 0, chans = 1, bits = 16, wfmt = 1;

    while (have < (int)sizeof hdr && data_off < 0) {
        const int n = esp_http_client_read(cl, (char *)hdr + have, (int)sizeof hdr - have);
        if (n < 0) break;
        if (n == 0) {
            if (esp_http_client_is_complete_data_received(cl)) break;
            vTaskDelay(pdMS_TO_TICKS(5));
            continue;
        }
        have += n;

        if (have >= 12 && memcmp(hdr, "RIFF", 4) == 0 && memcmp(hdr + 8, "WAVE", 4) == 0) {
            int off = 12;
            while (off + 8 <= have) {
                const uint32_t sz = (uint32_t)hdr[off+4] | ((uint32_t)hdr[off+5] << 8) |
                                    ((uint32_t)hdr[off+6] << 16) | ((uint32_t)hdr[off+7] << 24);
                if (memcmp(hdr + off, "fmt ", 4) == 0 && off + 24 <= have) {
                    wfmt  = hdr[off+8]  | (hdr[off+9]  << 8);
                    chans = hdr[off+10] | (hdr[off+11] << 8);
                    rate  = (int)((uint32_t)hdr[off+12] | ((uint32_t)hdr[off+13] << 8) |
                                  ((uint32_t)hdr[off+14] << 16) | ((uint32_t)hdr[off+15] << 24));
                    bits  = hdr[off+22] | (hdr[off+23] << 8);
                } else if (memcmp(hdr + off, "data", 4) == 0) {
                    data_off = off + 8;
                    break;
                }
                off += 8 + (int)sz + ((int)sz & 1);
            }
        }
    }

    if (data_off < 0 || rate <= 0) {
        ESP_LOGW(TAG, "tts: could not parse the audio header");
        esp_http_client_cleanup(cl);
        return;
    }
    ESP_LOGI(TAG, "tts: %d Hz, %d ch, %d-bit, wav-format %d, payload at %d",
             rate, chans, bits, wfmt, data_off);

    if (!audio_speaker_open(rate)) {
        ESP_LOGW(TAG, "tts: no speaker");
        esp_http_client_cleanup(cl);
        return;
    }

    s_state = TURN_SPEAKING;

    /* Convert into int16 in place-ish, a chunk at a time. Nothing larger than
       these two buffers is ever held. */
    static int16_t out[PLAY_CHUNK];
    uint8_t carry[8];
    int carry_n = 0, total = 0;
    const int in_bytes = (bits == 32) ? 4 : 2;

    int avail = have - data_off;
    uint8_t *src = hdr + data_off;

    for (;;) {
        if (avail > 0) {
            int used = 0, o = 0;
            while (o < PLAY_CHUNK) {
                uint8_t samp[4];
                int got = 0;
                /* FIFO. This popped the carry in reverse order, so a sample
                   split across two reads came back with its bytes swapped --
                   an audible click at every buffer boundary rather than the
                   network jitter it looked like. */
                for (int k = 0; k < carry_n && got < in_bytes; k++) samp[got++] = carry[k];
                carry_n = 0;
                while (got < in_bytes && used < avail) samp[got++] = src[used++];
                if (got < in_bytes) {                 /* keep the partial sample */
                    for (int k = 0; k < got; k++) carry[k] = samp[k];
                    carry_n = got;
                    break;
                }
                int32_t v;
                if (bits == 32 && wfmt == 3) {
                    float f;
                    memcpy(&f, samp, 4);
                    if (f > 1.0f) f = 1.0f;
                    if (f < -1.0f) f = -1.0f;
                    v = (int32_t)(f * 32767.0f);
                } else if (bits == 32) {
                    v = (int32_t)(((uint32_t)samp[2]) | ((uint32_t)samp[3] << 8));
                    v = (int16_t)v;
                } else {
                    v = (int16_t)((uint16_t)samp[0] | ((uint16_t)samp[1] << 8));
                }
                out[o++] = (int16_t)v;
                if (chans == 2) {                     /* drop the right channel */
                    int skip = 0;
                    while (skip < in_bytes && used < avail) { used++; skip++; }
                }
            }
            if (o > 0) {
                if (audio_speaker_write(out, o * 2) < 0) break;
                total += o * 2;
            }
            avail -= used;
            src   += used;
            if (avail > 0) continue;
        }

        const int n = esp_http_client_read(cl, (char *)hdr, (int)sizeof hdr);
        if (n < 0) {
            ESP_LOGW(TAG, "tts: read error after %d bytes played", total);
            break;
        }
        if (n == 0) {
            /* Zero means "nothing right now", not "finished" -- only the
               completion flag means finished. Treating it as EOF cut long
               replies off part-way while short ones sounded fine. */
            if (esp_http_client_is_complete_data_received(cl)) {
                ESP_LOGI(TAG, "tts: body complete");
                break;
            }
            vTaskDelay(pdMS_TO_TICKS(5));
            continue;
        }
        src = hdr;
        avail = n;
    }

    audio_speaker_close();
    esp_http_client_cleanup(cl);
    ESP_LOGI(TAG, "spoke %d bytes (%d ms) of a %u-char reply",
             total, total * 1000 / (rate * 2), (unsigned)strlen(text));
}

static void turn_worker(void *arg)
{
    for (;;) {
        xSemaphoreTake(s_kick, portMAX_DELAY);
        net_set_busy(true);
        ESP_LOGI(TAG, "turn start; free heap %u",
                 (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL));
        run_turn();
        net_set_busy(false);
        ESP_LOGI(TAG, "turn done; worker stack headroom %u bytes, free heap %u",
                 (unsigned)(uxTaskGetStackHighWaterMark(NULL)),
                 (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL));
    }
}

void voice_init(void)
{
    s_kick = xSemaphoreCreateBinary();
    if (s_kick == NULL || xTaskCreate(turn_worker, "turn", 6144, NULL, 4, NULL) != pdPASS) {
        ESP_LOGE(TAG, "could not create the turn worker (free heap %u)",
                 (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL));
        s_kick = NULL;
        return;
    }
    ESP_LOGI(TAG, "turn worker ready (free heap %u, largest block %u)",
             (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL),
             (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL));
}

/* Announce recording lifecycle to Magician.
 *
 * The whitelist server-side is recording started / stopped / failed; anything
 * else is refused, which is the right shape -- a device should not be able to
 * post arbitrary events. Failures here are logged and dropped: telemetry that
 * can break a turn is worse than no telemetry. */
void voice_note_event(const char *event_type, const char *detail)
{
    if (!net_configured() || !net_has_device_token()) return;

    char url[192];
    snprintf(url, sizeof url, "%s" API_PREFIX "/media/voice-notes/events", net_base_url());

    char payload[320];
    const int len = snprintf(payload, sizeof payload,
        "{\"event_type\":\"%s\","
        "\"payload\":{\"source_surface\":\"esp_terminal\",\"detail\":\"%s\"}}",
        event_type, detail ? detail : "");

    esp_http_client_config_t cfg = {
        .url = url, .method = HTTP_METHOD_POST, .timeout_ms = 8000,
        .crt_bundle_attach = esp_crt_bundle_attach,
    };
    esp_http_client_handle_t cl = esp_http_client_init(&cfg);
    if (cl == NULL) return;
    esp_http_client_set_header(cl, "Content-Type", "application/json");
    if (net_has_cf_token()) {
        esp_http_client_set_header(cl, "CF-Access-Client-Id", net_cf_id());
        esp_http_client_set_header(cl, "CF-Access-Client-Secret", net_cf_secret());
    }
    if (net_has_device_token()) {
        char bearer[112];
        snprintf(bearer, sizeof bearer, "Bearer %s", net_device_token());
        esp_http_client_set_header(cl, "X-Magician-Device-Id", net_device_id());
        esp_http_client_set_header(cl, "Authorization", bearer);
    }
    esp_http_client_set_post_field(cl, payload, len);
    if (esp_http_client_perform(cl) == ESP_OK)
        ESP_LOGI(TAG, "event %s -> HTTP %d", event_type, esp_http_client_get_status_code(cl));
    esp_http_client_cleanup(cl);
}

void voice_submit(void)
{
    if (s_state == TURN_SENDING || s_state == TURN_THINKING) return;

    s_transcript[0] = s_reply[0] = s_error[0] = 0;

    if (!net_configured()) { fail("magician url not set"); return; }
    if (!net_has_device_token()) { fail("device is not paired"); return; }
    if (audio_bytes() < MIN_PCM) {
        voice_note_event("media.voice_note.recording_failed", "too short");
        fail("too short (%u ms)",
             (unsigned)(audio_bytes() * 1000ULL / 32000ULL));
        return;
    }
    if (audio_peak() < 200) {
        voice_note_event("media.voice_note.recording_failed", "silence");
        /* The upload would succeed and transcribe to nothing, which reads as
           a server problem. Name the real cause here instead. */
        fail("microphone heard nothing");
        return;
    }

    if (s_kick == NULL) { fail("upload worker unavailable"); return; }
    voice_note_event("media.voice_note.recording_stopped", "submitting");
    s_state = TURN_SENDING;
    xSemaphoreGive(s_kick);
}
